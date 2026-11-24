//! What a member posts in Chat stays in the workspace after their device leaves.
//!
//! A member's device keeps what it posts and answers whoever asks the
//! workspace for it while it runs. The host asks too: each time a write reaches
//! a record of Chat, it reads the files there — the attachments of its
//! messages and the pictures of its feedback reports — and asks the workspace
//! for every one it does not keep yet, on its own presence. What the poster's
//! device answers is kept in the workspace's content store on the host, from
//! where the host answers another member's request: once the poster is gone,
//! the host alone serves the file.
use std::sync::Arc;

use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable as _};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server::kept_content::KeptContent;
use tilo_workspace_service::workspaces::fixtures::{governor, test_host};

/// The app a member's messages and their files are kept in.
const CHAT: &str = "so.tilo.chat";

/// The record one of Chat's messages and a filed report are in: the app's
/// own document, where a message names its attachments and a rich text names
/// its pictures.
fn posted(notes: &str, picture: &str) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    // A message's attachment: the address its file's bytes read at.
    let attachments = doc.put_object(ROOT, "attachments", ObjType::Map).unwrap();
    let attached = doc.put_object(&attachments, "notes", ObjType::Map).unwrap();
    doc.put(&attached, "name", "notes.txt").unwrap();
    doc.put(&attached, "blob", format!("tilo-blob://{notes}"))
        .unwrap();
    // A feedback report's picture: the content a rich text's picture paragraph names.
    let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
    doc.put(&body, "schema", "tilo.richtext").unwrap();
    let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
    let paragraph = doc.put_object(&blocks, "shot", ObjType::Map).unwrap();
    doc.put(&paragraph, "kind", "image").unwrap();
    let attrs = doc.put_object(&paragraph, "attrs", ObjType::Map).unwrap();
    doc.put(&attrs, tilo_automerge::rich_text::ATTR_BLOB, picture)
        .unwrap();
    doc.save()
}

/// What device `asker` asks the workspace for.
fn want(asker: &[u8; 32], workspace: &str, hash: &str) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::DocumentContentWant {
        requester: tilo_peer_state::peer_handle(asker),
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        hash: hash.into(),
        have: Vec::new(),
    })
    .unwrap()
}

/// What device `poster` answers the host with: the file `bytes`, whole.
fn answer(host: &[u8; 32], poster: &[u8; 32], workspace: &str, bytes: &[u8]) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::DocumentContentPiece {
        responder: neighbour_wire::peer_handle(poster),
        requester: neighbour_wire::peer_handle(host),
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        hash: blake3::hash(bytes).to_hex().to_string(),
        total: u64::try_from(bytes.len()).unwrap(),
        offset: 0,
        bytes: bytes.to_vec(),
    })
    .unwrap()
}

/// The content the host answers `frames` with, addressed to `asker`, or `None`
/// when it keeps none.
fn opened(frames: &[Vec<u8>], asker: &[u8; 32]) -> Option<Vec<u8>> {
    let mut whole = Vec::new();
    for frame in frames {
        match postcard::from_bytes(frame).unwrap() {
            NeighbourMsg::DocumentContentPiece {
                requester,
                mut bytes,
                ..
            } => {
                assert_eq!(
                    requester,
                    tilo_peer_state::peer_handle(asker),
                    "addressed to whoever asked"
                );
                whole.append(&mut bytes);
            }
            NeighbourMsg::DocumentContentLacks { .. } => return None,
            other => panic!("a piece of content, or word that the host keeps none: {other:?}"),
        }
    }
    (!whole.is_empty()).then_some(whole)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the host asks, the poster answers, and another member opens both"
)]
async fn a_file_posted_in_chat_and_a_reports_picture_stay_when_their_poster_leaves() {
    let scratch = tempfile::tempdir().unwrap();
    let (root, host_cert) = governor();
    let host = test_host(scratch.path(), &root, &host_cert).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Lounge").await.unwrap();
    let workspace = summary.workspace.clone();

    // The host keeps what the workspace's records name: the same keeper its daemon runs.
    let content = tilo_server::content::stores(scratch.path());
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());

    // The poster's and the reader's devices, by their keys as a link names a
    // device: membership is decided when a device links, and this drives the
    // host's keeping and answering once it has.
    let poster = [3_u8; 32];
    let reader = [4_u8; 32];

    // The files only the poster's device keeps: a file sent with a message, and
    // a picture in a feedback report.
    let notes = b"the meeting notes, only on the poster's laptop".to_vec();
    let picture: Vec<u8> = b"the window's picture, only on the poster's laptop. "
        .iter()
        .copied()
        .cycle()
        .take(40_000)
        .collect();
    let notes_hash = blake3::hash(&notes).to_hex().to_string();
    let picture_hash = blake3::hash(&picture).to_hex().to_string();
    // Chat's record is sealed for its members' channels; the host founds the one
    // it keeps here, as it does for a record no device wrote yet.
    if let Some(Err(error)) = workspaces
        .own_route_keys(
            &DocumentRoute {
                app_id: CHAT.into(),
                cluster_id: workspace.clone(),
                document: String::new(),
            },
            true,
        )
        .await
    {
        panic!("the record's channel is not founded here: {error}");
    }
    workspaces
        .author_workspace_document(
            &workspace,
            &host_cert,
            (CHAT, ""),
            |doc| -> Result<(), String> {
                *doc = AutoCommit::load(&posted(&notes_hash, &picture_hash))
                    .map_err(|error| error.to_string())?;
                Ok(())
            },
        )
        .await
        .expect("the message and the report reach the workspace");

    // The host asks the workspace for both files, and the poster's device
    // answers: the only copy from then on is the host's.
    tilo_test_env::within("the host keeps the file and the picture", async {
        loop {
            workspaces.content_for_host(
                &workspace,
                poster,
                &answer(&host_cert.device_public_key, &poster, &workspace, &notes),
            );
            workspaces.content_for_host(
                &workspace,
                poster,
                &answer(&host_cert.device_public_key, &poster, &workspace, &picture),
            );
            let notes_held = workspaces
                .kept_record_content(summary.workspace_id, CHAT, &notes_hash)
                .await
                .is_some();
            let picture_held = workspaces
                .kept_record_content(summary.workspace_id, CHAT, &picture_hash)
                .await
                .is_some();
            if notes_held && picture_held {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;

    // The poster's device leaves: nothing but the host keeps either file. Another
    // member opens the file and the report's picture, and the host answers with both.
    assert_eq!(
        opened(
            &workspaces.content_answer(
                &workspace,
                &reader,
                &want(&reader, &workspace, &notes_hash),
            ),
            &reader,
        ),
        Some(notes),
        "the file posted in Chat is served by the host once its poster is gone"
    );
    assert_eq!(
        opened(
            &workspaces.content_answer(
                &workspace,
                &reader,
                &want(&reader, &workspace, &picture_hash),
            ),
            &reader,
        ),
        Some(picture),
        "the report's picture is served by the host once its poster is gone"
    );

    keeping.abort();
    host.shutdown().await;
}
