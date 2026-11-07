//! A host decides the changes members make against the app surfaces those
//! changes name, and the workspace is where a surface it lacks comes from: the
//! member that wrote the change answers the host's ask for it, or any other
//! member that has it. What arrived is the workspace's content from then on —
//! a start after this one reads it beside the workspace's state, and a member
//! reading a record an older version of the app started is answered with it.
//!
//! No registry is on this path: a version no registry serves reads as well as
//! one it does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host stays live for the whole test"
)]

use neighbour_wire::NeighbourMsg;

const APP: &str = "so.tilo.chat";

/// A surface of a version of the app this host does not run: nothing in it
/// matters but its bytes, which no registry has.
fn unserved_surface() -> Vec<u8> {
    b"kind = \"chat.workspace\"\nversion = 1\n# written by a version no registry serves\n".to_vec()
}

/// What a member's device asks the workspace for.
fn want(requester: u64, workspace: &str, hash: &str) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::RecordContentWant {
        requester,
        app_id: APP.into(),
        cluster_id: workspace.into(),
        hash: hash.into(),
    })
    .unwrap()
}

/// What device `member` answers the host with: the surface `bytes`, whole.
fn answer(host: &[u8; 32], member: [u8; 32], workspace: &str, bytes: &[u8]) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::RecordContentPiece {
        responder: neighbour_wire::peer_handle(&member),
        requester: neighbour_wire::peer_handle(host),
        app_id: APP.into(),
        cluster_id: workspace.into(),
        hash: blake3::hash(bytes).to_hex().to_string(),
        total: u64::try_from(bytes.len()).unwrap(),
        offset: 0,
        bytes: bytes.to_vec(),
    })
    .unwrap()
}

/// The content the host answers `frames` with, or `None` when it keeps none.
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
async fn a_surface_a_member_answers_with_is_the_workspaces_from_then_on() {
    // The plane it installs is the process's: no other test runs here.
    if !crate::common::in_own_process(
        "surface_content::a_surface_a_member_answers_with_is_the_workspaces_from_then_on",
    ) {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let plane = scratch.path().join("surfaces");
    tilo_room_host::surfaces::decide_receipt(&plane);
    let (key, cert) = tilo_room_host::workspaces::fixtures::governor();
    let host = tilo_room_host::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let surface = unserved_surface();
    let hash = blake3::hash(&surface).to_hex().to_string();

    // Nothing has it: a member asking is told so, not left waiting.
    assert_eq!(
        gathered(&workspaces.content_answer(&workspace, &want(42, &workspace, &hash))),
        None
    );

    // A member's change names it: the host asks the workspace for it, and the
    // member answers the host with its bytes.
    let reached = workspaces.ask_surface(&workspace, APP, &hash).await;
    assert!(reached <= 1, "no members are in a workspace made just now");
    let member = [9; 32];
    workspaces.content_for_host(
        &workspace,
        member,
        &answer(&cert.device_public_key, member, &workspace, &surface),
    );

    // It is the workspace's content now: the host decides with it
    // ([`tilo_room_host::surfaces`]) and answers members reading the record it
    // was written with.
    assert_eq!(
        gathered(&workspaces.content_answer(&workspace, &want(42, &workspace, &hash))),
        Some(surface.clone()),
        "the surface the member answered with, whole"
    );

    // Kept beside the workspace's state: a start after this one has it without
    // asking anybody.
    assert_eq!(std::fs::read(plane.join(&hash)).unwrap(), surface);
    tilo_room_host::surfaces::decide_receipt(&plane);
    assert_eq!(
        gathered(&workspaces.content_answer(&workspace, &want(42, &workspace, &hash))),
        Some(surface),
        "answered after a start that asked nobody"
    );
    assert_eq!(
        gathered(&workspaces.content_answer(&workspace, &want(42, &workspace, &"0".repeat(64)))),
        None,
        "a surface nobody answered with is still not kept"
    );

    // Bytes that do not hash to the address they came under are not the
    // surface asked for.
    let other = blake3::hash(b"a version the workspace asked for")
        .to_hex()
        .to_string();
    workspaces.ask_surface(&workspace, APP, &other).await;
    workspaces.content_for_host(
        &workspace,
        member,
        &answer(
            &cert.device_public_key,
            member,
            &workspace,
            b"not those bytes",
        ),
    );
    assert_eq!(
        gathered(&workspaces.content_answer(&workspace, &want(42, &workspace, &other))),
        None
    );

    host.shutdown().await;
}
