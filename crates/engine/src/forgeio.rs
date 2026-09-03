//! The forge adapters: `gh` for GitHub, `glab` for GitLab (ADR 0029,
//! `spec/forge.md`).
//!
//! A forge is a tool on the path. Each tool is logged in by its own login
//! flow, knows the remote's host and project from the working directory, and
//! prints JSON for any endpoint — so this module holds no token, no hostname
//! and no HTTP client. It runs the tool through the same runner the model
//! backend uses, and maps the JSON it gets back onto `engine::forge`'s types.
//!
//! Every mapping is a pure function of a `serde_json::Value`, tested against
//! recorded answers, so the shape of what a forge says is pinned here and a
//! change to it fails a test rather than a review.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::{Value, json};

use crate::forge::{
    Batch, Forge, ForgeError, ForgeKind, NewComment, Published, RemoteComment, RemoteThread,
    Request,
};
use crate::subprocess;

/// One command-line tool, run from the repository root with a deadline.
///
/// The root, because both tools resolve the remote from the directory they
/// run in, exactly as `git` does.
struct Tool {
    program: &'static str,
    working_dir: PathBuf,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
}

impl Tool {
    fn new(program: &'static str, root: &Path) -> Self {
        Tool {
            program,
            working_dir: root.to_path_buf(),
            timeout: Duration::from_secs(60),
            cancel: None,
        }
    }

    /// Run the tool with these arguments and return its stdout.
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, ForgeError> {
        let argv: Vec<String> = std::iter::once(self.program)
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        let command = || subprocess::describe(&argv);
        let out = subprocess::run(&subprocess::Run {
            argv: &argv,
            stdin,
            working_dir: Some(&self.working_dir),
            timeout: self.timeout,
            cancel: self.cancel.as_ref(),
        })
        .map_err(|f| match f {
            subprocess::Failure::Spawn(source) => ForgeError::Spawn {
                command: command(),
                source,
            },
            subprocess::Failure::Io(source) => ForgeError::Io {
                command: command(),
                source,
            },
            subprocess::Failure::Timeout => ForgeError::Timeout {
                command: command(),
                timeout: self.timeout,
            },
            subprocess::Failure::Cancelled => ForgeError::Cancelled { command: command() },
        })?;
        if !out.status.success() {
            return Err(ForgeError::Failed {
                command: command(),
                code: out.status.code(),
                stderr: subprocess::stderr_excerpt(&out.stderr, 600),
            });
        }
        Ok(out.stdout)
    }

    fn json(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Value, ForgeError> {
        let bytes = self.run(args, stdin)?;
        serde_json::from_slice(&bytes).map_err(|e| self.parse_err(args, e.to_string()))
    }

    /// Every JSON document on stdout, in order. A paginated call prints one
    /// document per page, back to back.
    fn json_stream(&self, args: &[&str]) -> Result<Vec<Value>, ForgeError> {
        let bytes = self.run(args, None)?;
        serde_json::Deserializer::from_slice(&bytes)
            .into_iter::<Value>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| self.parse_err(args, e.to_string()))
    }

    fn parse_err(&self, args: &[&str], msg: String) -> ForgeError {
        ForgeError::Parse {
            command: format!("{} {}", self.program, args.join(" ")),
            msg,
        }
    }
}

/// Without a number the question was "which request is this branch", and
/// "none" is an answer rather than a broken tool.
fn no_request(err: ForgeError, id: Option<&str>, noun: &str) -> ForgeError {
    match err {
        ForgeError::Failed { stderr, .. } if id.is_none() => {
            ForgeError::NoRequest(format!("the current branch has no {noun} ({stderr})"))
        }
        e => e,
    }
}

fn parse_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Parse {
        command: "forge".into(),
        msg: msg.into(),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> Result<&'a str, ForgeError> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| parse_err(format!("missing {key}")))
}

fn u32_at(v: &Value, pointer: &str) -> Option<u32> {
    v.pointer(pointer).and_then(Value::as_u64).map(|n| n as u32)
}

// ===================================================================== GitHub

/// GitHub, through `gh`.
pub struct GhForge {
    tool: Tool,
}

/// One page of review threads. GitHub caps a page at 100 and a request can
/// carry more; the caller walks `pageInfo`.
const THREADS_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path diffSide line startLine
          comments(first: 100) {
            nodes {
              databaseId body createdAt diffHunk
              author { login }
              replyTo { databaseId }
            }
          }
        }
      }
    }
  }
}"#;

const RESOLVE_MUTATION: &str =
    "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) { thread { id } } }";
const UNRESOLVE_MUTATION: &str =
    "mutation($id: ID!) { unresolveReviewThread(input: {threadId: $id}) { thread { id } } }";

