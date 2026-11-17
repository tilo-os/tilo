//! One session carries a device's writes to every workspace it joined, and each workspace is
//! answered for its own: a write one workspace holds does not keep another's from being
//! confirmed, and is itself never confirmed before the workspace's holder has it.
//!
//! A server writes to all of its workspaces over one session. A change it wrote to a document
//! of one workspace before that document landed waits there for it, as it must. While it
//! waited, no write of any other workspace was confirmed either: what asks whether a write is
//! saved asked about the whole session, so every workspace's answers waited on the one
//! document.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::{AutoCommit, ROOT, transaction::Transactable as _};
use collab_types::DocumentMutation;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Outbox as _, TryRecvError};
use tilo_test_env::within;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use super::support::{
    WORKSPACE, enrol, in_workspace, route, start_peer, wait_keyed, workspace, written,
};

/// The person's second workspace, where a write waits.
const ELSEWHERE: &str = "3c9e5b7a-41d2-4f6b-8a10-7e2f9d0c6b35";

/// The change `text` makes to `doc`, as the write that carries it: the document whole the
/// first time, what it changed after.
fn write(doc: &mut AutoCommit, text: &str) -> DocumentMutation {
    let before = doc.get_heads();
    doc.put(ROOT, text, true).unwrap();
    doc.commit();
    if before.is_empty() {
        DocumentMutation::Snapshot(doc.save())
    } else {
        DocumentMutation::Incremental(doc.save_after(&before))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_one_workspace_holds_keeps_no_other_workspaces_write_from_being_confirmed() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = super::support::start_host(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let elsewhere = tilo_server_protocol::WorkspaceId::parse(ELSEWHERE).unwrap();

    // One session in both of its person's workspaces.
    let page = start_peer(
        server.iroh(),
        browser.cert.clone(),
        server_device.cert.clone(),
        browser.key.clone(),
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    page.join_placed(ELSEWHERE, "Elsewhere").unwrap();
    in_workspace(&page, workspace()).await;
    in_workspace(&page, elsewhere).await;

    // Here, a note the server has.
    let here = route();
    let mut note = AutoCommit::new();
    written(&page, &here, &write(&mut note, "first")).await;

    // Elsewhere, a change to a document nobody founded yet: the page holds its keys and no
    // document, so the change waits for it.
    let there = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: ELSEWHERE.into(),
        document: "note-2".into(),
    };
    page.send(Cmd::OpenRoute(there.clone())).unwrap();
    wait_keyed(&page, &there).await;
    let mut unfounded = AutoCommit::new();
    let founding = write(&mut unfounded, "founded");
    let mut copy = unfounded.clone();
    page.write(there.clone(), &write(&mut copy, "written early"))
        .unwrap();

    // What is written here next is confirmed for this workspace, while that change waits.
    page.write(here.clone(), &write(&mut note, "second"))
        .unwrap();
    let confirmed = page.barrier_of(&[workspace()]).unwrap();
    within(
        "the write here to be confirmed while a change elsewhere waits for its document",
        confirmed,
    )
    .await
    .expect("the session answers")
    .expect("the server has the write here");

    // The change that waits is not confirmed, nor is the session as a whole: both were asked
    // before a later write here, which is confirmed again.
    let own = page.barrier_of(&[elsewhere]).unwrap();
    let whole = page.barrier().unwrap();
    page.write(here, &write(&mut note, "third")).unwrap();
    within(
        "a later write here to be confirmed",
        page.barrier_of(&[workspace()]).unwrap(),
    )
    .await
    .expect("the session answers")
    .expect("the server has the later write here");
    assert_eq!(
        own.try_recv(),
        Err(TryRecvError::Empty),
        "a change that waits for its document is not said to be saved"
    );
    assert_eq!(
        whole.try_recv(),
        Err(TryRecvError::Empty),
        "the session as a whole still holds a write"
    );

    // Another device founds the document; the change goes once it lands, and only then is it
    // confirmed.
    let founder = start_peer(
        server.iroh(),
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    founder.join_placed(ELSEWHERE, "Elsewhere").unwrap();
    in_workspace(&founder, elsewhere).await;
    written(&founder, &there, &founding).await;
    within("the change to be confirmed once its document landed", own)
        .await
        .expect("the session answers")
        .expect("the server has the change");
    within("the whole session to be confirmed", whole)
        .await
        .expect("the session answers")
        .expect("the server has every write");
    for peer in [founder, page] {
        peer.close();
    }
}
