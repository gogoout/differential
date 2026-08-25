//! Git subprocess runner. Bytes in, bytes out — UTF-8 decoding happens only at
//! display boundaries (ADR 0002). Plumbing commands only (ADR 0011).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::EngineError;
use crate::ports;

#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Open the repository containing `dir`.
    pub fn open(dir: &Path) -> Result<Self, EngineError> {
        let probe = Repo {
            root: dir.to_path_buf(),
        };
        let out = probe.run(["rev-parse", "--show-toplevel"], None)?;
        let root = PathBuf::from(String::from_utf8_lossy(trim_newline(&out)).into_owned());
        Ok(Repo { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run a git command with raw byte i/o. Non-zero exit is an error carrying
    /// the command line and stderr.
    ///
    /// PRIVATE, and the point of the ports (ADR 0020): domain code cannot spell
    /// an arbitrary git command, so each consumer's bounds are an honest
    /// statement of what it touches.
    fn run<I, S>(&self, args: I, stdin: Option<&[u8]>) -> Result<Vec<u8>, EngineError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_env(args, stdin, &[])
    }

    /// Like `run`, with extra environment variables (e.g. GIT_INDEX_FILE).
    fn run_env<I, S>(
        &self,
        args: I,
        stdin: Option<&[u8]>,
        env: &[(&str, &OsStr)],
    ) -> Result<Vec<u8>, EngineError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Command::new("git");
        // Belt and braces on top of plumbing: no color, no external diff drivers,
        // no path quoting surprises.
        cmd.arg("-c")
            .arg("core.quotepath=false")
            .args(args)
            .current_dir(&self.root)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| EngineError::GitSpawn { source: e })?;
        if let Some(data) = stdin {
            use std::io::Write;
            let mut pipe = child.stdin.take().expect("stdin was requested");
            pipe.write_all(data)
                .map_err(|e| EngineError::GitSpawn { source: e })?;
            // pipe drops here, closing stdin
        }
        let out = child
            .wait_with_output()
            .map_err(|e| EngineError::GitSpawn { source: e })?;

        if !out.status.success() {
            return Err(EngineError::GitCommand {
                command: describe(&cmd),
                code: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr[..out.stderr.len().min(800)])
                    .into_owned(),
            });
        }
        Ok(out.stdout)
    }

    /// Blob content at `rev:path`. `Ok(None)` when the path does not exist at
    /// that revision; any other failure is a real error.
    fn blob(&self, rev: &str, path: &[u8]) -> Result<Option<Vec<u8>>, EngineError> {
        let spec = spec_os(rev, path);
        // Distinguish "absent" from "broken" instead of eating every failure.
        let exists = Command::new("git")
            .args(["cat-file", "-e"])
            .arg(&spec)
            .current_dir(&self.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| EngineError::GitSpawn { source: e })?;
        if !exists.success() {
            return Ok(None);
        }
        Ok(Some(self.run(
            [OsStr::new("cat-file"), OsStr::new("blob"), spec.as_os_str()],
            None,
        )?))
    }

    /// Fully resolve a revision to a commit sha.
    fn rev_parse(&self, rev: &str) -> Result<String, EngineError> {
        let out = self.run(
            ["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
            None,
        )?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }

    /// Resolve to a commit sha, or accept a raw tree oid — the endpoints of
    /// an uncommitted-state review are synthesized trees (ADR 0017), and
    /// everything downstream of resolution is tree-safe.
    fn rev_parse_commit_or_tree(&self, rev: &str) -> Result<String, EngineError> {
        self.rev_parse(rev)
            .or_else(|_| self.rev_parse_raw(&format!("{rev}^{{tree}}")))
    }

    /// Resolve any rev expression (tree, `X^{tree}`, blob spec) to an object id.
    fn rev_parse_raw(&self, expr: &str) -> Result<String, EngineError> {
        let out = self.run(["rev-parse", "--verify", expr], None)?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<String, EngineError> {
        let out = self.run(["merge-base", a, b], None)?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }

    /// The shared git directory (worktree-safe). Per-repo state such as the
    /// grouping cache lives under `<common-dir>/differential/`.
    fn common_dir(&self) -> Result<PathBuf, EngineError> {
        let out = self.run(["rev-parse", "--git-common-dir"], None)?;
        let p = PathBuf::from(String::from_utf8_lossy(trim_newline(&out)).into_owned());
        Ok(if p.is_absolute() {
            p
        } else {
            self.root.join(p)
        })
    }
}

// ------------------------------------------------------------------ ports
//
// `Repo` is the ONLY implementation of these, and ADR 0020 forbids a second —
// invariants 1-4 compare the engine against git's own answer, so a fake git
// would compare the fake with the fake.

impl ports::ObjectReader for Repo {
    fn blob(&self, rev: &str, path: &[u8]) -> Result<Option<Vec<u8>>, EngineError> {
        Repo::blob(self, rev, path)
    }

    fn require_object(&self, oid: &str) -> Result<(), EngineError> {
        self.run(["cat-file", "-e", oid], None).map(|_| ())
    }
}

impl ports::ObjectWriter for Repo {
    fn write_blob(&self, content: &[u8]) -> Result<String, EngineError> {
        let out = self.run(["hash-object", "-w", "--stdin"], Some(content))?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }
}

impl ports::RangeResolver for Repo {
    fn merge_base(&self, a: &str, b: &str) -> Result<String, EngineError> {
        Repo::merge_base(self, a, b)
    }

    fn resolve_endpoint(&self, rev: &str) -> Result<String, EngineError> {
        self.rev_parse_commit_or_tree(rev)
    }
}

impl ports::TreeResolver for Repo {
    fn tree_of(&self, rev: &str) -> Result<String, EngineError> {
        self.rev_parse_raw(&format!("{rev}^{{tree}}"))
    }
}

impl ports::DiffSource for Repo {
    // FROZEN ARGV — see the trait docs. Add a method, never edit one.
    fn raw_records(&self, base: &str, head: &str) -> Result<Vec<u8>, EngineError> {
        self.run(
            [
                "diff-tree",
                "-r",
                "-z",
                "--raw",
                "--full-index",
                "--no-renames",
                base,
                head,
            ],
            None,
        )
    }

    fn canonical_patch(&self, base: &str, head: &str) -> Result<Vec<u8>, EngineError> {
        self.run(
            [
                "diff-tree",
                "-r",
                "-U0",
                "--no-renames",
                "--no-color",
                "--no-ext-diff",
                base,
                head,
            ],
            None,
        )
    }

    fn rename_records(&self, base: &str, head: &str) -> Result<Vec<u8>, EngineError> {
        self.run(
            ["diff-tree", "-r", "-M", "-z", "--name-status", base, head],
            None,
        )
    }
}

impl ports::RecountSource for Repo {
    /// Spelled out here rather than delegating to `canonical_patch`, and the
    /// argv genuinely differs from it (no `--no-color --no-ext-diff`). Both
    /// facts are deliberate: invariant 4's independence is the whole point, so
    /// one edit to enumeration's flags must not move both sides of the
    /// comparison. Do not "tidy" these two into one.
    fn recount_patch(&self, from: &str, to: &str) -> Result<Vec<u8>, EngineError> {
        self.run(["diff-tree", "-r", "-U0", "--no-renames", from, to], None)
    }
}

impl ports::AttributeSource for Repo {
    fn check_attr(
        &self,
        attr: &str,
        paths: &[&[u8]],
    ) -> Result<Vec<ports::AttrValue>, EngineError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut stdin: Vec<u8> = Vec::new();
        for p in paths {
            stdin.extend_from_slice(p);
            stdin.push(0);
        }
        let out = self.run(["check-attr", "-z", "--stdin", attr], Some(&stdin))?;
        // -z output: path NUL attr NUL value NUL ...
        let fields: Vec<&[u8]> = out.split(|&b| b == 0).collect();
        Ok(fields
            .chunks_exact(3)
            .map(|triple| ports::AttrValue {
                path: triple[0].to_vec(),
                value: triple[2].to_vec(),
            })
            .collect())
    }
}

