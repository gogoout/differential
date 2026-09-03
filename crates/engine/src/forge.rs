//! The forge consumer's domain (ADR 0029, `spec/forge.md`).
//!
//! What a pull request or merge request is to a review, the forge's review
//! threads as the review sees them, and the two decisions that sit between a
//! forge and the reader's findings: where a fetched thread lands in the diff,
//! and which findings a publish may send.
//!
//! Nothing here runs a program. The adapters that speak to `gh` and `glab`
//! implement [`Forge`] and live in `forgeio`; this module is the trait, the
//! types it speaks in, and pure policy over a plan document.

use serde::{Deserialize, Serialize};

use crate::model::DiffView;
use crate::ports::ReviewIdentity;
use crate::review_state::{Anchor, Finding, FindingStatus};
use crate::schema;

/// Which forge a request lives on. The flag that names the request names
/// this too: `--pr` is GitHub and `--mr` is GitLab, and nothing infers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Github,
    Gitlab,
}

impl ForgeKind {
    /// The wire name, identical to `source.remote.forge` in the document.
    pub fn name(self) -> &'static str {
        match self {
            ForgeKind::Github => "github",
            ForgeKind::Gitlab => "gitlab",
        }
    }

    /// What the forge calls the thing: for messages.
    pub fn noun(self) -> &'static str {
        match self {
            ForgeKind::Github => "pull request",
            ForgeKind::Gitlab => "merge request",
        }
    }

    /// The ref a clone fetches to get a request's head without the branch.
    pub fn head_ref(self, id: &str) -> String {
        match self {
            ForgeKind::Github => format!("pull/{id}/head"),
            ForgeKind::Gitlab => format!("merge-requests/{id}/head"),
        }
    }

    pub fn source_kind(self) -> schema::SourceKind {
        match self {
            ForgeKind::Github => schema::SourceKind::Pr,
            ForgeKind::Gitlab => schema::SourceKind::Mr,
        }
    }
}

/// A request as the forge describes it: the object a review is of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub kind: ForgeKind,
    /// `owner/repo` on GitHub, the full namespaced path on GitLab.
    pub project: String,
    /// The number, as a string: GitLab's iid is a number too, and neither is
    /// ever arithmetic here.
    pub id: String,
    /// The branch the request targets, for the fetch hint.
    pub base_ref: String,
    /// The tip of that branch, as the forge sees it now.
    pub base_tip: String,
    /// The request's head commit, as the forge sees it now.
    pub head: String,
    pub url: String,
}

impl Request {
    /// The document's `source.remote`.
    pub fn remote(&self) -> schema::Remote {
        schema::Remote {
            forge: self.kind.name().to_string(),
            project: self.project.clone(),
            id: self.id.clone(),
        }
    }

    /// The review this request opens. Keyed like a name: the request is an
    /// object, and its endpoints are attributes that are allowed to move.
    pub fn identity(&self) -> ReviewIdentity {
        ReviewIdentity::Remote(self.remote())
    }

    /// The command a reader runs when the request's commits are not local.
    pub fn fetch_hint(&self, remote_name: &str) -> String {
        format!(
            "git fetch {remote_name} {} {}",
            self.base_ref,
            self.kind.head_ref(&self.id)
        )
    }
}

