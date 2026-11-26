//! What a member posts in Chat stays in the workspace after their device leaves.
//!
//! A member's device keeps what it posts and answers whoever asks the
//! workspace for it while it runs. It pushes each file to the host too, as it
//! keeps it: the attachments of its messages and the pictures of its feedback
//! reports. What it pushes is kept in the workspace's content store on the
//! host, from where the host answers another member's request: once the poster
//! is gone, the host alone serves the file.
use std::sync::Arc;

use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable as _};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server::kept_content::KeptContent;
use tilo_workspace_service::workspaces::fixtures::{hosting_device, member_device, test_host};

/// The app a member's messages and their files are kept in.
const CHAT: &str = "so.tilo.chat";

/// The record one of Chat's messages and a filed report are in: the app's
/// own document, where a message names its attachments and a rich text names
/// its pictures.
fn post(doc: &mut AutoCommit, notes: &str, picture: &str) {
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
}

/// What device `asker` asks the workspace for.
fn want(asker: &[u8; 32], workspace: &str, hash: &str) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
        requester: tilo_peer_state::peer_handle(asker),
        cluster_id: workspace.into(),
        wants: vec![neighbour_wire::content::Wanted {
            app_id: CHAT.into(),
            hash: hash.into(),
            have: Vec::new(),
        }],
    })
    .unwrap()
}

/// What device `poster` pushes to the host: the file `bytes`, whole.
fn push(host: &[u8; 32], poster: &[u8; 32], workspace: &str, bytes: &[u8]) -> Vec<u8> {
    postcard::to_allocvec(&NeighbourMsg::DocumentContentPieces {
        responder: neighbour_wire::peer_handle(poster),
        requester: neighbour_wire::peer_handle(host),
        cluster_id: workspace.into(),
        pieces: vec![neighbour_wire::content::Piece {
            app_id: CHAT.into(),
            hash: blake3::hash(bytes).to_hex().to_string(),
            total: u64::try_from(bytes.len()).unwrap(),
            offset: 0,
            bytes: bytes.to_vec(),
        }],
    })
    .unwrap()
}

/// The content the host answers `frames` with, addressed to `asker`, or `None`
/// when it keeps none.
fn opened(frames: &[Vec<u8>], asker: &[u8; 32]) -> Option<Vec<u8>> {
    let mut whole = Vec::new();
    for frame in frames {
        match postcard::from_bytes(frame).unwrap() {
            NeighbourMsg::DocumentContentPieces {
                requester, pieces, ..
            } => {
                assert_eq!(
                    requester,
                    tilo_peer_state::peer_handle(asker),
                    "addressed to whoever asked"
                );
                for piece in pieces {
                    assert_eq!(
                        piece.offset,
                        u64::try_from(whole.len()).unwrap(),
                        "every piece of the content, in order"
                    );
                    whole.extend(piece.bytes);
                }
            }
            NeighbourMsg::DocumentContentLacks { .. } => return None,
            other => panic!("pieces of content, or word that the host keeps none: {other:?}"),
        }
    }
    (!whole.is_empty()).then_some(whole)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_posted_in_chat_and_a_reports_picture_stay_when_their_poster_leaves() {
    let scratch = tempfile::tempdir().unwrap();
    let (root, host_cert) = hosting_device();
    let host = test_host(scratch.path(), &root, &host_cert).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Lounge").await.unwrap();
    let workspace = summary.workspace.clone();

    // The host keeps what the workspace's members push: the same keeper its daemon runs.
    let content = tilo_server::content::stores(scratch.path());
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());

    // The poster's device, a member's, and the reader's, by its key as a link
    // names a device.
    let poster = member_device(&workspaces, &workspace, &host_cert, 3)
        .await
        .1
        .device_public_key;
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
    crate::common::write_record(
        &workspaces,
        &workspace,
        (CHAT, "c_general"),
        |doc| -> Result<(), String> {
            post(doc, &notes_hash, &picture_hash);
            Ok(())
        },
    )
    .await
    .expect("the message and the report reach the workspace");

    // The poster's device pushes both files: the only copy from then on is the
    // host's.
    tilo_test_env::within("the host keeps the file and the picture", async {
        loop {
            workspaces.content_for_host(
                &workspace,
                poster,
                &push(&host_cert.device_public_key, &poster, &workspace, &notes),
            );
            workspaces.content_for_host(
                &workspace,
                poster,
                &push(&host_cert.device_public_key, &poster, &workspace, &picture),
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
    served_once_the_poster_left(&workspaces, &workspace, &reader, &notes, &notes_hash);
    served_once_the_poster_left(&workspaces, &workspace, &reader, &picture, &picture_hash);

    keeping.abort();
    host.shutdown().await;
}

/// The host serves `what` to `reader`, a member, once the device that posted it is gone: the
/// answer is the file's bytes.
fn served_once_the_poster_left(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    reader: &[u8; 32],
    what: &[u8],
    hash: &str,
) {
    assert_eq!(
        opened(
            &workspaces.content_answer(workspace, reader, &want(reader, workspace, hash)),
            reader,
        ),
        Some(what.to_vec()),
        "the file posted in Chat is served by the host once its poster is gone"
    );
}

/// Add to `doc`, Chat's record, a message's attachment `name` naming the file `hash`.
fn attach(doc: &mut AutoCommit, name: &str, hash: &str) -> Result<(), String> {
    use automerge::ReadDoc as _;
    let attachments = match doc
        .get(ROOT, "attachments")
        .map_err(|error| error.to_string())?
    {
        Some((automerge::Value::Object(ObjType::Map), id)) => id,
        _ => doc
            .put_object(ROOT, "attachments", ObjType::Map)
            .map_err(|error| error.to_string())?,
    };
    let attached = doc
        .put_object(&attachments, name, ObjType::Map)
        .map_err(|error| error.to_string())?;
    doc.put(&attached, "name", format!("{name}.bin"))
        .map_err(|error| error.to_string())?;
    doc.put(&attached, "blob", format!("tilo-blob://{hash}"))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The host keeps what a member pushes as it arrives, not on a later pass: a member who posts
/// one file after another sees each kept in turn, and five files posted one after the other are
/// all kept within two seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_file_a_member_posts_is_kept_as_it_is_pushed() {
    let scratch = tempfile::tempdir().unwrap();
    let (root, host_cert) = hosting_device();
    let host = test_host(scratch.path(), &root, &host_cert).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Lounge").await.unwrap();
    let workspace = summary.workspace.clone();
    let content = tilo_server::content::stores(scratch.path());
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));
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
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());
    let poster = member_device(&workspaces, &workspace, &host_cert, 3)
        .await
        .1
        .device_public_key;

    let started = std::time::Instant::now();
    for index in 0..5_u8 {
        let file = vec![index; 2_000];
        let hash = blake3::hash(&file).to_hex().to_string();
        crate::common::write_record(&workspaces, &workspace, (CHAT, "c_general"), |doc| {
            attach(doc, &format!("file-{index}"), &hash)
        })
        .await
        .expect("the message reaches the workspace");
        tilo_test_env::within("the host keeps the file the message names", async {
            loop {
                workspaces.content_for_host(
                    &workspace,
                    poster,
                    &push(&host_cert.device_public_key, &poster, &workspace, &file),
                );
                if workspaces
                    .kept_record_content(summary.workspace_id, CHAT, &hash)
                    .await
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await;
    }
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(2),
        "five files posted one after another were kept in {took:?}: each waited for a later \
         pass rather than being kept as it arrived"
    );

    keeping.abort();
    host.shutdown().await;
}