/// A scratch index and its temp directory, alive together.
///
/// Owning the `TempDir` is what removes the `let _keep = idx;` hazards the
/// callers used to need: `write_tree` borrows the session, so the index file
/// provably outlives every use of it.
pub struct ScratchIndex {
    repo: Repo,
    _dir: tempfile::TempDir,
    index: std::ffi::OsString,
}

impl ScratchIndex {
    /// A path that does NOT exist yet: git treats an existing empty file as a
    /// corrupt index, so hand it a fresh name inside a temp dir rather than a
    /// pre-created `NamedTempFile`.
    fn open(repo: &Repo) -> Result<Self, EngineError> {
        let dir = tempfile::TempDir::new().map_err(|e| EngineError::GitSpawn { source: e })?;
        let index = dir.path().join("index").into_os_string();
        Ok(ScratchIndex {
            repo: repo.clone(),
            _dir: dir,
            index,
        })
    }

    fn env(&self) -> [(&str, &OsStr); 1] {
        [("GIT_INDEX_FILE", self.index.as_os_str())]
    }
}

impl ports::TreeBuilder for Repo {
    type Session = ScratchIndex;

    fn begin_from_tree(&self, tree_ish: &str) -> Result<ScratchIndex, EngineError> {
        let idx = ScratchIndex::open(self)?;
        self.run_env(["read-tree", tree_ish], None, &idx.env())?;
        Ok(idx)
    }

