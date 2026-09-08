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
use std::time::Duration;

use serde_json::{Value, json};

use sha1::{Digest, Sha1};

use crate::forge::{
    Batch, Forge, ForgeError, ForgeKind, NewComment, NewReply, Published, RemoteComment,
    RemoteThread, Request, Sent, marker, strip_marker,
};
use crate::subprocess;

/// One command-line tool, run from the repository root with a deadline.
///
/// The root, because both tools resolve the remote from the directory they
/// run in, exactly as `git` does.
struct Tool {
    /// The name on the path, or a path to the executable.
    program: String,
    working_dir: PathBuf,
}

/// Long enough for a paginated read of a large request over a slow link;
/// short enough that a hung tool gives the reviewer back within a minute.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

impl Tool {
    fn new(program: &str, root: &Path) -> Self {
        Tool {
            program: program.to_string(),
            working_dir: root.to_path_buf(),
        }
    }

    /// Run the tool with these arguments and return its stdout.
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, ForgeError> {
        let argv: Vec<String> = std::iter::once(self.program.as_str())
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        let command = || argv.join(" ");
        let out = subprocess::run(&subprocess::Run {
            argv: &argv,
            stdin,
            working_dir: Some(&self.working_dir),
            timeout: TOOL_TIMEOUT,
            cancel: None,
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
                timeout: TOOL_TIMEOUT,
            },
            subprocess::Failure::Cancelled => ForgeError::Cancelled { command: command() },
        })?;
        if !out.status.success() {
            let output = [&out.stderr, &out.stdout]
                .into_iter()
                .map(|bytes| subprocess::stderr_excerpt(bytes, 600))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(ForgeError::Failed {
                command: command(),
                code: out.status.code(),
                output,
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

    /// `DELETE` at a path. Not read as JSON: a delete answers with no body.
    fn delete_at(&self, path: &str) -> Result<(), ForgeError> {
        self.run(&["api", "--method", "DELETE", path], None)
            .map(|_| ())
    }

    /// One REST call through `<tool> api` with the body as the tool's own
    /// field flags — `-f` for a string, `-F` for a number, a bool or a JSON
    /// object — which the tool sends as JSON with the content type set. A raw
    /// body on stdin is sent as-is: `gh` labels it JSON, `glab` does not, and
    /// GitLab answered `HTTP 415` on the first live write.
    fn rest_fields(
        &self,
        method: &str,
        path: &str,
        fields: &[(&str, &Value)],
    ) -> Result<Value, ForgeError> {
        let args = field_args(method, path, fields);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.json(&refs, None)
    }

    /// One REST call through `<tool> api`, a JSON body on stdin when there is
    /// one, the JSON answer back. For `gh`, which labels the body as JSON.
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
        self.json(&args, stdin)
    }
}

/// The argv of one `api` call with its body as field flags: `-f name=text`
/// for a string, `-F name=value` for anything the tool should type — a
/// number, a bool, a JSON object or array.
fn field_args(method: &str, path: &str, fields: &[(&str, &Value)]) -> Vec<String> {
    let mut args: Vec<String> = ["api", "--method", method, path]
        .into_iter()
        .map(str::to_string)
        .collect();
    for (name, value) in fields {
        let (flag, text) = match value {
            Value::String(s) => ("-f", s.clone()),
            other => ("-F", other.to_string()),
        };
        args.push(flag.to_string());
        args.push(format!("{name}={text}"));
    }
    args
}

/// Without a number the question was "which request is this branch", and
/// "none" is an answer rather than a broken tool.
fn no_request(err: ForgeError, id: Option<&str>, noun: &str) -> ForgeError {
    match err {
        ForgeError::Failed { output, .. } if id.is_none() => {
            ForgeError::NoRequest(format!("the current branch has no {noun} ({output})"))
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

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    v.get(key).and_then(Value::as_u64).map(|n| n as u32)
}

/// Whether a forge's record of a comment carries this marker in its body.
fn has_marker(v: &Value, mark: &str) -> bool {
    v.get("body")
        .and_then(Value::as_str)
        .is_some_and(|b| b.contains(mark))
}

/// A reply's record from the forge's answer to posting it, keyed back to its
/// finding and thread.
fn reply_published(r: &NewReply, answer: &Value) -> Published {
    Published {
        finding: r.finding.clone(),
        thread: r.thread.clone(),
        comment: answer
            .get("id")
            .and_then(Value::as_i64)
            .map(|n| n.to_string())
            .unwrap_or_default(),
        url: answer
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
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

    fn pulls(req: &Request, tail: &str) -> String {
        format!("repos/{}/pulls/{}{}", req.project, req.id, tail)
    }

    /// The comments of a review just submitted: the review's own answer names
    /// none of them.
    fn review_comments(&self, req: &Request, review: &Value) -> Result<Value, ForgeError> {
        let review_id = review
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| parse_err("the review came back without an id"))?;
        self.tool.rest(
            "GET",
            &Self::pulls(req, &format!("/reviews/{review_id}/comments")),
            None,
        )
    }
}

impl Forge for GhForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Github
    }

    fn whoami(&self) -> Result<String, ForgeError> {
        let v = self.tool.json(&["api", "user"], None)?;
        Ok(str_of(&v, "login")?.to_string())
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
            .map_err(|e| no_request(e, id, self.kind().noun()))?;
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

    fn publish(&self, req: &Request, batch: &Batch) -> Result<Sent, ForgeError> {
        if batch.is_empty() {
            return Ok(Sent::default());
        }
        let mut sent = Sent::default();

        // New comments: one review, so the author gets one notification. Up
        // to this call nothing has left; from its answer on, a failure is
        // reported in `sent`, never returned, or the caller would forget
        // comments that are already live.
        if !batch.comments.is_empty() {
            let review = self.tool.rest(
                "POST",
                &Self::pulls(req, "/reviews"),
                Some(&review_body(req, &batch.comments)),
            )?;
            match self.review_comments(req, &review) {
                Ok(posted) => sent
                    .published
                    .extend(match_published(&batch.comments, &posted)),
                Err(e) => {
                    sent.failed = Some(e);
                    return Ok(sent);
                }
            }
        }

        // Replies thread under the root comment, one call each.
        for r in &batch.replies {
            let v = match self.tool.rest(
                "POST",
                &Self::pulls(req, &format!("/comments/{}/replies", r.root_comment)),
                Some(&json!({ "body": r.body })),
            ) {
                Ok(v) => v,
                Err(e) if sent.published.is_empty() && batch.comments.is_empty() => return Err(e),
                Err(e) => {
                    sent.failed = Some(e);
                    return Ok(sent);
                }
            };
            sent.published.push(reply_published(r, &v));
        }

        // A new comment's thread id is GraphQL's, which REST never says. One
        // fetch of the threads names every root — and is the fresh set the
        // caller wants, so it is handed back rather than fetched twice.
        match self.threads(req) {
            Ok(threads) => {
                for p in sent.published.iter_mut().filter(|p| p.thread.is_empty()) {
                    if let Some(t) = threads
                        .iter()
                        .find(|t| t.root().is_some_and(|c| c.id == p.comment))
                    {
                        p.thread = t.id.clone();
                    }
                }
                sent.threads = Some(threads);
            }
            Err(e) => sent.failed = Some(e),
        }
        Ok(sent)
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

    fn edit_comment(
        &self,
        req: &Request,
        _thread: &str,
        comment: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.tool.rest(
            "PATCH",
            &format!("repos/{}/pulls/comments/{comment}", req.project),
            Some(&json!({ "body": body })),
        )?;
        Ok(())
    }

    fn delete_comment(
        &self,
        req: &Request,
        _thread: &str,
        comment: &str,
    ) -> Result<(), ForgeError> {
        self.tool
            .delete_at(&format!("repos/{}/pulls/comments/{comment}", req.project))
    }
}

/// `gh pr view --json number,baseRefName,baseRefOid,headRefOid,url`.
///
/// The project comes from the URL: the request lives in the base repository,
/// and `gh pr view` names the head repository only.
fn parse_request(v: &Value) -> Result<Request, ForgeError> {
    let url = str_of(v, "url")?;
    // `…/owner/repo/pull/123`, read from the right.
    let mut segments = url.trim_end_matches('/').rsplit('/').skip(2);
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
fn parse_threads_page(v: &Value) -> Result<(Vec<RemoteThread>, Option<String>), ForgeError> {
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
        line: u32_of(t, "line"),
        start_line: u32_of(t, "startLine"),
        line_text,
        anchor: None,
        comments,
    })
}

fn parse_gh_comment(c: &Value) -> Result<RemoteComment, ForgeError> {
    let (body, finding) = strip_marker(str_of(c, "body")?);
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
        body,
        finding,
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
fn review_body(req: &Request, comments: &[NewComment]) -> Value {
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

/// Pair each sent comment with the record GitHub made of it, by the marker
/// its body carries. The review's answer has no ids for its comments; the
/// list of the review's comments does. The marker rather than path, line
/// and body: an equality on the stored text matched nothing the first time
/// this ran against the real forge, and a publish that cannot find what it
/// sent is a publish that sends it again.
fn match_published(sent: &[NewComment], posted: &Value) -> Vec<Published> {
    let Some(posted) = posted.as_array() else {
        return Vec::new();
    };
    sent.iter()
        .filter_map(|c| {
            let mark = marker(&c.finding);
            let hit = posted.iter().find(|p| has_marker(p, &mark))?;
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
        Self::with_tool("glab", root)
    }

    /// The same adapter over another executable: a scripted `glab` in a test.
    fn with_tool(program: &str, root: &Path) -> Self {
        GlabForge {
            tool: Tool::new(program, root),
        }
    }

    fn mr(req: &Request, tail: &str) -> String {
        format!("projects/:id/merge_requests/{}{}", req.id, tail)
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

    fn whoami(&self) -> Result<String, ForgeError> {
        let v = self.tool.json(&["api", "user"], None)?;
        Ok(str_of(&v, "username")?.to_string())
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
            .map_err(|e| no_request(e, id, self.kind().noun()))?;
        parse_mr(&v)
    }

    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError> {
        let pages = self.discussions(req)?;
        Ok(parse_discussions(&pages, &req.head))
    }

    fn publish(&self, req: &Request, batch: &Batch) -> Result<Sent, ForgeError> {
        if batch.is_empty() {
            return Ok(Sent::default());
        }
        let mut sent = Sent::default();
        // Nothing is live until the first reply lands or the drafts are
        // published; an error before that is `Err`. After it, the error
        // rides in `sent.failed` behind whatever is already recorded.
        let stop = |mut sent: Sent, e: ForgeError| {
            if sent.published.is_empty() {
                return Err(e);
            }
            sent.failed = Some(e);
            Ok(sent)
        };

        // Replies first, one call each into their discussion: the note
        // endpoint is the documented way to add to a thread, and it answers
        // with the note, so each reply is on record the moment it lands and
        // needs no fetch to be found again. A reply that fails stops the
        // batch before any new comment has gone up, so nothing live is ever
        // unrecorded. (A draft note with `in_reply_to_discussion_id` came out
        // as a new discussion on the first live run.)
        for r in &batch.replies {
            let body = json!(r.body);
            let v = match self.tool.rest_fields(
                "POST",
                &Self::mr(req, &format!("/discussions/{}/notes", r.thread)),
                &[("body", &body)],
            ) {
                Ok(v) => v,
                Err(e) => return stop(sent, e),
            };
            sent.published.push(reply_published(r, &v));
        }
        if batch.comments.is_empty() {
            return Ok(sent);
        }

        // New comments: draft notes, then one publish, so the author is
        // notified once, as a GitHub review notifies once. A draft is not
        // live, so a failure before the publish leaves nothing to record —
        // though the drafts already made stay on the request, unpublished,
        // which the spec names as a limit.
        for c in &batch.comments {
            // The position goes in the query string as `position[key]=…`, not
            // as a `position` object in the JSON body: this endpoint reads the
            // hash only from bracket-encoded params, and a body object comes
            // back "position[base_sha] is missing" for every key. The note,
            // which carries newlines and the marker, stays a body field.
            let query = encode_query(&draft_note_position(req, c));
            let path = Self::mr(req, &format!("/draft_notes?{query}"));
            let note = json!(ranged_note(c));
            if let Err(e) = self.tool.rest_fields("POST", &path, &[("note", &note)]) {
                return stop(sent, e);
            }
        }
        let bulk = self.tool.run(
            &[
                "api",
                "--method",
                "POST",
                &Self::mr(req, "/draft_notes/bulk_publish"),
            ],
            None,
        );
        if let Err(e) = bulk {
            return stop(sent, e);
        }

        // Live from here. The bulk publish answers with nothing; the
        // discussions, fetched once, hold every note that landed — and are
        // the fresh set the caller wants, so they are handed back rather than
        // fetched twice. If this fetch fails the notes are live and unnamed,
        // and the markers name them on the next fetch.
        match self.discussions(req) {
            Ok(pages) => {
                sent.published
                    .extend(match_gitlab_published(&batch.comments, &pages));
                sent.threads = Some(parse_discussions(&pages, &req.head));
            }
            Err(e) => sent.failed = Some(e),
        }
        Ok(sent)
    }

    fn set_resolved(&self, req: &Request, thread: &str, resolved: bool) -> Result<(), ForgeError> {
        self.tool.rest_fields(
            "PUT",
            &Self::mr(req, &format!("/discussions/{thread}")),
            &[("resolved", &json!(resolved))],
        )?;
        Ok(())
    }

    fn edit_comment(
        &self,
        req: &Request,
        thread: &str,
        comment: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.tool.rest_fields(
            "PUT",
            &Self::mr(req, &format!("/discussions/{thread}/notes/{comment}")),
            &[("body", &json!(body))],
        )?;
        Ok(())
    }

    fn delete_comment(&self, req: &Request, thread: &str, comment: &str) -> Result<(), ForgeError> {
        self.tool.delete_at(&Self::mr(
            req,
            &format!("/discussions/{thread}/notes/{comment}"),
        ))
    }
}

/// `glab mr view --output json`: the merge request as the API describes it.
///
/// `diff_refs` is the three shas a position needs: the target branch tip when
/// the diff was last computed (`start_sha`), the merge base (`base_sha`), and
/// the head. The project is the URL's path up to `/-/`.
fn parse_mr(v: &Value) -> Result<Request, ForgeError> {
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
fn parse_discussions(pages: &[Value], head: &str) -> Vec<RemoteThread> {
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
    let new_line = u32_of(pos, "new_line");
    let old_line = u32_of(pos, "old_line");
    let (side, line) = match (new_line, old_line) {
        (Some(n), _) => ("new", n),
        (None, Some(o)) => ("old", o),
        (None, None) => return None,
    };
    let start_line = u32_at(pos, &format!("/line_range/start/{side}_line")).filter(|s| *s < line);
    let outdated = pos.get("head_sha").and_then(Value::as_str) != Some(head);
    first.get("id").and_then(Value::as_i64)?;
    let comments = notes
        .iter()
        .map(|n| {
            let (body, finding) =
                strip_marker(n.get("body").and_then(Value::as_str).unwrap_or_default());
            RemoteComment {
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
                body,
                finding,
            }
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

/// `key=value&…`, each side percent-encoded. Building a query string by hand
/// is where an unescaped `/` or `+` in a path or sha corrupts a request, so
/// the encoder is `percent_encoding`, not `format!`. The set encodes every
/// reserved character — the `[` `]` of a bracket key, the `/` of a path —
/// and leaves the URL-unreserved `-_.~` alone.
fn encode_query(fields: &[(String, String)]) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    let enc = |s: &str| utf8_percent_encode(s, UNRESERVED).to_string();
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The `position[key]=value` fields of `POST .../draft_notes` for one new
/// comment, in the order GitLab documents them.
///
/// A position names both paths and the three shas. A multi-line finding is
/// positioned at its last line and says its range in the note: GitLab's
/// `line_range` wants a `line_code` built from a hash of the path and both
/// sides' numbers, which is not confirmed against a live instance yet.
fn draft_note_position(req: &Request, c: &NewComment) -> Vec<(String, String)> {
    let mut fields = vec![
        ("position[position_type]".into(), "text".into()),
        (
            "position[base_sha]".into(),
            req.merge_base.clone().unwrap_or_default(),
        ),
        ("position[start_sha]".into(), req.base_tip.clone()),
        ("position[head_sha]".into(), req.head.clone()),
        ("position[new_path]".into(), c.path.clone()),
        (
            "position[old_path]".into(),
            c.old_path.clone().unwrap_or_else(|| c.path.clone()),
        ),
    ];
    // GitLab wants one number for a changed line and both for an unchanged
    // one, which exists on both sides.
    let (mine, other) = if c.side == "old" {
        ("old_line", "new_line")
    } else {
        ("new_line", "old_line")
    };
    fields.push((format!("position[{mine}]"), c.line.to_string()));
    if let Some(o) = c.other_line {
        fields.push((format!("position[{other}]"), o.to_string()));
    }
    // A multi-line comment names each end: a `line_code`, a `type`, and the
    // real line number on each side the line exists — the shape GitLab's own
    // web UI sends. See `forge::line_end` for how the three kinds differ.
    if let Some(span) = &c.span {
        for (end, e) in [("start", &span.start), ("end", &span.end)] {
            let at = format!("position[line_range][{end}]");
            fields.push((format!("{at}[line_code]"), line_code(&c.path, e.old, e.new)));
            fields.push((format!("{at}[type]"), e.kind.into()));
            if let Some(n) = e.new_line {
                fields.push((format!("{at}[new_line]"), n.to_string()));
            }
            if let Some(o) = e.old_line {
                fields.push((format!("{at}[old_line]"), o.to_string()));
            }
        }
    }
    fields
}

/// GitLab's id for a diff line: the sha1 of the file path, then the old and
/// new line numbers. `<sha>_<old>_<new>`, as the docs and the diff UI form it.
fn line_code(path: &str, old: u32, new: u32) -> String {
    let mut h = Sha1::new();
    h.update(path.as_bytes());
    format!("{}_{old}_{new}", hex::encode(h.finalize()))
}

/// Pair each sent note with the discussion and note GitLab made of it, from
/// the discussions fetched after the publish, by the marker each body
/// carries.
fn match_gitlab_published(sent: &[NewComment], pages: &[Value]) -> Vec<Published> {
    let discussions: Vec<&Value> = pages.iter().filter_map(Value::as_array).flatten().collect();
    sent.iter()
        .filter_map(|c| {
            let mark = marker(&c.finding);
            let (thread, comment) = discussions.iter().find_map(|d| {
                let note = d
                    .get("notes")?
                    .as_array()?
                    .iter()
                    .find(|n| has_marker(n, &mark))?;
                Some((
                    d.get("id")?.as_str()?.to_string(),
                    note.get("id")?.as_i64()?.to_string(),
                ))
            })?;
            Some(Published {
                finding: c.finding.clone(),
                thread,
                comment,
                url: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::with_marker;

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
                {"databaseId":3928619949_i64,"body":format!("root

{}", marker("abc123")),"author":{"login":"alice"},
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
        assert_eq!(a.root().unwrap().body, "root");
        assert_eq!(a.root().unwrap().finding.as_deref(), Some("abc123"));
        assert_eq!(a.comments[1].finding, None);
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
            other_line: None,
            span: None,
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
        // GitHub gives the body back reflowed; only the marker is trusted.
        let posted = json!([
            {"id": 11, "path": "a.rs", "line": 9, "html_url": "https://x/9",
             "body": format!("y\r\n\r\n{}", marker("f2"))},
            {"id": 10, "path": "a.rs", "line": 3, "html_url": "https://x/3",
             "body": format!("x\r\n\r\n{}", marker("f1"))},
            {"id": 12, "path": "a.rs", "line": 3, "html_url": "https://x/3b",
             "body": "x"},
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
    fn a_gitlab_write_goes_as_field_flags_typed_by_shape() {
        let position = json!({"new_line": 3, "new_path": "a.rs"});
        let args = field_args(
            "POST",
            "projects/:id/merge_requests/7/draft_notes",
            &[
                ("note", &json!("why?\n\n<!-- m -->")),
                ("position", &position),
                ("resolved", &json!(true)),
            ],
        );
        assert_eq!(
            args,
            vec![
                "api",
                "--method",
                "POST",
                "projects/:id/merge_requests/7/draft_notes",
                "-f",
                "note=why?\n\n<!-- m -->",
                "-F",
                "position={\"new_line\":3,\"new_path\":\"a.rs\"}",
                "-F",
                "resolved=true",
            ]
        );
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
                    note(101, &with_marker("why?", "f1"), "alice", Some("DiffNote"), Some(position(HEAD, Some(3), None))),
                    note(102, &with_marker("because", "f2"), "bob", Some("DiffNote"), Some(position(HEAD, Some(3), None))),
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
        assert_eq!(d1.comments[1].author, "bob");
        // The marker is read and not shown.
        assert_eq!(d1.comments[0].body, "why?");
        assert_eq!(d1.comments[0].finding.as_deref(), Some("f1"));
        assert_eq!(d1.comments[1].finding.as_deref(), Some("f2"));
        assert_eq!(threads[1].comments[0].finding, None);

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
        let pos: std::collections::HashMap<String, String> =
            draft_note_position(&req, &c).into_iter().collect();
        assert_eq!(ranged_note(&c), "one line");
        assert_eq!(pos["position[position_type]"], "text");
        assert_eq!(pos["position[base_sha]"], "2".repeat(40));
        assert_eq!(pos["position[start_sha]"], "3".repeat(40));
        assert_eq!(pos["position[head_sha]"], HEAD);
        assert_eq!(pos["position[new_path]"], "src/lib.rs");
        assert_eq!(pos["position[old_path]"], "src/old.rs");
        assert_eq!(pos["position[new_line]"], "3");
        assert!(!pos.contains_key("position[old_line]"));

        // An unchanged line carries both numbers.
        let mut ctx = comment("f3", "new", 12, None, "context");
        ctx.other_line = Some(11);
        let both: std::collections::HashMap<String, String> =
            draft_note_position(&req, &ctx).into_iter().collect();
        assert_eq!(both["position[new_line]"], "12");
        assert_eq!(both["position[old_line]"], "11");

        // A range is positioned at its last line and says so in the note.
        let ranged = comment("f2", "old", 8, Some(6), "a range");
        assert_eq!(ranged_note(&ranged), "(lines 6-8)\n\na range");
        let rpos: std::collections::HashMap<String, String> =
            draft_note_position(&req, &ranged).into_iter().collect();
        assert_eq!(rpos["position[old_line]"], "8");
        assert!(!rpos.contains_key("position[new_line]"));

        // The position rides in the query string, not a body object; a path
        // with a slash is escaped so it cannot break the query.
        let query = encode_query(&draft_note_position(&req, &c));
        assert!(
            query.contains("position%5Bnew_path%5D=src%2Flib.rs"),
            "{query}"
        );
        assert!(!query.contains('/'), "{query}");
    }

    #[test]
    fn a_multi_line_draft_note_carries_a_line_range_at_each_end() {
        use crate::forge::LineEnd;
        let req = parse_mr(&mr_view()).unwrap();
        let mut c = comment("f4", "new", 34, Some(15), "a run of lines");
        // Lines 15-34 are all added: `0` on the old side, the real number on
        // the new side, `new` type. This is the shape the web UI sends.
        c.span = Some(crate::forge::LineSpan {
            start: LineEnd {
                kind: "new",
                old: 0,
                new: 15,
                old_line: None,
                new_line: Some(15),
            },
            end: LineEnd {
                kind: "new",
                old: 0,
                new: 34,
                old_line: None,
                new_line: Some(34),
            },
        });
        let pos: std::collections::HashMap<String, String> =
            draft_note_position(&req, &c).into_iter().collect();
        let code = |old: u32, new: u32| line_code("src/lib.rs", old, new);
        assert_eq!(pos["position[line_range][start][line_code]"], code(0, 15));
        assert_eq!(pos["position[line_range][start][type]"], "new");
        assert_eq!(pos["position[line_range][start][new_line]"], "15");
        assert!(!pos.contains_key("position[line_range][start][old_line]"));
        assert_eq!(pos["position[line_range][end][line_code]"], code(0, 34));
        assert_eq!(pos["position[line_range][end][new_line]"], "34");
        // The last line is still the anchor position.
        assert_eq!(pos["position[new_line]"], "34");
        // A line_code is the path's sha1, then the two numbers, `0` for the
        // side an added line is missing from.
        assert!(code(0, 34).ends_with("_0_34"), "{}", code(0, 34));
        assert_eq!(code(0, 34).len(), 40 + "_0_34".len());
    }

    #[test]
    fn a_deleted_range_takes_the_new_side_position_not_zero() {
        use crate::forge::LineEnd;
        let req = parse_mr(&mr_view()).unwrap();
        let mut c = comment("f5", "old", 2953, Some(2950), "on a deleted run");
        // Deleted old lines 2950-2953 sit at new-side position 2952; the
        // `line_code`'s new number is that position, shared by both ends.
        c.span = Some(crate::forge::LineSpan {
            start: LineEnd {
                kind: "old",
                old: 2950,
                new: 2952,
                old_line: Some(2950),
                new_line: None,
            },
            end: LineEnd {
                kind: "old",
                old: 2953,
                new: 2952,
                old_line: Some(2953),
                new_line: None,
            },
        });
        let pos: std::collections::HashMap<String, String> =
            draft_note_position(&req, &c).into_iter().collect();
        let code = |old: u32, new: u32| line_code("src/lib.rs", old, new);
        assert_eq!(
            pos["position[line_range][start][line_code]"],
            code(2950, 2952)
        );
        assert_eq!(pos["position[line_range][start][type]"], "old");
        assert_eq!(pos["position[line_range][start][old_line]"], "2950");
        assert!(!pos.contains_key("position[line_range][start][new_line]"));
        assert_eq!(
            pos["position[line_range][end][line_code]"],
            code(2953, 2952)
        );
    }

    #[test]
    fn published_notes_are_matched_from_the_refetched_discussions() {
        let sent = vec![
            comment("f1", "new", 3, None, &with_marker("why?", "f1")),
            comment("f2", "new", 3, None, &with_marker("because", "f2")),
        ];
        let got = match_gitlab_published(&sent, &discussion_pages());
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

    /// A `glab` that is a shell script: it logs each call to `calls`, reads
    /// which step fails from `mode`, and answers the discussions fetch with
    /// `discussion_pages()`. The adapter's ordering is the thing under test,
    /// and only a tool that fails on cue can show it.
    #[cfg(unix)]
    fn scripted_glab(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let pages: Vec<String> = discussion_pages()
            .iter()
            .map(|p| serde_json::to_string(p).unwrap())
            .collect();
        std::fs::write(dir.join("pages.json"), pages.join("\n")).unwrap();
        let script = dir.join("glab");
        std::fs::write(
            &script,
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls"
mode=$(cat "$dir/mode")
case "$*" in
  *"/discussions/bad/notes"*) echo '{"error":"body is invalid"}'; echo "glab: HTTP 400" >&2; exit 1 ;;
  *"/discussions/"*"/notes"*) echo '{"id": 555, "body": "because"}' ;;
  *"/draft_notes/bulk_publish"*)
    if [ "$mode" = "bulk-fails" ]; then echo "glab: HTTP 500" >&2; exit 1; fi ;;
  *"/draft_notes"*) echo '{"id": 9}' ;;
  *"--paginate"*) cat "$dir/pages.json" ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn calls(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("calls")).unwrap_or_default()
    }

    #[cfg(unix)]
    fn mixed_batch(reply_thread: &str) -> Batch {
        Batch {
            comments: vec![comment("f1", "new", 3, None, &with_marker("why?", "f1"))],
            replies: vec![NewReply {
                finding: "f2".into(),
                thread: reply_thread.into(),
                root_comment: "101".into(),
                body: with_marker("because", "f2"),
            }],
        }
    }

    /// Whatever step fails, nothing that went live is left off the record:
    /// that record is what keeps the next publish from sending it again.
    #[test]
    #[cfg(unix)]
    fn a_gitlab_publish_records_everything_live_before_it_reports_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let glab = scripted_glab(dir.path());
        let forge = GlabForge::with_tool(&glab, dir.path());
        let req = parse_mr(&mr_view()).unwrap();

        // The reply fails: nothing has gone up, so nothing is recorded and
        // the comments were never sent.
        std::fs::write(dir.path().join("mode"), "ok").unwrap();
        let err = forge.publish(&req, &mixed_batch("bad")).unwrap_err();
        // The status from stderr and the forge's reason from stdout, both.
        assert!(err.to_string().contains("HTTP 400"), "{err}");
        assert!(err.to_string().contains("body is invalid"), "{err}");
        assert!(
            !calls(dir.path()).contains("draft_notes"),
            "{}",
            calls(dir.path())
        );

        // The bulk publish fails after the reply landed: the reply is on
        // record, the failure rides behind it, and no fetch was made for
        // notes that never went live.
        std::fs::remove_file(dir.path().join("calls")).unwrap();
        std::fs::write(dir.path().join("mode"), "bulk-fails").unwrap();
        let sent = forge.publish(&req, &mixed_batch("d1")).unwrap();
        assert_eq!(sent.published.len(), 1);
        assert_eq!(
            (
                sent.published[0].finding.as_str(),
                sent.published[0].thread.as_str()
            ),
            ("f2", "d1")
        );
        assert_eq!(sent.published[0].comment, "555");
        assert!(sent.failed.is_some());
        assert!(sent.threads.is_none());
        let log = calls(dir.path());
        assert!(log.contains("draft_notes/bulk_publish"), "{log}");
        assert!(!log.contains("--paginate"), "{log}");

        // Everything lands: the reply from its answer, the comment from the
        // discussions fetched once, which are also handed back.
        std::fs::write(dir.path().join("mode"), "ok").unwrap();
        let sent = forge.publish(&req, &mixed_batch("d1")).unwrap();
        assert!(sent.failed.is_none());
        let mut named: Vec<(&str, &str, &str)> = sent
            .published
            .iter()
            .map(|p| (p.finding.as_str(), p.thread.as_str(), p.comment.as_str()))
            .collect();
        named.sort();
        assert_eq!(named, vec![("f1", "d1", "101"), ("f2", "d1", "555")]);
        assert_eq!(sent.threads.as_ref().map(Vec::len), Some(4));
    }
}