impl GhForge {
    pub fn new(root: &Path) -> Self {
        GhForge {
            tool: Tool::new("gh", root),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.tool.timeout = timeout;
        self
    }

    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.tool.cancel = Some(flag);
        self
    }

    fn graphql(&self, query: &str, vars: &[(&str, Value)]) -> Result<Value, ForgeError> {
        let body = json!({
            "query": query,
            "variables": vars
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<serde_json::Map<String, Value>>(),
        });
        let v = self.tool.json(
            &["api", "graphql", "--input", "-"],
            Some(body.to_string().as_bytes()),
        )?;
        if let Some(errors) = v.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            let msg = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ForgeError::Parse {
                command: "gh api graphql".into(),
                msg,
            });
        }
        Ok(v)
    }

    fn rest(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ForgeError> {
        let mut args = vec!["api", "--method", method, path];
        let text;
        let stdin = match body {
            Some(b) => {
                args.extend(["--input", "-"]);
                text = b.to_string();
                Some(text.as_bytes())
            }
            None => None,
        };
        self.tool.json(&args, stdin)
    }

    fn pulls(req: &Request, tail: &str) -> String {
        format!("repos/{}/pulls/{}{}", req.project, req.id, tail)
    }
}

impl Forge for GhForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Github
    }

    fn request(&self, id: Option<&str>) -> Result<Request, ForgeError> {
        let mut args = vec!["pr", "view"];
        if let Some(id) = id {
            args.push(id);
        }
        args.extend(["--json", "number,baseRefName,baseRefOid,headRefOid,url"]);
        let v = self
            .tool
            .json(&args, None)
            .map_err(|e| no_request(e, id, "pull request"))?;
        parse_request(&v)
    }

    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError> {
        let (owner, name) = req
            .project
            .split_once('/')
            .ok_or_else(|| parse_err(format!("project {:?} is not owner/repo", req.project)))?;
        let number: i64 = req
            .id
            .parse()
            .map_err(|_| parse_err(format!("pull request number {:?} is not a number", req.id)))?;
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let v = self.graphql(
                THREADS_QUERY,
                &[
                    ("owner", json!(owner)),
                    ("name", json!(name)),
                    ("number", json!(number)),
                    ("after", after.as_deref().map_or(Value::Null, |s| json!(s))),
                ],
            )?;
            let (page, next) = parse_threads_page(&v)?;
            all.extend(page);
            match next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        Ok(all)
    }

    fn publish(&self, req: &Request, batch: &Batch) -> Result<Vec<Published>, ForgeError> {
        let mut out = Vec::new();

        // New comments: one review, so the author gets one notification.
        if !batch.comments.is_empty() {
            let review = self.rest(
                "POST",
                &Self::pulls(req, "/reviews"),
                Some(&review_body(req, &batch.comments)),
            )?;
            let review_id = review
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| parse_err("the review came back without an id"))?;
            let posted = self.rest(
                "GET",
                &Self::pulls(req, &format!("/reviews/{review_id}/comments")),
                None,
            )?;
            out.extend(match_published(&batch.comments, &posted));
        }

        // Replies thread under the root comment, one call each.
        for r in &batch.replies {
            let v = self.rest(
                "POST",
                &Self::pulls(req, &format!("/comments/{}/replies", r.root_comment)),
                Some(&json!({ "body": r.body })),
            )?;
            out.push(Published {
                finding: r.finding.clone(),
                thread: r.thread.clone(),
                comment: v
                    .get("id")
                    .and_then(Value::as_i64)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                url: v
                    .get("html_url")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }

        // A new comment's thread id is GraphQL's, which REST never says. One
        // fetch of the threads names every root, and the renderer needs the
        // fresh threads anyway.
        if !batch.comments.is_empty() {
            let threads = self.threads(req)?;
            for p in out.iter_mut().filter(|p| p.thread.is_empty()) {
                if let Some(t) = threads
                    .iter()
                    .find(|t| t.root().is_some_and(|c| c.id == p.comment))
                {
                    p.thread = t.id.clone();
                }
            }
        }
        Ok(out)
    }

    fn set_resolved(&self, _req: &Request, thread: &str, resolved: bool) -> Result<(), ForgeError> {
        let mutation = if resolved {
            RESOLVE_MUTATION
        } else {
            UNRESOLVE_MUTATION
        };
        self.graphql(mutation, &[("id", json!(thread))])?;
        Ok(())
    }
}