    fn begin_from_current_index(&self) -> Result<ScratchIndex, EngineError> {
        // "<mode> <oid> <stage>\t<path>" records — exactly the second input
        // format `update-index --index-info` accepts, so the seed is a byte
        // pipe with both ends inside this adapter.
        let entries = self.run(["ls-files", "-s", "-z"], None)?;
        for record in entries.split(|&b| b == 0) {
            let meta = record.split(|&b| b == b'\t').next().unwrap_or(record);
            if meta.ends_with(b" 1") || meta.ends_with(b" 2") || meta.ends_with(b" 3") {
                return Err(EngineError::Range(
                    "index has unmerged entries — resolve conflicts before reviewing \
                     uncommitted changes"
                        .into(),
                ));
            }
        }
        let idx = ScratchIndex::open(self)?;
        if !entries.is_empty() {
            self.run_env(
                ["update-index", "-z", "--index-info"],
                Some(&entries),
                &idx.env(),
            )?;
        }
        Ok(idx)
    }
}

impl ports::IndexSession for ScratchIndex {
    fn stage(&mut self, entries: &[ports::IndexEntry]) -> Result<(), EngineError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut feed: Vec<u8> = Vec::new();
        for e in entries {
            feed.extend_from_slice(&index_record(e));
            feed.push(0);
        }
        self.repo
            .run_env(
                ["update-index", "-z", "--index-info"],
                Some(&feed),
                &self.env(),
            )
            .map(|_| ())
    }

    fn stage_from_worktree(&mut self, nul_paths: &[u8]) -> Result<(), EngineError> {
        if nul_paths.is_empty() {
            return Ok(());
        }
        self.repo
            .run_env(
                ["update-index", "--add", "--remove", "-z", "--stdin"],
                Some(nul_paths),
                &self.env(),
            )
            .map(|_| ())
    }

    fn write_tree(&self) -> Result<String, EngineError> {
        let out = self.repo.run_env(["write-tree"], None, &self.env())?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }
}

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// One `update-index --index-info` record: `<mode> <oid>\t<path>`, removal
/// spelled as mode 0. Git wire format, so it lives with the adapter.
fn index_record(e: &ports::IndexEntry) -> Vec<u8> {
    let (mode, oid, path) = match e {
        ports::IndexEntry::Set { mode, oid, path } => (mode.as_str(), oid.as_str(), path),
        ports::IndexEntry::Remove { path } => ("0", ZERO_OID, path),
    };
    let mut line = format!("{mode} {oid}\t").into_bytes();
    line.extend_from_slice(path);
    line
}

impl ports::WorkingCopy for Repo {
    fn tracked_paths(&self) -> Result<Vec<u8>, EngineError> {
        self.run(["ls-files", "-z"], None)
    }

    fn untracked_paths(&self) -> Result<Vec<u8>, EngineError> {
        self.run(["ls-files", "--others", "--exclude-standard", "-z"], None)
    }
}

impl ports::CommitWriter for Repo {
    fn commit_tree(
        &self,
        tree: &str,
        parent: &str,
        message: &[u8],
        identity: ports::CommitIdentity<'_>,
    ) -> Result<String, EngineError> {
        let env: [(&str, &OsStr); 4] = [
            ("GIT_AUTHOR_NAME", OsStr::new(identity.name)),
            ("GIT_AUTHOR_EMAIL", OsStr::new(identity.email)),
            ("GIT_COMMITTER_NAME", OsStr::new(identity.name)),
            ("GIT_COMMITTER_EMAIL", OsStr::new(identity.email)),
        ];
        let out = self.run_env(
            ["commit-tree", tree, "-p", parent, "-F", "-"],
            Some(message),
            &env,
        )?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }
}

impl ports::RefWriter for Repo {
    fn update_ref(&self, name: &str, target: &str) -> Result<(), EngineError> {
        self.run(["update-ref", name, target], None).map(|_| ())
    }
}

impl ports::CommitHistory for Repo {
    fn has_commits(&self) -> bool {
        self.rev_parse("HEAD").is_ok()
    }

    fn recent_commits(
        &self,
        from: &str,
        max: usize,
    ) -> Result<Vec<ports::CommitSummary>, EngineError> {
        let raw = self.run(
            [
                "rev-list",
                &format!("--max-count={max}"),
                "--no-commit-header",
                "--format=%H%x00%h%x00%s%x00%an",
                from,
            ],
            None,
        )?;
        Ok(parse_rev_list(&raw))
    }

    fn refs_by_commit(&self) -> HashMap<String, Vec<String>> {
        self.run(
            [
                "for-each-ref",
                "--format=%(objectname)%00%(*objectname)%00%(refname:short)",
                "refs/heads",
                "refs/tags",
                "refs/remotes",
            ],
            None,
        )
        .map(|out| parse_refs(&out))
        .unwrap_or_default()
    }
}

