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
use crate::plan::ReviewSource;
use crate::ports::{Ancestry, Fetcher, RangeResolver, ReviewIdentity};
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

    /// How far from a change a line comment may sit, or no limit.
    ///
    /// GitHub's public API resolves a line against the request's diff with
    /// exactly `REQUEST_CONTEXT` lines around each hunk — measured on a live
    /// request: three after a change lands, four is refused. GitLab positions
    /// a note by its own line numbers against the diff refs and is not held to
    /// that rule here; if it refuses a line, its refusal is what the reader
    /// sees.
    pub fn line_rule(self) -> Option<u32> {
        match self {
            ForgeKind::Github => Some(REQUEST_CONTEXT),
            ForgeKind::Gitlab => None,
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
    /// The merge base, when the forge says it (GitLab's `diff_refs.base_sha`).
    /// A GitLab position names it; the review's range is computed from git
    /// either way.
    pub merge_base: Option<String>,
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
    /// As the forge gives it, ISO 8601. Shown as its date, never parsed
    /// for anything else.
    pub created: String,
    pub body: String,
    /// The finding this comment was published from, when its body carried
    /// the marker `with_marker` writes. What makes a publish idempotent: a
    /// comment that says which finding it is can be matched without trusting
    /// the forge's answer to the publish itself.
    #[serde(default)]
    pub finding: Option<String>,
}

/// The marker a published body ends with: an HTML comment, which neither
/// forge renders, carrying the finding's id.
pub(crate) fn marker(finding: &str) -> String {
    format!("<!-- differential:finding {finding} -->")
}

/// A finding's body as it is sent: the text, a blank line, the marker.
pub fn with_marker(body: &str, finding: &str) -> String {
    format!("{}\n\n{}", body.trim_end(), marker(finding))
}

/// A fetched body, split into what is shown and which finding wrote it.
pub fn strip_marker(body: &str) -> (String, Option<String>) {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\s*<!-- differential:finding ([0-9a-f]+) -->\s*").expect("a literal")
    });
    match re.captures(body) {
        Some(c) => {
            let id = c[1].to_string();
            (re.replace(body, "").trim_end().to_string(), Some(id))
        }
        None => (body.to_string(), None),
    }
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

    /// Whether this thread holds `finding`, published from here: by the
    /// address the publish recorded, or by the marker in a comment's body,
    /// which survives a publish whose answer was lost.
    pub fn is_twin_of(&self, finding: &Finding) -> bool {
        if let Some(up) = &finding.upstream
            && (up.thread == self.id || self.comments.iter().any(|c| c.id == up.comment))
        {
            return true;
        }
        self.published_here(&finding.id).is_some()
    }

    /// The comment in this thread that a finding's publish made, if any.
    pub fn published_here(&self, finding: &str) -> Option<&RemoteComment> {
        self.comments
            .iter()
            .find(|c| c.finding.as_deref() == Some(finding))
    }
}

/// A comment on the forge that is the reader's: by author, by marker, or by
/// the address a publish recorded. What the reviewer's `c` edits and `dd`
/// deletes; `ReviewSession::own_comment` is the one place that decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnComment {
    pub thread: String,
    pub comment: String,
    /// The local record, when one is linked.
    pub finding: Option<String>,
    pub body: String,
    /// `file:lines`, for a prompt.
    pub at: String,
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
    /// The same line's number on the other side, when the line is unchanged
    /// and so exists on both. GitLab positions an unchanged line by both
    /// numbers; a changed line has one.
    pub other_line: Option<u32>,
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

/// What an adapter's publish brought back.
///
/// `Err` from `Forge::publish` means nothing left the machine. Once anything
/// has, the adapter returns `Ok` with what it knows: the comments it can name,
/// the threads if it fetched them on the way, and the error that stopped it,
/// so the caller records what landed before it says what failed. A publish
/// that lost track of its own comments would send them again.
#[derive(Debug, Default)]
pub struct Sent {
    pub published: Vec<Published>,
    /// The threads, when the adapter had to fetch them anyway.
    pub threads: Option<Vec<RemoteThread>>,
    /// What stopped the adapter after something had already gone up.
    pub failed: Option<ForgeError>,
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
    /// The request's commits are not in this clone, and a fetch of its refs
    /// did not bring them. The message carries the command a reader could
    /// try by hand.
    #[error(
        "{noun} {id} needs commits this clone does not have, and fetching did not bring them; try\n    {hint}\nby hand"
    )]
    NotFetched {
        noun: &'static str,
        id: String,
        hint: String,
    },
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
    /// The request's head is not the one this review was built on: someone
    /// pushed. Both forges refuse a comment against another commit, so
    /// nothing was sent.
    #[error("the {noun} moved to {at} since this review was built; open it again")]
    HeadMoved { noun: &'static str, at: String },
}

