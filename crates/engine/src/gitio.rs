//! Git subprocess runner. Bytes in, bytes out — UTF-8 decoding happens only at
//! display boundaries (ADR 0002). Plumbing commands only (ADR 0011).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::EngineError;

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
    pub fn run<I, S>(&self, args: I, stdin: Option<&[u8]>) -> Result<Vec<u8>, EngineError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_env(args, stdin, &[])
    }

    /// Like `run`, with extra environment variables (e.g. GIT_INDEX_FILE).
    pub fn run_env<I, S>(
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
    pub fn blob(&self, rev: &str, path: &[u8]) -> Result<Option<Vec<u8>>, EngineError> {
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
    pub fn rev_parse(&self, rev: &str) -> Result<String, EngineError> {
        let out = self.run(
            ["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
            None,
        )?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }

    /// Resolve any rev expression (tree, `X^{tree}`, blob spec) to an object id.
    pub fn rev_parse_raw(&self, expr: &str) -> Result<String, EngineError> {
        let out = self.run(["rev-parse", "--verify", expr], None)?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String, EngineError> {
        let out = self.run(["merge-base", a, b], None)?;
        Ok(String::from_utf8_lossy(trim_newline(&out)).into_owned())
    }

    /// The shared git directory (worktree-safe). Per-repo state such as the
    /// grouping cache lives under `<common-dir>/differential/`.
    pub fn common_dir(&self) -> Result<PathBuf, EngineError> {
        let out = self.run(["rev-parse", "--git-common-dir"], None)?;
        let p = PathBuf::from(String::from_utf8_lossy(trim_newline(&out)).into_owned());
        Ok(if p.is_absolute() {
            p
        } else {
            self.root.join(p)
        })
    }
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
