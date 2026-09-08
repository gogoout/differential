//! The GitHub adapter against the real forge. Ignored: it needs `gh` logged
//! in and this repository's remote, so it runs by hand, never in CI.
//!
//!     cargo test -p differential-engine --test forge_live -- --ignored --nocapture

use differential_engine::forge::Forge;
use differential_engine::forgeio::GhForge;
use differential_engine::gitio::Repo;

#[test]
#[ignore]
fn gh_reads_a_request_and_its_threads() {
    let repo = Repo::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let gh = GhForge::new(repo.root());
    // A merged request of this repository that carries review threads.
    let req = gh.request(Some("84")).unwrap();
    assert_eq!(req.id, "84");
    assert_eq!(req.head.len(), 40);
    println!("{req:#?}");
    let threads = gh.threads(&req).unwrap();
    assert!(!threads.is_empty(), "PR 84 has review threads");
    for t in &threads {
        println!(
            "{} {}:{:?} resolved={} outdated={} comments={} text={:?}",
            t.id,
            t.path,
            t.line,
            t.resolved,
            t.outdated,
            t.comments.len(),
            t.line_text
        );
    }
}