/// The forge, as the domain needs it. `dyn`: which forge a repository is on
/// is a run-time answer, like which model groups (ADR 0020, 0029).
pub trait Forge: Send + Sync {
    fn kind(&self) -> ForgeKind;

    /// The login the tool is signed in as. A comment by this author is the
    /// reader's, marker or not.
    fn whoami(&self) -> Result<String, ForgeError>;

    /// The request with this id, or the current branch's when `None`.
    fn request(&self, id: Option<&str>) -> Result<Request, ForgeError>;

    /// Every review thread on the request, unplaced (`anchor: None`).
    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError>;

    /// Send one batch. Comments against `req.head`; the caller has already
    /// checked that is the review's head. `Err` only while nothing has left
    /// the machine; after that, `Ok(Sent)` with a `failed`.
    fn publish(&self, req: &Request, batch: &Batch) -> Result<Sent, ForgeError>;

    fn set_resolved(&self, req: &Request, thread: &str, resolved: bool) -> Result<(), ForgeError>;

    /// Rewrite a comment this reader published. The body arrives with its
    /// marker, as it was sent.
    fn edit_comment(
        &self,
        req: &Request,
        thread: &str,
        comment: &str,
        body: &str,
    ) -> Result<(), ForgeError>;

    /// Remove a comment this reader published.
    fn delete_comment(&self, req: &Request, thread: &str, comment: &str) -> Result<(), ForgeError>;
}

// ------------------------------------------------------------------ placing