impl ports::RepoLayout for Repo {
    fn common_dir(&self) -> Result<PathBuf, EngineError> {
        Repo::common_dir(self)
    }

    fn work_root(&self) -> &Path {
        &self.root
    }
}

/// `rev-list --no-commit-header --format=%H%x00%h%x00%s%x00%an` output: one
/// record per line, fields NUL-separated (subjects are single-line by
/// definition, so the line split is safe; bytes decode lossily).
fn parse_rev_list(bytes: &[u8]) -> Vec<ports::CommitSummary> {
    bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let fields: Vec<String> = line
                .split(|&b| b == 0)
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            match fields.as_slice() {
                [sha, short, subject, author] => Some(ports::CommitSummary {
                    sha: sha.clone(),
                    short: short.clone(),
                    subject: subject.clone(),
                    author: author.clone(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// `for-each-ref --format='%(objectname)%00%(*objectname)%00%(refname:short)'`
/// output → sha -> ref names. Plumbing, so unaffected by log.decorate config;
/// annotated tags carry the peeled commit in the second field.
///
/// NOTE the escape: for-each-ref's format language spells NUL `%00`. `%x00`
/// is a rev-list/log spelling and passes through as literal text here, which
/// is exactly how this silently produced no decorations at all.
fn parse_refs(bytes: &[u8]) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for line in bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let fields: Vec<String> = line
            .split(|&b| b == 0)
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        let [oid, peeled, name] = fields.as_slice() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        // An annotated tag's own object id is the tag; the commit it points at
        // is the peeled one.
        let target = if peeled.is_empty() { oid } else { peeled };
        out.entry(target.clone()).or_default().push(name.clone());
    }
    out
}

pub(crate) fn spec_os(rev: &str, path: &[u8]) -> std::ffi::OsString {
    // rev:path with the path kept as raw bytes (unix).
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut v = rev.as_bytes().to_vec();
        v.push(b':');
        v.extend_from_slice(path);
        std::ffi::OsString::from_vec(v)
    }
    #[cfg(not(unix))]
    {
        let mut s = String::from(rev);
        s.push(':');
        s.push_str(&String::from_utf8_lossy(path));
        std::ffi::OsString::from(s)
    }
}

fn trim_newline(b: &[u8]) -> &[u8] {
    let mut end = b.len();
    while end > 0 && (b[end - 1] == b'\n' || b[end - 1] == b'\r') {
        end -= 1;
    }
    &b[..end]
}

fn describe(cmd: &Command) -> String {
    let mut s = String::from("git");
    for a in cmd.get_args() {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
        if s.len() > 200 {
            s.push_str(" …");
            break;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{parse_refs, parse_rev_list};

    #[test]
    fn parses_nul_separated_records() {
        let raw = b"aaaa\0a1\0fix the thing\0Alice\nbbbb\0b2\0subject with \xe2\x9c\x93 unicode\0B\xc3\xb6b\n";
        let entries = parse_rev_list(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sha, "aaaa");
        assert_eq!(entries[0].short, "a1");
        assert_eq!(entries[0].subject, "fix the thing");
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[1].subject, "subject with ✓ unicode");
        assert_eq!(entries[1].author, "Böb");
    }

    #[test]
    fn tolerates_empty_and_malformed_lines() {
        assert!(parse_rev_list(b"").is_empty());
        assert!(parse_rev_list(b"\n\n").is_empty());
        assert!(parse_rev_list(b"only-two\0fields\n").is_empty());
    }

    #[test]
    fn refs_group_by_commit_and_peel_annotated_tags() {
        // Lightweight ref: own oid is the commit. Annotated tag: the peeled
        // field carries the commit.
        let raw = b"aaaa\0\0main\naaaa\0\0origin/main\ntagobj\0aaaa\0v1.0\nbbbb\0\0feature\n";
        let refs = parse_refs(raw);
        assert_eq!(
            refs.get("aaaa").unwrap(),
            &vec![
                "main".to_string(),
                "origin/main".to_string(),
                "v1.0".to_string()
            ]
        );
        assert_eq!(refs.get("bbbb").unwrap(), &vec!["feature".to_string()]);
        // The tag object's own id is never a key.
        assert!(!refs.contains_key("tagobj"));
    }

    #[test]
    fn refs_tolerate_junk() {
        assert!(parse_refs(b"").is_empty());
        assert!(parse_refs(b"\n\n").is_empty());
        assert!(parse_refs(b"two\0fields\n").is_empty());
        // An empty ref name is skipped rather than stored.
        assert!(parse_refs(b"aaaa\0\0\n").is_empty());
    }
}