/// `gh pr view --json number,baseRefName,baseRefOid,headRefOid,url`.
///
/// The project comes from the URL: the request lives in the base repository,
/// and `gh pr view` names the head repository only.
pub fn parse_request(v: &Value) -> Result<Request, ForgeError> {
    let url = str_of(v, "url")?;
    let mut segments = url.trim_end_matches('/').rsplit('/');
    let _number = segments.next();
    let _pull = segments.next();
    let repo = segments
        .next()
        .ok_or_else(|| parse_err("url has no repo"))?;
    let owner = segments
        .next()
        .ok_or_else(|| parse_err("url has no owner"))?;
    let number = v
        .get("number")
        .and_then(Value::as_i64)
        .ok_or_else(|| parse_err("missing number"))?;
    Ok(Request {
        kind: ForgeKind::Github,
        project: format!("{owner}/{repo}"),
        id: number.to_string(),
        base_ref: str_of(v, "baseRefName")?.to_string(),
        base_tip: str_of(v, "baseRefOid")?.to_string(),
        head: str_of(v, "headRefOid")?.to_string(),
        merge_base: None,
        url: url.to_string(),
    })
}

/// One `reviewThreads` page: the threads, and the cursor of the next page.
pub fn parse_threads_page(v: &Value) -> Result<(Vec<RemoteThread>, Option<String>), ForgeError> {
    let conn = v
        .pointer("/data/repository/pullRequest/reviewThreads")
        .ok_or_else(|| parse_err("no reviewThreads in the answer"))?;
    let next = conn
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| conn.pointer("/pageInfo/endCursor").and_then(Value::as_str))
        .flatten()
        .map(str::to_string);
    let nodes = conn
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| parse_err("reviewThreads has no nodes"))?;
    let threads = nodes
        .iter()
        .map(parse_gh_thread)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((threads, next))
}

