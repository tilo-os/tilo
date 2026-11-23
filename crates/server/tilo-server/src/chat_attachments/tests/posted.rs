//! A file a member posted in the workspace's Chat, kept on the host past
//! their leaving within the quotas.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_app_id::apps::CHAT;
use tilo_resource::MemberId;
use tilo_workspace_service::workspaces::fixtures::chat::{chat_workspace, first_part, post_with};
use tilo_workspace_service::workspaces::fixtures::{governor, test_host};

use crate::workspaces::Workspaces;

/// Ada's root key.
const ADA: [u8; 32] = [8; 32];

/// The files the chat's first part references, as the workspace holds it.
async fn referenced_in(
    workspaces: &Workspaces,
    workspace: &str,
) -> Vec<crate::chat_attachments::Referenced> {
    crate::chat_attachments::referenced(
        &workspaces
            .resource_document(workspace, CHAT, &first_part())
            .await
            .unwrap()
            .unwrap(),
    )
}

/// The file `bytes` as a message's attachment.
fn file(name: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "kind": "file",
        "name": name,
        "blob": blake3::hash(bytes).to_hex().to_string(),
        "size": bytes.len(),
    })
}

/// The one piece `device` answers `want` with when it keeps `bytes`, as a
/// member's device answers.
fn answer(want: &[u8], device: [u8; 32], bytes: &[u8]) -> Vec<u8> {
    let neighbour_wire::NeighbourMsg::DocumentContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
        ..
    } = postcard::from_bytes(want).unwrap()
    else {
        panic!("a request for content")
    };
    postcard::to_allocvec(&neighbour_wire::NeighbourMsg::DocumentContentPiece {
        responder: tilo_peer_state::peer_handle(&device),
        requester,
        app_id,
        cluster_id,
        hash,
        total: u64::try_from(bytes.len()).unwrap(),
        offset: 0,
        bytes: bytes.to_vec(),
    })
    .unwrap()
}

/// What the workspace answers `device` asking it for `bytes`, assembled.
fn opened(
    workspaces: &Workspaces,
    workspace: &str,
    device: [u8; 32],
    bytes: &[u8],
) -> Option<Vec<u8>> {
    let want = postcard::to_allocvec(&neighbour_wire::NeighbourMsg::DocumentContentWant {
        requester: tilo_peer_state::peer_handle(&device),
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        hash: blake3::hash(bytes).to_hex().to_string(),
        have: Vec::new(),
    })
    .unwrap();
    let mut whole = Vec::new();
    for frame in workspaces.content_answer(workspace, &device, &want) {
        match postcard::from_bytes(&frame).unwrap() {
            neighbour_wire::NeighbourMsg::DocumentContentPiece { bytes, .. } => whole.extend(bytes),
            neighbour_wire::NeighbourMsg::DocumentContentLacks { .. } => return None,
            _ => panic!("a piece of content, or word that the host keeps none"),
        }
    }
    (!whole.is_empty()).then_some(whole)
}

/// Ada posts a file in the workspace's Chat. While her device is online it
/// answers the host's request for it; once she left, Bo opens the file
/// from the host's copy. Past Ada's quota her oldest file goes, and a file
/// over the cap is not asked for at all.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one story: posting, leaving, the host's copy, the quota and the cap"
)]
async fn a_file_a_member_posted_stays_in_the_workspace_after_they_leave() {
    const BO: [u8; 32] = [9; 32];
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    // The native apps name their surfaces as the host first runs them.
    let _ = tilo_agent_session::AppCatalogue::native();
    crate::surfaces::decide_receipt(&scratch.path().join("surfaces"));
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let content = crate::content::stores(scratch.path());
    workspaces.serve_content(std::sync::Arc::new(crate::content::StoresContent(
        content.clone(),
    )));
    let blobs = content.get(&workspace).unwrap();
    let attachments_of = |limits| {
        crate::chat_attachments::WorkspaceAttachments::new(
            workspaces.clone(),
            workspace.clone(),
            cert.device_public_key,
            blobs.clone(),
            scratch.path().join("attachments.json"),
            limits,
        )
    };
    let keeper = attachments_of(crate::chat_attachments::AttachmentLimits {
        file_bytes: 64,
        member_bytes: 40,
        workspace_bytes: 1000,
    });
    let kept: std::sync::Arc<dyn crate::workspaces::ContentKeeper> = keeper.clone();
    workspaces.keep_content(&kept);
    let ada = hex::encode(ADA);

    let notes = b"the meeting notes, version one".to_vec();
    post_with(
        &workspaces,
        &workspace,
        &cert,
        (&ada, &general, "notes"),
        Some(file("notes.txt", &notes)),
    )
    .await;
    let asks = keeper.asks(&referenced_in(&workspaces, &workspace).await, 0);
    assert_eq!(
        asks.len(),
        1,
        "the host asks the workspace for the file it lacks"
    );
    assert!(
        keeper
            .asks(&referenced_in(&workspaces, &workspace).await, 1)
            .is_empty(),
        "and only once a while"
    );
    // Ada's device answers the host; then she leaves.
    workspaces.content_for_host(&workspace, ADA, &answer(&asks[0], ADA, &notes));
    assert_eq!(
        keeper.kept_for(&blake3::hash(&notes).to_hex()),
        Some(MemberId::person(ada.as_str()))
    );
    assert_eq!(
        opened(&workspaces, &workspace, BO, &notes),
        Some(notes.clone()),
        "Bo opens it from the host"
    );

    // A second file takes Ada past her quota: her oldest goes.
    let slides = b"the slides that came after them".to_vec();
    post_with(
        &workspaces,
        &workspace,
        &cert,
        (&ada, &general, "slides"),
        Some(file("slides.txt", &slides)),
    )
    .await;
    assert!(
        keeper
            .asks(&referenced_in(&workspaces, &workspace).await, 2)
            .is_empty(),
        "within the hour her quota has no workspace for it"
    );
    let an_hour_later = 3_600_000 + 2;
    let asks = keeper.asks(&referenced_in(&workspaces, &workspace).await, an_hour_later);
    assert_eq!(
        asks.len(),
        1,
        "an hour later only the new file is asked for"
    );
    workspaces.content_for_host(&workspace, ADA, &answer(&asks[0], ADA, &slides));
    assert_eq!(
        opened(&workspaces, &workspace, BO, &slides),
        Some(slides.clone())
    );
    assert_eq!(
        opened(&workspaces, &workspace, BO, &notes),
        None,
        "the oldest went past her quota"
    );

    // A file over the cap stays with its poster.
    let video = vec![7_u8; 65];
    post_with(
        &workspaces,
        &workspace,
        &cert,
        (&ada, &general, "a video"),
        Some(file("big.bin", &video)),
    )
    .await;
    assert!(
        keeper
            .asks(
                &referenced_in(&workspaces, &workspace).await,
                an_hour_later + 1
            )
            .is_empty()
    );

    // A restart keeps counting what it kept.
    drop(keeper);
    let again = attachments_of(crate::chat_attachments::AttachmentLimits::default());
    assert_eq!(
        again.kept_for(&blake3::hash(&slides).to_hex()),
        Some(MemberId::person(ada.as_str()))
    );
    assert_eq!(again.kept_for(&blake3::hash(&notes).to_hex()), None);
}