/// One comment in a thread. `reply_to` is `None` on the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteComment {
    pub id: String,
    pub author: String,
    /// As the forge gives it, ISO 8601. Displayed as an age, never parsed
    /// for anything else.
    pub created: String,
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// One review thread: where the forge put it, and where this review did.
///
/// The forge's coordinates are kept beside the anchor so a thread loaded from
/// a stale cache can be placed again against a newer plan — `place` is a pure
/// function of them and the document, and it runs on every open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteThread {
    /// The forge's thread id, opaque. GraphQL node id on GitHub, discussion
    /// id on GitLab.
    pub id: String,
    pub resolved: bool,
    /// The forge says the line has left the request's diff. Such a thread
    /// has no `line`, and is placed by content or not at all.
    pub outdated: bool,
    /// The file's path in the request, which is the new path.
    pub path: String,
    /// `"old"` | `"new"`, the review's words for LEFT and RIGHT.
    pub side: String,
    /// The last (or only) line, in the side's numbering. `None` when outdated.
    #[serde(default)]
    pub line: Option<u32>,
    /// The first line of a multi-line thread.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// The text of `line`, when the forge recorded the diff around it. The
    /// content key for a thread whose line is gone.
    #[serde(default)]
    pub line_text: Option<String>,
    /// Where this review shows the thread. `None` until placed, and `None`
    /// after placing when nothing in the plan holds it.
    #[serde(default)]
    pub anchor: Option<Anchor>,
    pub comments: Vec<RemoteComment>,
}

impl RemoteThread {
    /// The comment a reply is threaded under. GitHub replies to the root;
    /// GitLab replies to the discussion, whose id this thread already is.
    pub fn root(&self) -> Option<&RemoteComment> {
        self.comments.first()
    }

    /// Whether `finding` is this thread's own root, published from here.
    pub fn is_twin_of(&self, finding: &Finding) -> bool {
        match &finding.upstream {
            Some(up) => up.thread == self.id || self.comments.iter().any(|c| c.id == up.comment),
            None => false,
        }
    }
}

/// A new review comment to publish: a finding that is not a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewComment {
    pub finding: String,
    /// The path in the request: the new path.
    pub path: String,
    /// The old path when the file was renamed. GitLab wants both; GitHub
    /// wants only `path`.
    pub old_path: Option<String>,
    /// `"old"` | `"new"`.
    pub side: String,
    /// The last (or only) line.
    pub line: u32,
    /// The first line of a multi-line comment; `None` for one line.
    pub start_line: Option<u32>,
    pub body: String,
}

/// A reply to publish into an existing thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReply {
    pub finding: String,
    pub thread: String,
    /// The thread's root comment id, for a forge that threads under a comment.
    pub root_comment: String,
    pub body: String,
}

/// What one publish sends: everything in one submission where the forge
/// allows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    pub comments: Vec<NewComment>,
    pub replies: Vec<NewReply>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.comments.is_empty() && self.replies.is_empty()
    }

    pub fn len(&self) -> usize {
        self.comments.len() + self.replies.len()
    }
}

/// A finding a publish left out, and why, in words for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    pub finding: String,
    pub file: String,
    pub lines: String,
    pub reason: String,
}

/// A publish, decided: what goes and what stays.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublishPlan {
    pub batch: Batch,
    pub excluded: Vec<Excluded>,
}

/// One comment the forge accepted, keyed back to its finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub finding: String,
    pub thread: String,
    pub comment: String,
    pub url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("failed to spawn {command}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{command} exited with {code:?}: {stderr}")]
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("{command} did not finish within {timeout:?}")]
    Timeout {
        command: String,
        timeout: std::time::Duration,
    },
    #[error("{command} was cancelled")]
    Cancelled { command: String },
    #[error("{command}: {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not read {command}'s answer: {msg}")]
    Parse { command: String, msg: String },
    #[error("{0}")]
    NoRequest(String),
}

/// The forge, as the domain needs it. `dyn`: which forge a repository is on
/// is a run-time answer, like which model groups (ADR 0020, 0029).
pub trait Forge: Send + Sync {
    fn kind(&self) -> ForgeKind;

    /// The request with this id, or the current branch's when `None`.
    fn request(&self, id: Option<&str>) -> Result<Request, ForgeError>;

    /// Every review thread on the request, unplaced (`anchor: None`).
    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError>;

    /// Send one batch. Comments against `req.head`; the caller has already
    /// checked that is the review's head.
    fn publish(&self, req: &Request, batch: &Batch) -> Result<Vec<Published>, ForgeError>;

