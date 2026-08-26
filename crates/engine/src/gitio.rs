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

    /// Run a git command for its exit status, which the caller treats as the
    /// answer rather than as success or failure.
    ///
    /// Separate from `run` rather than loosening it: `run` turning a non-zero
    /// exit into an error is what makes every other call site safe by default.
    fn run_status<I, S>(&self, args: I) -> Result<std::process::ExitStatus, EngineError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new("git")
            .arg("-c")
            .arg("core.quotepath=false")
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| EngineError::GitSpawn { source: e })
    }

    /// Blob content at `rev:path`. `Ok(None)` when the path does not exist at
    /// that revision; any other failure is a real error.
    fn blob(&self, rev: &str, path: &[u8]) -> Result<Option<Vec<u8>>, EngineError> {
        Ok(self.blobs(&[(rev, path)])?.pop().flatten())
    }

    /// Several blobs, one process.
    ///
    /// `--batch` states absence as the word "missing" rather than as an exit
    /// code, so the existence probe this used to spawn first is not merely
    /// saved but replaced by something more explicit. And it reads a LIST from
    /// stdin, which is what makes the batch free: the protocol was always
    /// there, only the loop is new. A spawn costs milliseconds and the reviewer
    /// reads two blobs for every file it draws (ADR 0021).
    ///
    /// The specs go in on stdin, which also keeps paths as raw bytes without an
    /// `OsString` detour. `-z` is not optional: git paths may contain a
    /// newline, and line-delimited input splits such a path into two specs that
    /// both come back "missing" — a wrong answer with no error attached to it.
    fn blobs(&self, specs: &[(&str, &[u8])]) -> Result<Vec<Option<Vec<u8>>>, EngineError> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        let wire: Vec<Vec<u8>> = specs
            .iter()
            .map(|(rev, path)| {
                let mut s = rev.as_bytes().to_vec();
                s.push(b':');
                s.extend_from_slice(path);
                s
            })
            .collect();
        let mut stdin = Vec::new();
        for s in &wire {
            stdin.extend_from_slice(s);
            stdin.push(0);
        }
        let out = self.run(["cat-file", "--batch", "-z"], Some(&stdin))?;
        parse_batch_blobs(&out, &wire).map_err(|msg| EngineError::GitCommand {
            command: format!("cat-file --batch ({} specs)", specs.len()),
            code: None,
            stderr: msg,
        })
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

    fn blobs(&self, specs: &[(&str, &[u8])]) -> Result<Vec<Option<Vec<u8>>>, EngineError> {
        Repo::blobs(self, specs)
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

    /// `diff-index --quiet HEAD --`: exit 1 means differences, which is the
    /// answer rather than a failure — hence `run_status` instead of `run`.
    fn has_tracked_changes(&self) -> Result<bool, EngineError> {
        let status = self.run_status(["diff-index", "--quiet", "HEAD", "--"])?;
        Ok(!status.success())
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

/// A `cat-file --batch -z` stream: one response per spec, in the order asked.
///
/// Each is `<oid> SP <type> SP <size> LF <contents> LF`, or
/// `<spec> SP "missing" LF` when the path is not there. `-z` changes only the
/// INPUT framing, so headers are still LF-terminated.
///
/// Telling the two apart needs the specs, because both near-misses are real:
///
/// - a missing response echoes the spec, and a git path may contain a newline,
///   so scanning for the next LF does not reliably find the end of the line;
/// - a blob whose contents happen to END with " missing" looks, from the tail,
///   exactly like an absent one.
///
/// Knowing the spec settles both: absence is an EXACT match for
/// `<spec> " missing\n"` at the current position, and everything else must be a
/// header — a hex oid, a type, a number — or the repository is broken, which is
/// not the same as a file being absent and must never render as one.
fn parse_batch_blobs(out: &[u8], specs: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>, String> {
    const MISSING: &[u8] = b" missing\n";
    let mut at = 0usize;
    let mut got = Vec::with_capacity(specs.len());

    for spec in specs {
        let rest = out
            .get(at..)
            .ok_or_else(|| "output ended early".to_string())?;
        if rest.starts_with(spec) && rest[spec.len()..].starts_with(MISSING) {
            got.push(None);
            at += spec.len() + MISSING.len();
            continue;
        }
        let end = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| "no header line in cat-file output".to_string())?;
        let header = String::from_utf8_lossy(&rest[..end]).into_owned();
        let fields: Vec<&str> = header.split(' ').collect();
        if fields.len() != 3
            || fields[0].is_empty()
            || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(format!("unrecognised cat-file response: {header}"));
        }
        if fields[1] != "blob" {
            return Err(format!("{header}: not a blob"));
        }
        let size: usize = fields[2]
            .parse()
            .map_err(|_| format!("{header}: unparsable size"))?;
        let body = &rest[end + 1..];
        if body.len() < size {
            return Err(format!(
                "{header}: body is {} bytes, header said {size}",
                body.len()
            ));
        }
        // git writes an LF after the body. Check it rather than assume it: on
        // a malformed stream the alternative is to walk on from the wrong
        // offset and read the next response as this one's neighbour, which
        // desyncs everything after it without ever reporting a fault.
        if body.get(size) != Some(&b'\n') {
            return Err(format!("{header}: body is not LF-terminated"));
        }
        got.push(Some(body[..size].to_vec()));
        // header LF + body + the LF git writes after it
        at += end + 1 + size + 1;
    }
    Ok(got)
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
    use super::{parse_batch_blobs, parse_refs, parse_rev_list};

    /// The absent/broken distinction the old two-spawn probe existed for, now
    /// carried by `--batch`'s own vocabulary — and read against the specs that
    /// were sent, which is what makes both near-misses decidable.
    #[test]
    fn batch_blob_separates_absent_from_broken() {
        let oid = "e".repeat(40);
        let one = |spec: &str| vec![spec.as_bytes().to_vec()];

        let found = format!("{oid} blob 6\nhello\n\n");
        assert_eq!(
            parse_batch_blobs(found.as_bytes(), &one("HEAD:a"))
                .unwrap()
                .remove(0)
                .as_deref(),
            Some(&b"hello\n"[..]),
            "the body is exactly the declared size, trailing LF excluded"
        );

        // A path that is not there is absence, not failure.
        assert_eq!(
            parse_batch_blobs(b"HEAD:nope missing\n", &one("HEAD:nope")).unwrap(),
            vec![None]
        );
        // A blob whose CONTENT ends in " missing" produces exactly the bytes an
        // absent path produces at the end of the response.
        let ends_missing = format!("{oid} blob 9\nx missing\n");
        assert_eq!(
            parse_batch_blobs(ends_missing.as_bytes(), &one("HEAD:a"))
                .unwrap()
                .remove(0)
                .as_deref(),
            Some(&b"x missing"[..]),
            "a blob ending in \" missing\" must not be read as absent"
        );
        // A space in the path is not positional: the spec is matched whole.
        assert_eq!(
            parse_batch_blobs(b"HEAD:a b.txt missing\n", &one("HEAD:a b.txt")).unwrap(),
            vec![None]
        );
        // Nor is a NEWLINE in one — which is the whole reason the input is
        // NUL-delimited, and the reason the spec has to be matched rather than
        // the next LF looked for.
        assert_eq!(
            parse_batch_blobs(b"HEAD:we\nird.txt missing\n", &one("HEAD:we\nird.txt")).unwrap(),
            vec![None]
        );

        // A tree or a commit at that path is a broken assumption, not a file.
        assert!(parse_batch_blobs(format!("{oid} tree 42\n").as_bytes(), &one("HEAD:a")).is_err());
        // A truncated body must never be mistaken for a short file.
        assert!(
            parse_batch_blobs(format!("{oid} blob 99\nshort\n").as_bytes(), &one("HEAD:a"))
                .is_err()
        );
        assert!(parse_batch_blobs(b"no newline at all", &one("HEAD:a")).is_err());
    }

    /// Many specs, one stream: each response has to be found by walking, since
    /// only the previous one's declared size says where the next begins.
    #[test]
    fn batch_blobs_walks_a_stream_of_responses() {
        let oid = "a".repeat(40);
        let specs = vec![
            b"HEAD:one".to_vec(),
            b"HEAD:gone".to_vec(),
            b"HEAD:two".to_vec(),
        ];
        let mut out = format!("{oid} blob 4\nabcd\n").into_bytes();
        out.extend_from_slice(b"HEAD:gone missing\n");
        out.extend_from_slice(format!("{oid} blob 2\nxy\n").as_bytes());

        assert_eq!(
            parse_batch_blobs(&out, &specs).unwrap(),
            vec![Some(b"abcd".to_vec()), None, Some(b"xy".to_vec()),],
            "answers come back in the order the specs were given"
        );

        // A stream that stops early is a broken repository, not three absences.
        assert!(parse_batch_blobs(&out[..10], &specs).is_err());

        // And one where the body is not LF-terminated where the header said it
        // would end: walking on from there would read the next response as this
        // one's neighbour and desync the rest without reporting a fault.
        let mut bad = format!("{oid} blob 4\nabcd").into_bytes();
        bad.extend_from_slice(b"XHEAD:gone missing\n");
        assert!(parse_batch_blobs(&bad, &specs[..2]).is_err());
    }

    /// Bytes in, bytes out: a blob that is not UTF-8 survives intact.
    #[test]
    fn batch_blob_is_byte_faithful() {
        let oid = "f".repeat(40);
        let mut raw = format!("{oid} blob 4\n").into_bytes();
        raw.extend_from_slice(&[0x00, 0xff, 0xfe, 0x0a, 0x0a]);
        assert_eq!(
            parse_batch_blobs(&raw, &[b"HEAD:a".to_vec()])
                .unwrap()
                .remove(0)
                .unwrap(),
            vec![0x00, 0xff, 0xfe, 0x0a]
        );
    }

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