/// One side of a hunk as `(first line, lines)`, in that side's numbering. The
/// three places that need it used to spell it out, one `if` each.
fn side_range(h: &schema::HunkEntry, old: bool) -> (u32, u32) {
    if old {
        (h.old_start.max(1), h.old_count)
    } else {
        (h.new_start.max(1), h.new_count)
    }
}

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
        // Exact: a hunk whose changed lines on this side hold the last line.
        let holds = |h: &&schema::HunkEntry| {
            let (s, n) = side_range(h, old);
            n > 0 && line >= s && line < s.saturating_add(n)
        };
        // Otherwise the nearest hunk in the file on this side.
        let distance = |h: &schema::HunkEntry| -> u32 {
            let (s, n) = side_range(h, old);
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
        let (s, n) = side_range(h, old);
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
            let (s, _) = side_range(h, side == "old");
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
const REQUEST_CONTEXT: u32 = 3;

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
    line_rule: Option<u32>,
) -> PublishPlan {
    let mut out = PublishPlan::default();
    // A finding a thread already carries is on the request, whatever its
    // record says: a publish whose answer was lost must not send it twice.
    for f in findings.iter().filter(|f| {
        f.status == FindingStatus::Open
            && f.upstream.is_none()
            && !threads.iter().any(|t| t.published_here(&f.id).is_some())
    }) {
        if let Some(thread_id) = &f.reply_to {
            match threads.iter().find(|t| &t.id == thread_id) {
                Some(t) => out.batch.replies.push(NewReply {
                    finding: f.id.clone(),
                    thread: t.id.clone(),
                    root_comment: t.root().map(|c| c.id.clone()).unwrap_or_default(),
                    body: with_marker(&f.body, &f.id),
                }),
                None => out
                    .excluded
                    .push(excluded(f, "its thread is no longer on the request")),
            }
            continue;
        }
        if let Some(context) = line_rule
            && !in_request_diff(doc, &f.anchor, context)
        {
            out.excluded.push(excluded(
                f,
                &format!("outside the request's diff: more than {context} lines from a change"),
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
            other_line: other_side_line(
                doc,
                &f.anchor.file,
                &f.anchor.side,
                f.anchor.end_line.max(f.anchor.line),
            ),
            body: with_marker(&f.body, &f.id),
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

/// The number an unchanged line has on the other side, or `None` for a
/// changed line, which exists on one side only.
///
/// Every hunk that ends before the line shifts the other side's numbering by
/// its own imbalance; the nearest such hunk carries the whole shift, since
/// its end already accounts for every hunk before it.
pub fn other_side_line(
    doc: &schema::PlanDocument,
    file: &str,
    side: &str,
    line: u32,
) -> Option<u32> {
    let old = side == "old";
    let in_file = || doc.hunks.iter().filter(|h| h.file == file);
    if in_file().any(|h| {
        let (s, n) = side_range(h, old);
        n > 0 && line >= s && line < s.saturating_add(n)
    }) {
        return None;
    }
    let shift = in_file()
        .map(|h| (side_range(h, old), side_range(h, !old)))
        .filter(|((s, n), _)| s.saturating_add(*n) <= line)
        .max_by_key(|((s, _), _)| *s)
        .map(|((s, n), (os, on))| i64::from(os + on) - i64::from(s + n))
        .unwrap_or(0);
    u32::try_from(i64::from(line) + shift).ok()
}

/// Whether both ends of `a` sit inside the request's diff of its file, with
/// `context` lines around each hunk.
fn in_request_diff(doc: &schema::PlanDocument, a: &Anchor, context: u32) -> bool {
    let old = a.side == "old";
    let first = a.line;
    let last = a.end_line.max(a.line);
    doc.hunks.iter().filter(|h| h.file == a.file).any(|h| {
        let (s, n) = side_range(h, old);
        let lo = s.saturating_sub(context);
        let hi = s
            .saturating_add(n)
            .saturating_sub(1)
            .saturating_add(context);
        first >= lo && last <= hi
    })
}

/// The range a request reviews: the merge base of its target branch's tip
/// and its head, to its head. That is the diff the request page shows.
///
/// When either commit is not local, the request's refs are fetched from
/// `origin` — the target branch and the forge's own ref for the head — and
/// the check is made again. Only a commit still missing after that is
/// `NotFetched`, with the line a reader could run by hand (ADR 0029, decision
/// 4 as reversed by the author).
pub fn source_for<G: Ancestry + RangeResolver + Fetcher>(
    git: &G,
    req: &Request,
) -> Result<ReviewSource, crate::EngineError> {
    let have = |sha: &str| git.commit_of(sha).map(|c| c.is_some());
    if !have(&req.head)? || !have(&req.base_tip)? {
        git.fetch("origin", &[&req.base_ref, &req.kind.head_ref(&req.id)])?;
    }
    if !have(&req.head)? || !have(&req.base_tip)? {
        return Err(ForgeError::NotFetched {
            noun: req.kind.noun(),
            id: req.id.clone(),
            hint: req.fetch_hint("origin"),
        }
        .into());
    }
    let base = git.merge_base(&req.base_tip, &req.head)?;
    Ok(ReviewSource::request(
        base,
        req.head.clone(),
        req.kind.source_kind(),
        req.remote(),
    ))
}

/// Whether the forge still has the head this review was opened on. Both
/// forges reject a comment against any other commit, so this is the first
/// thing a publish checks and the batch is not built when it fails.
pub fn head_matches(req: &Request, review_head: &str) -> bool {
    req.head == review_head
}

/// What one publish brings back: the forge's record of each finding it
/// took, and the threads fetched afterwards so the twins can be shown.
///
/// The refetch is its own result. The comments are on the request the moment
/// the publish returned; a refetch that then fails must not read as nothing
/// having been sent, or the next publish sends it all again.
#[derive(Debug)]
pub struct PublishOutcome {
    pub published: Vec<Published>,
    pub threads: Result<Vec<RemoteThread>, ForgeError>,
    /// What stopped the adapter after part of the batch had gone up.
    pub failed: Option<ForgeError>,
}

/// The whole publish, forge side: ask the forge where the request is now,
/// refuse if it moved, send the batch, fetch the threads again.
///
/// One function so the reviewer and `dfr findings --post` cannot order these
/// differently. It runs on whichever thread the caller chooses; it touches
/// nothing of the review's.
pub fn publish(
    forge: &dyn Forge,
    req: &Request,
    review_head: &str,
    batch: &Batch,
) -> Result<PublishOutcome, ForgeError> {
    let fresh = forge.request(Some(&req.id))?;
    if !head_matches(&fresh, review_head) {
        return Err(ForgeError::HeadMoved {
            noun: req.kind.noun(),
            at: fresh.head.get(..12).unwrap_or(&fresh.head).to_string(),
        });
    }
    let sent = forge.publish(req, batch)?;
    // The adapter may have fetched the threads on its way; one round of
    // pages, not two.
    let threads = match sent.threads {
        Some(threads) => Ok(threads),
        None => forge.threads(req),
    };
    Ok(PublishOutcome {
        published: sent.published,
        threads,
        failed: sent.failed,
    })
}