    fn set_resolved(&self, req: &Request, thread: &str, resolved: bool) -> Result<(), ForgeError>;
}

// ------------------------------------------------------------------ placing

/// Where a fetched thread lands in this plan.
///
/// Three tries, cheapest and most certain first. A line the forge gave that a
/// hunk on that side holds is exact: the digest and the offset are the hunk's.
/// A line no hunk holds is context, and lands on the nearest hunk in the file
/// at a signed offset, which is how a finding on a context line is recorded
/// too. A thread with no line — outdated, the forge says — is looked for by
/// its recorded text in the file's hunks, on its own side first. Nothing
/// found is `anchor: None`, and the thread is counted, not drawn.
pub fn place(doc: &schema::PlanDocument, view: &DiffView, thread: &mut RemoteThread) {
    thread.anchor = None;
    let old = thread.side == "old";
    let in_file = |h: &&schema::HunkEntry| h.file == thread.path;

    if let Some(line) = thread.line {
        let start = thread.start_line.unwrap_or(line).min(line);
        let side_range = |h: &schema::HunkEntry| -> (u32, u32) {
            if old {
                (h.old_start.max(1), h.old_count)
            } else {
                (h.new_start.max(1), h.new_count)
            }
        };
        // Exact: a hunk whose changed lines on this side hold the last line.
        let holds = |h: &&schema::HunkEntry| {
            let (s, n) = side_range(h);
            n > 0 && line >= s && line < s.saturating_add(n)
        };
        // Otherwise the nearest hunk in the file on this side.
        let distance = |h: &schema::HunkEntry| -> u32 {
            let (s, n) = side_range(h);
            let end = s.saturating_add(n.max(1)) - 1;
            if line < s {
                s - line
            } else {
                line.saturating_sub(end)
            }
        };
        let hit = doc
            .hunks
            .iter()
            .enumerate()
            .filter(|(_, h)| in_file(h))
            .find(|(_, h)| holds(h))
            .or_else(|| {
                doc.hunks
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| in_file(h))
                    .min_by_key(|(_, h)| distance(h))
            });
        let Some((hi, h)) = hit else {
            return;
        };
        let (s, n) = side_range(h);
        let vh = &view.hunks[hi];
        let side_lines = if old { &vh.removed } else { &vh.added };
        let text_at = |l: u32| -> Option<String> {
            (l >= s && l < s.saturating_add(n))
                .then(|| side_lines.get((l - s) as usize))
                .flatten()
                .map(|b| String::from_utf8_lossy(b).into_owned())
        };
        let end_line_text = text_at(line)
            .or_else(|| thread.line_text.clone())
            .unwrap_or_default();
        let line_text = if start == line {
            end_line_text.clone()
        } else {
            text_at(start).unwrap_or_default()
        };
        thread.anchor = Some(Anchor {
            file: thread.path.clone(),
            side: thread.side.clone(),
            line: start,
            end_line: line,
            offset: (i64::from(start) - i64::from(s)) as i32,
            span: line - start,
            hunk_digest: h.digest.clone(),
            line_text,
            end_line_text,
        });
        return;
    }

    // No line: find the text.
    let Some(text) = thread.line_text.as_deref().filter(|t| !t.is_empty()) else {
        return;
    };
    let at = |lines: &[Vec<u8>]| lines.iter().position(|l| l == text.as_bytes());
    for (hi, h) in doc.hunks.iter().enumerate().filter(|(_, h)| in_file(h)) {
        let vh = &view.hunks[hi];
        let found = if old {
            at(&vh.removed)
                .map(|p| ("old", p))
                .or_else(|| at(&vh.added).map(|p| ("new", p)))
        } else {
            at(&vh.added)
                .map(|p| ("new", p))
                .or_else(|| at(&vh.removed).map(|p| ("old", p)))
        };
        if let Some((side, offset)) = found {
            let s = if side == "old" {
                h.old_start
            } else {
                h.new_start
            }
            .max(1);
            let line = s + offset as u32;
            thread.anchor = Some(Anchor {
                file: thread.path.clone(),
                side: side.to_string(),
                line,
                end_line: line,
                offset: offset as i32,
                span: 0,
                hunk_digest: h.digest.clone(),
                line_text: text.to_string(),
                end_line_text: text.to_string(),
            });
            return;
        }
    }
}