fn parse_gh_thread(t: &Value) -> Result<RemoteThread, ForgeError> {
    let comments = t
        .pointer("/comments/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .map(parse_gh_comment)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    // The text of the last line, from the root's diff hunk: the content key
    // for a thread whose line has left the diff.
    let line_text = t
        .pointer("/comments/nodes/0/diffHunk")
        .and_then(Value::as_str)
        .and_then(last_diff_line);
    let side = match t.get("diffSide").and_then(Value::as_str) {
        Some("LEFT") => "old",
        _ => "new",
    };
    Ok(RemoteThread {
        id: str_of(t, "id")?.to_string(),
        resolved: t
            .get("isResolved")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        outdated: t
            .get("isOutdated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        path: str_of(t, "path")?.to_string(),
        side: side.to_string(),
        line: u32_at(t, "/line"),
        start_line: u32_at(t, "/startLine"),
        line_text,
        anchor: None,
        comments,
    })
}

fn parse_gh_comment(c: &Value) -> Result<RemoteComment, ForgeError> {
    Ok(RemoteComment {
        id: c
            .get("databaseId")
            .and_then(Value::as_i64)
            .ok_or_else(|| parse_err("comment without databaseId"))?
            .to_string(),
        author: c
            .pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or("(deleted)")
            .to_string(),
        created: str_of(c, "createdAt")?.to_string(),
        body: str_of(c, "body")?.to_string(),
        reply_to: c
            .pointer("/replyTo/databaseId")
            .and_then(Value::as_i64)
            .map(|n| n.to_string()),
    })
}

/// The content of the last line of a diff hunk, without its `+`/`-`/space.
fn last_diff_line(hunk: &str) -> Option<String> {
    let last = hunk
        .lines()
        .rev()
        .find(|l| !l.is_empty() && !l.starts_with("@@"))?;
    Some(last.get(1..).unwrap_or("").to_string())
}

/// The body of `POST /pulls/{n}/reviews`: a pending review submitted at once
/// as a plain comment (a verdict is later work), against the head this review
/// was opened on.
pub fn review_body(req: &Request, comments: &[NewComment]) -> Value {
    let side = |s: &str| if s == "old" { "LEFT" } else { "RIGHT" };
    let items: Vec<Value> = comments
        .iter()
        .map(|c| {
            let mut item = json!({
                "path": c.path,
                "body": c.body,
                "line": c.line,
                "side": side(&c.side),
            });
            if let Some(start) = c.start_line {
                item["start_line"] = json!(start);
                item["start_side"] = json!(side(&c.side));
            }
            item
        })
        .collect();
    json!({
        "commit_id": req.head,
        "event": "COMMENT",
        "body": "",
        "comments": items,
    })
}

/// Pair each sent comment with the record GitHub made of it, by path, line
/// and body. The review's answer has no ids for its comments; the list of
/// the review's comments does.
pub fn match_published(sent: &[NewComment], posted: &Value) -> Vec<Published> {
    let Some(posted) = posted.as_array() else {
        return Vec::new();
    };
    sent.iter()
        .filter_map(|c| {
            let hit = posted.iter().find(|p| {
                p.get("path").and_then(Value::as_str) == Some(c.path.as_str())
                    && p.get("line").and_then(Value::as_u64) == Some(u64::from(c.line))
                    && p.get("body").and_then(Value::as_str) == Some(c.body.as_str())
            })?;
            Some(Published {
                finding: c.finding.clone(),
                thread: String::new(),
                comment: hit.get("id").and_then(Value::as_i64)?.to_string(),
                url: hit
                    .get("html_url")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

// ===================================================================== GitLab

/// GitLab, through `glab`.
///
/// `:id` in an endpoint is the tool's own placeholder for the project of the
/// current directory, so no path here spells the project out.
pub struct GlabForge {
    tool: Tool,
}

impl GlabForge {
    pub fn new(root: &Path) -> Self {
        GlabForge {
            tool: Tool::new("glab", root),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.tool.timeout = timeout;
        self
    }

    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.tool.cancel = Some(flag);
        self
    }

    fn mr(req: &Request, tail: &str) -> String {
        format!("projects/:id/merge_requests/{}{}", req.id, tail)
    }

    fn rest(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ForgeError> {
        let mut args = vec!["api", "--method", method, path];
        let text;
        let stdin = match body {
            Some(b) => {
                args.extend(["--input", "-"]);
                text = b.to_string();
                Some(text.as_bytes())
            }
            None => None,
        };
        self.tool.json(&args, stdin)
    }

    fn discussions(&self, req: &Request) -> Result<Vec<Value>, ForgeError> {
        let path = Self::mr(req, "/discussions?per_page=100");
        self.tool.json_stream(&["api", "--paginate", &path])
    }
}

impl Forge for GlabForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Gitlab
    }

    fn request(&self, id: Option<&str>) -> Result<Request, ForgeError> {
        let mut args = vec!["mr", "view"];
        if let Some(id) = id {
            args.push(id);
        }
        args.extend(["--output", "json"]);
        let v = self
            .tool
            .json(&args, None)
            .map_err(|e| no_request(e, id, "merge request"))?;
        parse_mr(&v)
    }

    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError> {
        let pages = self.discussions(req)?;
        Ok(parse_discussions(&pages, &req.head))
    }

    fn publish(&self, req: &Request, batch: &Batch) -> Result<Vec<Published>, ForgeError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        // Draft notes, then one publish: the author is notified once, as a
        // GitHub review notifies once.
        for c in &batch.comments {
            self.rest(
                "POST",
                &Self::mr(req, "/draft_notes"),
                Some(&draft_note_body(req, c)),
            )?;
        }
        for r in &batch.replies {
            self.rest(
                "POST",
                &Self::mr(req, "/draft_notes"),
                Some(&json!({
                    "note": r.body,
                    "in_reply_to_discussion_id": r.thread,
                })),
            )?;
        }
        self.tool.run(
            &[
                "api",
                "--method",
                "POST",
                &Self::mr(req, "/draft_notes/bulk_publish"),
            ],
            None,
        )?;
        // The publish answers with nothing. The discussions, fetched again,
        // hold every note that landed.
        let pages = self.discussions(req)?;
        Ok(match_gitlab_published(batch, &pages))
    }

    fn set_resolved(&self, req: &Request, thread: &str, resolved: bool) -> Result<(), ForgeError> {
        self.rest(
            "PUT",
            &Self::mr(req, &format!("/discussions/{thread}")),
            Some(&json!({ "resolved": resolved })),
        )?;
        Ok(())
    }
}

/// `glab mr view --output json`: the merge request as the API describes it.
///
/// `diff_refs` is the three shas a position needs: the target branch tip when
/// the diff was last computed (`start_sha`), the merge base (`base_sha`), and
/// the head. The project is the URL's path up to `/-/`.
pub fn parse_mr(v: &Value) -> Result<Request, ForgeError> {
    let url = str_of(v, "web_url")?;
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, path)| path)
        .ok_or_else(|| parse_err("web_url has no path"))?;
    let project = path
        .split_once("/-/")
        .map(|(p, _)| p)
        .ok_or_else(|| parse_err("web_url is not a merge request url"))?;
    let iid = v
        .get("iid")
        .and_then(Value::as_i64)
        .ok_or_else(|| parse_err("missing iid"))?;
    let refs = v
        .get("diff_refs")
        .ok_or_else(|| parse_err("missing diff_refs"))?;
    Ok(Request {
        kind: ForgeKind::Gitlab,
        project: project.to_string(),
        id: iid.to_string(),
        base_ref: str_of(v, "target_branch")?.to_string(),
        base_tip: str_of(refs, "start_sha")?.to_string(),
        head: str_of(refs, "head_sha")?.to_string(),
        merge_base: Some(str_of(refs, "base_sha")?.to_string()),
        url: url.to_string(),
    })
}

/// Every page of `GET .../discussions`, as threads.
///
/// Only a discussion whose first note is a `DiffNote` with a text position is
/// a thread here: a comment on the request itself has no line. System notes
/// are dropped. A position recorded against another head is **outdated**: the
/// REST answer carries no diff text to place it by, so it is counted, not
/// drawn.
pub fn parse_discussions(pages: &[Value], head: &str) -> Vec<RemoteThread> {
    pages
        .iter()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|d| parse_discussion(d, head))
        .collect()
}

fn parse_discussion(d: &Value, head: &str) -> Option<RemoteThread> {
    let notes: Vec<&Value> = d
        .get("notes")?
        .as_array()?
        .iter()
        .filter(|n| !n.get("system").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    let first = *notes.first()?;
    if first.get("type").and_then(Value::as_str) != Some("DiffNote") {
        return None;
    }
    let pos = first.get("position")?;
    if pos.get("position_type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let new_line = u32_at(pos, "/new_line");
    let old_line = u32_at(pos, "/old_line");
    let (side, line) = match (new_line, old_line) {
        (Some(n), _) => ("new", n),
        (None, Some(o)) => ("old", o),
        (None, None) => return None,
    };
    let start_line = u32_at(pos, &format!("/line_range/start/{side}_line")).filter(|s| *s < line);
    let outdated = pos.get("head_sha").and_then(Value::as_str) != Some(head);
    let root_id = first.get("id").and_then(Value::as_i64)?.to_string();
    let comments = notes
        .iter()
        .enumerate()
        .map(|(i, n)| RemoteComment {
            id: n
                .get("id")
                .and_then(Value::as_i64)
                .map(|n| n.to_string())
                .unwrap_or_default(),
            author: n
                .pointer("/author/username")
                .and_then(Value::as_str)
                .unwrap_or("(deleted)")
                .to_string(),
            created: n
                .get("created_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            body: n
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            reply_to: (i > 0).then(|| root_id.clone()),
        })
        .collect();
    Some(RemoteThread {
        id: d.get("id")?.as_str()?.to_string(),
        resolved: first
            .get("resolved")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        outdated,
        path: pos
            .get("new_path")
            .and_then(Value::as_str)
            .or_else(|| pos.get("old_path").and_then(Value::as_str))?
            .to_string(),
        side: side.to_string(),
        line: (!outdated).then_some(line),
        start_line: if outdated { None } else { start_line },
        line_text: None,
        anchor: None,
        comments,
    })
}

/// The text of a note that carries a range: the range first, since the
/// position names only the last line.
fn ranged_note(c: &NewComment) -> String {
    match c.start_line {
        Some(start) => format!("(lines {start}-{})\n\n{}", c.line, c.body),
        None => c.body.clone(),
    }
}

/// The body of `POST .../draft_notes` for one new comment.
///
/// A position names both paths and the three shas. A multi-line finding is
/// positioned at its last line and says its range in the note: GitLab's
/// `line_range` wants a `line_code` built from a hash of the path and both
/// sides' numbers, which is not confirmed against a live instance yet.
pub fn draft_note_body(req: &Request, c: &NewComment) -> Value {
    let mut position = json!({
        "position_type": "text",
        "base_sha": req.merge_base.clone().unwrap_or_default(),
        "start_sha": req.base_tip,
        "head_sha": req.head,
        "new_path": c.path,
        "old_path": c.old_path.clone().unwrap_or_else(|| c.path.clone()),
    });
    if c.side == "old" {
        position["old_line"] = json!(c.line);
    } else {
        position["new_line"] = json!(c.line);
    }
    json!({ "note": ranged_note(c), "position": position })
}

/// Pair each sent note with the discussion or note GitLab made of it, from
/// the discussions fetched after the publish. Comments match on path, line
/// and body against a discussion's root; replies on their discussion and
/// body, taking the newest such note.
pub fn match_gitlab_published(batch: &Batch, pages: &[Value]) -> Vec<Published> {
    let discussions: Vec<&Value> = pages.iter().filter_map(Value::as_array).flatten().collect();
    let mut out = Vec::new();
    for c in &batch.comments {
        let want = ranged_note(c);
        let line_key = if c.side == "old" {
            "/old_line"
        } else {
            "/new_line"
        };
        let hit = discussions.iter().find(|d| {
            let Some(first) = d.pointer("/notes/0") else {
                return false;
            };
            let pos = first.get("position");
            first.get("body").and_then(Value::as_str) == Some(want.as_str())
                && pos.and_then(|p| p.get("new_path")).and_then(Value::as_str)
                    == Some(c.path.as_str())
                && pos.and_then(|p| u32_at(p, line_key)) == Some(c.line)
        });
        if let Some(d) = hit
            && let (Some(id), Some(note)) = (
                d.get("id").and_then(Value::as_str),
                d.pointer("/notes/0/id").and_then(Value::as_i64),
            )
        {
            out.push(Published {
                finding: c.finding.clone(),
                thread: id.to_string(),
                comment: note.to_string(),
                url: None,
            });
        }
    }
    for r in &batch.replies {
        let note = discussions
            .iter()
            .find(|d| d.get("id").and_then(Value::as_str) == Some(r.thread.as_str()))
            .and_then(|d| d.get("notes")?.as_array())
            .and_then(|notes| {
                notes
                    .iter()
                    .rev()
                    .find(|n| n.get("body").and_then(Value::as_str) == Some(r.body.as_str()))
            })
            .and_then(|n| n.get("id").and_then(Value::as_i64));
        if let Some(note) = note {
            out.push(Published {
                finding: r.finding.clone(),
                thread: r.thread.clone(),
                comment: note.to_string(),
                url: None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::NewReply;

    #[test]
    fn a_pull_request_is_read_from_gh_pr_view() {
        let v = json!({
            "number": 84,
            "baseRefName": "main",
            "baseRefOid": "ecc9400aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "headRefOid": "d5cd4feac46323dae75365481257bd71fb736603",
            "url": "https://github.com/owner/repo/pull/84"
        });
        let req = parse_request(&v).unwrap();
        assert_eq!(req.kind, ForgeKind::Github);
        assert_eq!(req.project, "owner/repo");
        assert_eq!(req.id, "84");
        assert_eq!(req.base_ref, "main");
        assert_eq!(req.head, "d5cd4feac46323dae75365481257bd71fb736603");
        assert_eq!(
            req.fetch_hint("origin"),
            "git fetch origin main pull/84/head"
        );
    }

    /// The shape GitHub returned for a real thread page, cut to two threads.
    fn threads_page(has_next: bool) -> Value {
        json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
        "pageInfo": {"hasNextPage": has_next, "endCursor": "Y3Vyc29y"},
        "nodes": [
            {"id":"PRRT_a","isResolved":true,"isOutdated":false,"line":39,"startLine":34,
             "diffSide":"RIGHT","path":"assets/record.vhs",
             "comments":{"nodes":[
                {"databaseId":3928619949_i64,"body":"root","author":{"login":"alice"},
                 "createdAt":"2026-09-03T20:53:12Z","replyTo":null,
                 "diffHunk":"@@ -1,100 +1,227 @@\n+# The demo\n+Hide\n+Type \"y\""},
                {"databaseId":3928660390_i64,"body":"reply","author":{"login":"bob"},
                 "createdAt":"2026-09-03T20:58:44Z","replyTo":{"databaseId":3928619949_i64},
                 "diffHunk":"@@ -1,100 +1,227 @@\n+# The demo"}]}},
            {"id":"PRRT_b","isResolved":false,"isOutdated":true,"line":null,"startLine":null,
             "diffSide":"LEFT","path":"src/lib.rs",
             "comments":{"nodes":[
                {"databaseId":1,"body":"gone","author":null,
                 "createdAt":"2026-09-03T20:58:48Z","replyTo":null,
                 "diffHunk":"@@ -5,3 +5,2 @@\n line_5 = 5\n-line_6 = 6"}]}}
        ]}}}}})
    }

    #[test]
    fn a_thread_page_maps_sides_lines_replies_and_the_last_diff_line() {
        let (threads, next) = parse_threads_page(&threads_page(true)).unwrap();
        assert_eq!(next.as_deref(), Some("Y3Vyc29y"));
        assert_eq!(threads.len(), 2);

        let a = &threads[0];
        assert_eq!(a.id, "PRRT_a");
        assert!(a.resolved);
        assert_eq!(
            (a.side.as_str(), a.line, a.start_line),
            ("new", Some(39), Some(34))
        );
        assert_eq!(a.line_text.as_deref(), Some("Type \"y\""));
        assert_eq!(a.comments.len(), 2);
        assert_eq!(a.root().unwrap().id, "3928619949");
        assert_eq!(a.comments[1].reply_to.as_deref(), Some("3928619949"));
        assert_eq!(a.comments[1].author, "bob");

        let b = &threads[1];
        assert!(b.outdated);
        assert_eq!((b.side.as_str(), b.line), ("old", None));
        assert_eq!(b.line_text.as_deref(), Some("line_6 = 6"));
        assert_eq!(b.comments[0].author, "(deleted)");

        let (_, none) = parse_threads_page(&threads_page(false)).unwrap();
        assert!(none.is_none());
    }

    fn request() -> Request {
        Request {
            kind: ForgeKind::Github,
            project: "owner/repo".into(),
            id: "84".into(),
            base_ref: "main".into(),
            base_tip: "b".repeat(40),
            head: "h".repeat(40),
            merge_base: None,
            url: "https://github.com/owner/repo/pull/84".into(),
        }
    }

    fn comment(finding: &str, side: &str, line: u32, start: Option<u32>, body: &str) -> NewComment {
        NewComment {
            finding: finding.into(),
            path: "src/lib.rs".into(),
            old_path: None,
            side: side.into(),
            line,
            start_line: start,
            body: body.into(),
        }
    }

    #[test]
    fn a_review_body_is_one_comment_event_against_the_head() {
        let mut ranged = comment("f2", "old", 8, Some(6), "a range");
        ranged.old_path = Some("src/old.rs".into());
        let comments = vec![comment("f1", "new", 3, None, "one line"), ranged];
        let body = review_body(&request(), &comments);
        assert_eq!(body["commit_id"], json!("h".repeat(40)));
        assert_eq!(body["event"], json!("COMMENT"));
        let items = body["comments"].as_array().unwrap();
        assert_eq!(
            items[0],
            json!({"path":"src/lib.rs","body":"one line","line":3,"side":"RIGHT"})
        );
        assert_eq!(
            items[1],
            json!({"path":"src/lib.rs","body":"a range","line":8,"side":"LEFT",
                   "start_line":6,"start_side":"LEFT"})
        );
        // GitHub takes the new path only; the old path is GitLab's concern.
        assert!(items[1].get("old_path").is_none());
    }

    #[test]
    fn posted_comments_are_matched_back_to_their_findings() {
        let sent = vec![
            {
                let mut c = comment("f1", "new", 3, None, "x");
                c.path = "a.rs".into();
                c
            },
            {
                let mut c = comment("f2", "new", 9, None, "y");
                c.path = "a.rs".into();
                c
            },
        ];
        let posted = json!([
            {"id": 11, "path": "a.rs", "line": 9, "body": "y", "html_url": "https://x/9"},
            {"id": 10, "path": "a.rs", "line": 3, "body": "x", "html_url": "https://x/3"},
        ]);
        let got = match_published(&sent, &posted);
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].finding.as_str(), got[0].comment.as_str()),
            ("f1", "10")
        );
        assert_eq!(
            (got[1].finding.as_str(), got[1].comment.as_str()),
            ("f2", "11")
        );
        assert_eq!(got[1].url.as_deref(), Some("https://x/9"));
        assert!(got[0].thread.is_empty(), "REST never names the thread");
    }

    #[test]
    fn the_last_diff_line_drops_its_marker() {
        assert_eq!(
            last_diff_line("@@ -1 +1 @@\n-old\n+new"),
            Some("new".into())
        );
        assert_eq!(
            last_diff_line("@@ -1 +1 @@\n context"),
            Some("context".into())
        );
        assert_eq!(last_diff_line("@@ -1 +1 @@"), None);
    }

    // ------------------------------------------------------------- GitLab

    const HEAD: &str = "1111111111111111111111111111111111111111";

    fn mr_view() -> Value {
        json!({
            "iid": 12,
            "web_url": "https://gitlab.example.com/group/sub/proj/-/merge_requests/12",
            "source_branch": "feature",
            "target_branch": "main",
            "sha": HEAD,
            "diff_refs": {
                "base_sha": "2222222222222222222222222222222222222222",
                "start_sha": "3333333333333333333333333333333333333333",
                "head_sha": HEAD
            }
        })
    }

    #[test]
    fn a_merge_request_is_read_from_glab_mr_view() {
        let req = parse_mr(&mr_view()).unwrap();
        assert_eq!(req.kind, ForgeKind::Gitlab);
        assert_eq!(req.project, "group/sub/proj");
        assert_eq!(req.id, "12");
        assert_eq!(req.base_ref, "main");
        assert_eq!(req.base_tip, "3".repeat(40));
        assert_eq!(req.head, HEAD);
        assert_eq!(req.merge_base.as_deref(), Some("2".repeat(40).as_str()));
        assert_eq!(
            req.fetch_hint("origin"),
            "git fetch origin main merge-requests/12/head"
        );
    }

    fn note(id: i64, body: &str, author: &str, ty: Option<&str>, position: Option<Value>) -> Value {
        let mut n = json!({
            "id": id, "body": body, "system": false,
            "author": {"username": author},
            "created_at": "2026-09-04T09:00:00Z",
            "resolvable": true, "resolved": false,
        });
        n["type"] = ty.map_or(Value::Null, |t| json!(t));
        if let Some(p) = position {
            n["position"] = p;
        }
        n
    }

    fn position(head: &str, new_line: Option<u32>, old_line: Option<u32>) -> Value {
        json!({
            "base_sha": "2".repeat(40), "start_sha": "3".repeat(40), "head_sha": head,
            "old_path": "src/lib.rs", "new_path": "src/lib.rs", "position_type": "text",
            "old_line": old_line, "new_line": new_line,
        })
    }

    /// Two pages, as `--paginate` prints them: one array per page.
    fn discussion_pages() -> Vec<Value> {
        let mut ranged = position(HEAD, Some(8), None);
        ranged["line_range"] = json!({
            "start": {"new_line": 6, "old_line": null, "type": "new"},
            "end": {"new_line": 8, "old_line": null, "type": "new"},
        });
        vec![
            json!([
                {"id": "d1", "individual_note": false, "notes": [
                    note(101, "why?", "alice", Some("DiffNote"), Some(position(HEAD, Some(3), None))),
                    note(102, "because", "bob", Some("DiffNote"), Some(position(HEAD, Some(3), None))),
                ]},
                {"id": "d2", "individual_note": false, "notes": [
                    note(201, "old side", "carol", Some("DiffNote"), Some(position(HEAD, None, Some(5)))),
                ]},
                // A comment on the request itself: no line, not a thread.
                {"id": "d3", "individual_note": true, "notes": [
                    note(301, "looks good", "dave", None, None),
                ]},
            ]),
            json!([
                {"id": "d4", "individual_note": false, "notes": [
                    {"id": 401, "body": "changed the description", "system": true,
                     "author": {"username": "bot"}, "created_at": "2026-09-04T09:00:00Z"},
                    note(402, "stale", "erin", Some("DiffNote"), Some(position(&"0".repeat(40), Some(9), None))),
                ]},
                {"id": "d5", "individual_note": false, "notes": [
                    note(501, "range", "frank", Some("DiffNote"), Some(ranged)),
                ]},
            ]),
        ]
    }

    #[test]
    fn discussions_become_threads_and_only_diff_notes_count() {
        let threads = parse_discussions(&discussion_pages(), HEAD);
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["d1", "d2", "d4", "d5"], "d3 has no line");

        let d1 = &threads[0];
        assert_eq!(
            (d1.side.as_str(), d1.line, d1.start_line),
            ("new", Some(3), None)
        );
        assert_eq!(d1.comments.len(), 2);
        assert_eq!(d1.root().unwrap().id, "101");
        assert_eq!(d1.comments[1].reply_to.as_deref(), Some("101"));
        assert_eq!(d1.comments[1].author, "bob");

        assert_eq!(
            (threads[1].side.as_str(), threads[1].line),
            ("old", Some(5))
        );

        // Recorded against another head: outdated, and the system note is gone.
        let d4 = &threads[2];
        assert!(d4.outdated);
        assert_eq!(d4.line, None);
        assert_eq!(d4.comments.len(), 1);
        assert_eq!(d4.comments[0].author, "erin");

        let d5 = &threads[3];
        assert_eq!((d5.line, d5.start_line), (Some(8), Some(6)));
    }

    #[test]
    fn a_draft_note_positions_by_three_shas_and_both_paths() {
        let req = parse_mr(&mr_view()).unwrap();
        let mut c = comment("f1", "new", 3, None, "one line");
        c.old_path = Some("src/old.rs".into());
        let body = draft_note_body(&req, &c);
        assert_eq!(body["note"], json!("one line"));
        assert_eq!(body["position"]["position_type"], json!("text"));
        assert_eq!(body["position"]["base_sha"], json!("2".repeat(40)));
        assert_eq!(body["position"]["start_sha"], json!("3".repeat(40)));
        assert_eq!(body["position"]["head_sha"], json!(HEAD));
        assert_eq!(body["position"]["new_path"], json!("src/lib.rs"));
        assert_eq!(body["position"]["old_path"], json!("src/old.rs"));
        assert_eq!(body["position"]["new_line"], json!(3));
        assert!(body["position"].get("old_line").is_none());

        // A range is positioned at its last line and says so in the note.
        let ranged = draft_note_body(&req, &comment("f2", "old", 8, Some(6), "a range"));
        assert_eq!(ranged["note"], json!("(lines 6-8)\n\na range"));
        assert_eq!(ranged["position"]["old_line"], json!(8));
        assert!(ranged["position"].get("new_line").is_none());
    }

    #[test]
    fn published_notes_are_matched_from_the_refetched_discussions() {
        let batch = Batch {
            comments: vec![comment("f1", "new", 3, None, "why?")],
            replies: vec![NewReply {
                finding: "f2".into(),
                thread: "d1".into(),
                root_comment: "101".into(),
                body: "because".into(),
            }],
        };
        let got = match_gitlab_published(&batch, &discussion_pages());
        assert_eq!(got.len(), 2);
        assert_eq!(
            (
                got[0].finding.as_str(),
                got[0].thread.as_str(),
                got[0].comment.as_str()
            ),
            ("f1", "d1", "101")
        );
        assert_eq!(
            (
                got[1].finding.as_str(),
                got[1].thread.as_str(),
                got[1].comment.as_str()
            ),
            ("f2", "d1", "102")
        );
    }
}
