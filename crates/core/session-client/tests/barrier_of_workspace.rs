//! One session carries a device's writes to every workspace it joined, and each workspace is
//! answered for its own: a write one workspace holds does not keep another's from being
//! confirmed, and is itself never confirmed before the workspace's holder has it.
//!
//! A server writes to all of its workspaces over one session. A change it wrote to a document
//! of one workspace before that document landed waits there for it, as it must. While it
//! waited, no write of any other workspace was confirmed either: what asks whether a write is
//! saved asked about the whole session, so every workspace's answers waited on the one
//! document.
//!
//! Nor did anything say why the change stayed unconfirmed: the session says which document
//! it keeps writes of and what they wait for, to whoever asks of that workspace.
//!
//! Nor is a workspace answered all or nothing: whoever asks of the documents they wrote is
//! answered for those, so a change that waits for its document keeps no write to another
//! document of its workspace from being confirmed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Kept, Link, Outbox as _, Topic, TryRecvError, Waits};
use tilo_test_env::within;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use super::support::{
    WORKSPACE, enrol, in_workspace, person_of, route, set_field, start_host as start_server,
    start_peer, wait_keyed, workspace, written,
};

/// The person's second workspace, where a write waits.
const ELSEWHERE: &str = "3c9e5b7a-41d2-4f6b-8a10-7e2f9d0c6b35";

/// The change `text` makes to `doc` as `writer`'s, as the write that carries it: the document
/// whole the first time, what it changed after.
fn write(doc: &mut AutoCommit, writer: &Link, text: &str) -> DocumentMutation {
    let before = doc.get_heads();
    set_field(doc, person_of(writer), "note", text, true);
    if before.is_empty() {
        DocumentMutation::Snapshot(doc.save())
    } else {
        DocumentMutation::Incremental(doc.save_after(&before))
    }
}