// --------------------------------------------------------------- publishing

/// Lines of context a request diff shows around each hunk, on both forges'
/// web diffs. A comment further out than this is refused by the forge, and on
/// GitHub it fails the whole review.
pub const REQUEST_CONTEXT: u32 = 3;

/// Which open findings a publish may send, and which it must leave.
///
/// A finding already published is not a candidate. A reply needs its thread
/// to still exist and nothing else. A new comment needs its lines inside the
/// request's diff: within `REQUEST_CONTEXT` of a hunk in its file on its
/// side, both ends. The old path rides along for a renamed file, because
/// GitLab positions a comment by both paths.
pub fn publish_plan(
    doc: &schema::PlanDocument,
    findings: &[Finding],
    threads: &[RemoteThread],
) -> PublishPlan {
    let mut out = PublishPlan::default();
    for f in findings
        .iter()
        .filter(|f| f.status == FindingStatus::Open && f.upstream.is_none())
    {
        if let Some(thread_id) = &f.reply_to {
            match threads.iter().find(|t| &t.id == thread_id) {
                Some(t) => out.batch.replies.push(NewReply {
                    finding: f.id.clone(),
                    thread: t.id.clone(),
                    root_comment: t.root().map(|c| c.id.clone()).unwrap_or_default(),
                    body: f.body.clone(),
                }),
                None => out
                    .excluded
                    .push(excluded(f, "its thread is no longer on the request")),
            }
            continue;
        }
        if !in_request_diff(doc, &f.anchor) {
            out.excluded.push(excluded(
                f,
                "outside the request's diff (more than 3 lines from a change)",
            ));
            continue;
        }
        let old_path = doc
            .files
            .iter()
            .find(|e| e.path == f.anchor.file)
            .and_then(|e| e.old_path.clone());
        out.batch.comments.push(NewComment {
            finding: f.id.clone(),
            path: f.anchor.file.clone(),
            old_path,
            side: f.anchor.side.clone(),
            line: f.anchor.end_line.max(f.anchor.line),
            start_line: (f.anchor.end_line > f.anchor.line).then_some(f.anchor.line),
            body: f.body.clone(),
        });
    }
    out
}

fn excluded(f: &Finding, reason: &str) -> Excluded {
    Excluded {
        finding: f.id.clone(),
        file: f.anchor.file.clone(),
        lines: f.anchor.line_span(),
        reason: reason.to_string(),
    }
}

/// Whether both ends of `a` sit inside the request's diff of its file.
fn in_request_diff(doc: &schema::PlanDocument, a: &Anchor) -> bool {
    let old = a.side == "old";
    let first = a.line;
    let last = a.end_line.max(a.line);
    doc.hunks.iter().filter(|h| h.file == a.file).any(|h| {
        let (s, n) = if old {
            (h.old_start.max(1), h.old_count)
        } else {
            (h.new_start.max(1), h.new_count)
        };
        let lo = s.saturating_sub(REQUEST_CONTEXT);
        let hi = s
            .saturating_add(n)
            .saturating_sub(1)
            .saturating_add(REQUEST_CONTEXT);
        first >= lo && last <= hi
    })
}

/// Whether the forge still has the head this review was opened on. Both
/// forges reject a comment against any other commit, so this is the first
/// thing a publish checks and the batch is not built when it fails.
pub fn head_matches(req: &Request, review_head: &str) -> bool {
    req.head == review_head
}
