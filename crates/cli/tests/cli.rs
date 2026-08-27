//! CLI argument contract: the range is optional only for `review` (which
//! opens the picker); every other subcommand still demands it.

use std::process::Command;

use tempfile::TempDir;

fn init_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let ok = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(ok.success());
    tmp
}

#[test]
fn non_review_commands_require_a_range() {
    let repo = init_repo();
    for sub in ["check", "stack", "findings"] {
        let out = Command::new(env!("CARGO_BIN_EXE_dfr"))
            .args([sub, "--repo"])
            .arg(repo.path())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "dfr {sub} without range");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("revision range is required"),
            "dfr {sub}: {stderr}"
        );
    }
}

/// `--summary` is a projection of the same store, not a second command. It has
/// to reach the CLI's argument surface, or the reviewer's footer names a flag
/// that does not exist.
#[test]
fn findings_offers_a_markdown_summary() {
    let out = Command::new(env!("CARGO_BIN_EXE_dfr"))
        .args(["findings", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--summary"), "{help}");
    assert!(help.contains("markdown"), "{help}");
}