/// What `page`'s session says it keeps of what it wrote to `workspaces`.
async fn kept(page: &Link, workspaces: &[Topic]) -> Vec<Kept> {
    within(
        "the session to say what it keeps",
        page.kept_of(workspaces).unwrap(),
    )
    .await
    .expect("the session answers")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_one_workspace_holds_keeps_no_other_workspaces_write_from_being_confirmed() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_server(
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
    written(&page, &here, &write(&mut note, &page, "first")).await;

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
    let founding = write(&mut unfounded, &page, "founded");
    let mut copy = unfounded.clone();
    page.write(there.clone(), &write(&mut copy, &page, "written early"))
        .unwrap();

    // What is written here next is confirmed for this workspace, while that change waits.
    page.write(here.clone(), &write(&mut note, &page, "second"))
        .unwrap();
    let confirmed = page.barrier_for(&[route()]).unwrap();
    within(
        "the write here to be confirmed while a change elsewhere waits for its document",
        confirmed,
    )
    .await
    .expect("the session answers")
    .expect("the server has the write here");

    // The session says what it keeps and why: elsewhere the change, which waits for the
    // document it changes; here nothing, once the server has the write.
    assert_eq!(
        kept(&page, &[elsewhere])
            .await
            .iter()
            .map(|kept| (&kept.route, &kept.waits))
            .collect::<Vec<_>>(),
        [(&there, &Waits::Document)],
        "a change that waits for its document is said so, by its document"
    );
    assert_eq!(
        kept(&page, &[workspace()]).await,
        Vec::new(),
        "a workspace whose writes the server has keeps none"
    );

    // The change that waits is not confirmed, nor is the session as a whole: both were asked
    // before a later write here, which is confirmed again.
    let own = page.barrier_for(std::slice::from_ref(&there)).unwrap();
    let whole = page.barrier().unwrap();
    page.write(here, &write(&mut note, &page, "third")).unwrap();
    within(
        "a later write here to be confirmed",
        page.barrier_for(&[route()]).unwrap(),
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
    assert_eq!(
        kept(&page, &[elsewhere, workspace()]).await,
        Vec::new(),
        "nothing is kept once the document landed and the change went"
    );
    for peer in [founder, page] {
        peer.close();
    }
}

/// One workspace is answered for each of its documents by itself, where it is asked so: a
/// change that waits for its document keeps no write to another document of that workspace from
/// being confirmed, and is itself confirmed neither by its own document, nor with the other,
/// nor by its workspace, until its document lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_one_document_holds_keeps_no_other_documents_write_from_being_confirmed() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_server(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let page = start_peer(
        server.iroh(),
        browser.cert.clone(),
        server_device.cert.clone(),
        browser.key.clone(),
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&page, workspace()).await;

    // A change to a document nobody founded yet: the page holds its keys and no document, so
    // the change waits for it.
    let waiting = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: WORKSPACE.into(),
        document: "note-2".into(),
    };
    page.send(Cmd::OpenRoute(waiting.clone())).unwrap();
    wait_keyed(&page, &waiting).await;
    let mut unfounded = AutoCommit::new();
    let founding = write(&mut unfounded, &page, "founded");
    let mut copy = unfounded.clone();
    page.write(waiting.clone(), &write(&mut copy, &page, "written early"))
        .unwrap();

    // A note of the same workspace, written after it, is confirmed for its own document.
    let beside = route();
    page.send(Cmd::OpenRoute(beside.clone())).unwrap();
    wait_keyed(&page, &beside).await;
    let mut note = AutoCommit::new();
    page.write(beside.clone(), &write(&mut note, &page, "first"))
        .unwrap();
    within(
        "a write to be confirmed while a change to another document of its workspace waits",
        page.barrier_for(std::slice::from_ref(&beside)).unwrap(),
    )
    .await
    .expect("the session answers")
    .expect("the server has the write beside the change that waits");
    assert_eq!(
        kept(&page, &[workspace()])
            .await
            .iter()
            .map(|kept| (&kept.route, &kept.waits))
            .collect::<Vec<_>>(),
        [(&waiting, &Waits::Document)],
        "the workspace keeps the change that waits, and nothing of the document beside it"
    );

    // The change that waits is confirmed neither by its own document, nor with the other, nor
    // by its workspace: each was asked before a later write beside it, which is confirmed again.
    let own = page.barrier_for(std::slice::from_ref(&waiting)).unwrap();
    let both = page
        .barrier_for(&[beside.clone(), waiting.clone()])
        .unwrap();
    let whole = page.barrier_of(&[workspace()]).unwrap();
    page.write(beside.clone(), &write(&mut note, &page, "second"))
        .unwrap();
    within(
        "a later write beside the change that waits to be confirmed",
        page.barrier_for(std::slice::from_ref(&beside)).unwrap(),
    )
    .await
    .expect("the session answers")
    .expect("the server has the later write beside it");
    assert_eq!(
        own.try_recv(),
        Err(TryRecvError::Empty),
        "a change that waits for its document is not said to be saved, asked of its document"
    );
    assert_eq!(
        both.try_recv(),
        Err(TryRecvError::Empty),
        "nor asked of its document together with one whose writes the server has"
    );
    assert_eq!(
        whole.try_recv(),
        Err(TryRecvError::Empty),
        "nor asked of its workspace"
    );

    // Another device founds the document; the change goes once it lands, and only then is it
    // confirmed, however it was asked.
    let founder = start_peer(
        server.iroh(),
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    founder.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&founder, workspace()).await;
    written(&founder, &waiting, &founding).await;
    within(
        "the change to be confirmed by its document once that landed",
        own,
    )
    .await
    .expect("the session answers")
    .expect("the server has the change");
    within("both documents to be confirmed", both)
        .await
        .expect("the session answers")
        .expect("the server has the writes to both");
    within("the workspace to be confirmed", whole)
        .await
        .expect("the session answers")
        .expect("the server has every write to the workspace");
    assert_eq!(
        kept(&page, &[workspace()]).await,
        Vec::new(),
        "nothing is kept once the document landed and the change went"
    );
    for peer in [founder, page] {
        peer.close();
    }
}
