//! The forge adapters: `gh` for GitHub (ADR 0029, `spec/forge.md`).
//!
//! A forge is a tool on the path. `gh` is logged in by its own login flow,
//! knows the remote's host and project from the working directory, and prints
//! JSON for any endpoint — so this module holds no token, no hostname and no
//! HTTP client. It runs the tool through the same runner the model backend
//! uses, and maps the JSON it gets back onto `engine::forge`'s types.
//!
//! Every mapping is a pure function of a `serde_json::Value`, tested against
//! recorded answers, so the shape of what GitHub says is pinned here and a
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

/// GitHub, through `gh`.
pub struct GhForge {
    /// The repository root: `gh` resolves the remote from the directory it
    /// runs in, exactly as `git` does.
    working_dir: PathBuf,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
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
            working_dir: root.to_path_buf(),
            timeout: Duration::from_secs(60),
            cancel: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// Run `gh` with these arguments and return its stdout.
    fn gh(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, ForgeError> {
        let argv: Vec<String> = std::iter::once("gh")
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

    fn gh_json(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Value, ForgeError> {
        let bytes = self.gh(args, stdin)?;
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Parse {
            command: format!("gh {}", args.join(" ")),
            msg: e.to_string(),
        })
    }

    fn graphql(&self, query: &str, vars: &[(&str, Value)]) -> Result<Value, ForgeError> {
        let body = json!({
            "query": query,
            "variables": vars
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<serde_json::Map<String, Value>>(),
        });
        let v = self.gh_json(
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
        self.gh_json(&args, stdin)
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
        let v = match self.gh_json(&args, None) {
            Ok(v) => v,
            // Without a number the question was "which request is this
            // branch", and "none" is an answer rather than a broken tool.
            Err(ForgeError::Failed { stderr, .. }) if id.is_none() => {
                return Err(ForgeError::NoRequest(format!(
                    "the current branch has no pull request ({stderr})"
                )));
            }
            Err(e) => return Err(e),
        };
        parse_request(&v)
    }

    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError> {
        let (owner, name) = req
            .project
            .split_once('/')
            .ok_or_else(|| ForgeError::Parse {
                command: "gh api graphql".into(),
                msg: format!("project {:?} is not owner/repo", req.project),
            })?;
        let number: i64 = req.id.parse().map_err(|_| ForgeError::Parse {
            command: "gh api graphql".into(),
            msg: format!("pull request number {:?} is not a number", req.id),
        })?;
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
            let review_id =
                review
                    .get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| ForgeError::Parse {
                        command: "gh api pulls/reviews".into(),
                        msg: "the review came back without an id".into(),
                    })?;
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

// ----------------------------------------------------------------- mappings

fn parse_err(msg: impl Into<String>) -> ForgeError {
    ForgeError::Parse {
        command: "gh".into(),
        msg: msg.into(),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> Result<&'a str, ForgeError> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| parse_err(format!("missing {key}")))
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
        .map(parse_thread)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((threads, next))
}

fn parse_thread(t: &Value) -> Result<RemoteThread, ForgeError> {
    let comments = t
        .pointer("/comments/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .map(parse_comment)
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
        line: t.get("line").and_then(Value::as_u64).map(|n| n as u32),
        start_line: t.get("startLine").and_then(Value::as_u64).map(|n| n as u32),
        line_text,
        anchor: None,
        comments,
    })
}

fn parse_comment(c: &Value) -> Result<RemoteComment, ForgeError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            url: "https://github.com/owner/repo/pull/84".into(),
        }
    }

    #[test]
    fn a_review_body_is_one_comment_event_against_the_head() {
        let comments = vec![
            NewComment {
                finding: "f1".into(),
                path: "src/lib.rs".into(),
                old_path: None,
                side: "new".into(),
                line: 3,
                start_line: None,
                body: "one line".into(),
            },
            NewComment {
                finding: "f2".into(),
                path: "src/lib.rs".into(),
                old_path: Some("src/old.rs".into()),
                side: "old".into(),
                line: 8,
                start_line: Some(6),
                body: "a range".into(),
            },
        ];
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
            NewComment {
                finding: "f1".into(),
                path: "a.rs".into(),
                old_path: None,
                side: "new".into(),
                line: 3,
                start_line: None,
                body: "x".into(),
            },
            NewComment {
                finding: "f2".into(),
                path: "a.rs".into(),
                old_path: None,
                side: "new".into(),
                line: 9,
                start_line: None,
                body: "y".into(),
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
}
