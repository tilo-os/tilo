//! A host answers a member's request for the app surface a change was written
//! with, whichever version of the app that is: a member reading a record an
//! older version started decides its writes against that version's surface,
//! which it asks its workspace for by hash, as it asks for any content.
//!
//! The version a host never ran is not in its own apps: it fetches it from the
//! registry by hash, the way it fetched it to decide the change, and answers
//! with one copy however many members ask.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host stays live for the whole test"
)]

use neighbour_wire::NeighbourMsg;

const NOTES: &str = "so.tilo.notes";
/// The workspace the member asks in: any workspace the host governs.
const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// The surface of a version of the notes app this host does not run.
fn older_surface() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/notes/surface.toml"
    ))
    .unwrap()
}

/// What a member's device asks the workspace for.
fn want(requester: u64, hash: &str) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::RecordContentWant {
        requester,
        app_id: NOTES.into(),
        cluster_id: WORKSPACE.into(),
        hash: hash.into(),
    })
    .unwrap()
}

/// The content `frames` answer with, or `None` when the host said it keeps
/// none.
fn gathered(frames: &[Vec<u8>]) -> Option<Vec<u8>> {
    let mut whole = Vec::new();
    for frame in frames {
        match postcard::from_bytes(frame).unwrap() {
            NeighbourMsg::RecordContentPiece {
                requester,
                total,
                offset,
                mut bytes,
                ..
            } => {
                assert_eq!(requester, 42, "addressed to whoever asked");
                assert_eq!(offset, u64::try_from(whole.len()).unwrap());
                whole.append(&mut bytes);
                assert_eq!(total, u64::try_from(whole.len()).unwrap_or(u64::MAX));
            }
            NeighbourMsg::RecordContentLacks { .. } => return None,
            other => panic!("a piece of content, or word that the host keeps none: {other:?}"),
        }
    }
    Some(whole)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_answers_with_the_surface_of_an_app_version_it_never_ran() {
    // The surfaces it installs are the process's: no other test runs here.
    if !crate::common::in_own_process(
        "surface_content::a_host_answers_with_the_surface_of_an_app_version_it_never_ran",
    ) {
        return;
    }
    let surface = older_surface();
    let hash = blake3::hash(&surface).to_hex().to_string();
    let served = surface.clone();
    let name = hash.clone();
    let registry = test_http::Responder::handle(move |request| {
        Some(match request.target.as_str() {
            path if path == format!("/blobs/{name}") => {
                test_http::Reply::status(200).body(served.clone())
            }
            _ => test_http::Reply::status(404),
        })
    });
    tilo_room_host::surfaces::decide_receipt_from_registry(&registry.url());
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = tilo_room_host::workspaces::fixtures::governor();
    let host = tilo_room_host::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();

    // Answering fetches the surface, so it happens off the threads that serve
    // members.
    let asked = want(42, &hash);
    let answering = workspaces.clone();
    let frames = tokio::task::spawn_blocking(move || answering.content_answer(WORKSPACE, &asked))
        .await
        .unwrap();
    assert_eq!(
        gathered(&frames),
        Some(surface.clone()),
        "the surface the member asked for, whole"
    );
    let requested: Vec<String> = registry
        .drain_requests()
        .into_iter()
        .map(|request| request.target)
        .collect();
    assert!(
        requested
            .iter()
            .any(|path| path == &format!("/blobs/{hash}")),
        "the host fetched the version it never ran by hash: {requested:?}"
    );

    // It keeps what it fetched: the second member asking is answered without
    // asking the registry again.
    let asked = want(42, &hash);
    let answering = workspaces.clone();
    let frames = tokio::task::spawn_blocking(move || answering.content_answer(WORKSPACE, &asked))
        .await
        .unwrap();
    assert_eq!(
        gathered(&frames),
        Some(surface),
        "answered from what it kept"
    );
    assert!(registry.drain_requests().is_empty(), "no second fetch");

    // A hash the registry has, as far as the host's own apps go, keeps none:
    // the member is told so rather than left waiting.
    let absent = "0".repeat(64);
    let asked = want(42, &absent);
    let answering = workspaces.clone();
    let frames = tokio::task::spawn_blocking(move || answering.content_answer(WORKSPACE, &asked))
        .await
        .unwrap();
    assert_eq!(gathered(&frames), None, "not kept: the member is told so");

    host.shutdown().await;
}
