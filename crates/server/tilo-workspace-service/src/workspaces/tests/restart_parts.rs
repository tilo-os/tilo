//! A restart with a workspace's chain of a document's parts: how quickly it answers.
//!
//! A workspace's chain of parts is kept as documents of the workspace's own history, each
//! as large as the part it holds. A start reads them before it answers, so
//! it reads them side by side rather than one after another: the bound below is the point of
//! the test, and the chain is of the size a chain grows to.

use super::*;
use automerge::ReadDoc as _;

/// The app the chain belongs to.
const APP: &str = "so.tilo.notes";
/// How many parts the chain has.
const PARTS: usize = 26;
/// How large each part's document is.
const BYTES: usize = 512 * 1024;
/// How long a restart may take to open the chain and answer its first read.
const BOUND: std::time::Duration = std::time::Duration::from_secs(2);

/// Incompressible text, so a part's document is as large as it says.
fn text(bytes: usize) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut out = String::with_capacity(bytes);
    while out.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{state:016x}"));
    }
    out.truncate(bytes);
    out
}

#[test]
fn a_restart_with_a_large_chain_answers_within_two_seconds() {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut workspace =
        GovernedWorkspace::create(dir.path(), "Team", &cert, &key, None, SystemTime::now())
            .unwrap();
    let state = workspace.collaboration().clone();
    let chain = collab_types::document::DocumentId::parse(
        "derived:pages@derived:default@tab:0000000000000000",
    )
    .unwrap();
    // A document of its own, which a restart reads: nothing of the chain's.
    {
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "title", "Welcome").unwrap();
        let mut candidate = workspace.peer().clone();
        candidate
            .publication_mut()
            .publish_document(APP, "welcome", document.save(), &key, SystemTime::now())
            .unwrap();
        workspace
            .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
            .unwrap();
    }
    for index in 0..PARTS {
        let part = chain.part(u64::try_from(index).unwrap());
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "text", text(BYTES)).unwrap();
        let mut candidate = workspace.peer().clone();
        candidate
            .publication_mut()
            .publish_document(APP, part.as_str(), document.save(), &key, SystemTime::now())
            .unwrap();
        workspace
            .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
            .unwrap();
    }
    let path = workspace.path.clone();
    let held = std::fs::read_dir(path.with_extension("parts"))
        .unwrap()
        .count();
    assert!(held > 0, "the chain is kept in parts of its own");
    drop(workspace);

    // A hard stop: the next start opens what the parts hold and nothing more.
    let started = std::time::Instant::now();
    let reopened = GovernedWorkspace::open(path, None).unwrap();
    let reopened_in = started.elapsed();
    println!("a restart of {PARTS} parts of {BYTES} bytes opened in {reopened_in:?}");
    let started = std::time::Instant::now();
    let read = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(reopened.readable_document(APP, "welcome"))
        .unwrap()
        .unwrap();
    let answered = started.elapsed();
    assert_eq!(
        automerge::AutoCommit::load(&read)
            .unwrap()
            .get(automerge::ROOT, "title")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref(),
        Some("Welcome"),
        "the document read is the one asked for"
    );
    assert!(
        reopened.peer().holds_document(APP, chain.part(0).as_str()),
        "the chain's parts are held"
    );
    assert!(
        reopened_in < BOUND,
        "a restart of {PARTS} parts of {BYTES} bytes opened in {reopened_in:?}"
    );
    assert!(
        answered < BOUND,
        "the first answer took {answered:?} after a restart"
    );
}
