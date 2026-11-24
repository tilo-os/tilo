//! What a host keeps of a workspace's content is bounded, and the bounds are
//! policy: the workspace's owners say how large one file may be and how much
//! one member's files may take, now and in any hour.
//!
//! Past a limit the poster is refused, plainly: the refusal names the limit
//! and who can raise it, while the record itself is untouched and syncs
//! between the members' devices as any does. Raising the limit lets the same
//! content through.
use std::sync::Arc;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _, transaction::Transactable as _};
use neighbour_wire::NeighbourMsg;
use tilo_action_policy::{ContentLimit, ContentPolicy};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server::kept_content::KeptContent;
use tilo_workspace_service::workspaces::fixtures::{governor, test_host};

/// The app a member's messages and their files are kept in.
const CHAT: &str = "so.tilo.chat";

/// Write into `doc`, Chat's record, a message whose one attachment is the file
/// `hash`: the way a member posts a file there. A file posted before stays
/// posted; the record names every one.
fn post_into(doc: &mut AutoCommit, hash: &str) {
    let attachments = match doc.get(ROOT, "attachments") {
        Ok(Some((automerge::Value::Object(ObjType::Map), map))) => map,
        _ => doc.put_object(ROOT, "attachments", ObjType::Map).unwrap(),
    };
    let attached = doc.put_object(&attachments, hash, ObjType::Map).unwrap();
    doc.put(&attached, "name", "notes.txt").unwrap();
    doc.put(&attached, "blob", format!("tilo-blob://{hash}"))
        .unwrap();
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

/// What became of one posted file.
#[derive(Debug, PartialEq, Eq)]
enum Posted {
    /// The host keeps it.
    Kept,
    /// The host refused it, past a limit: what the refusal says.
    Refused {
        limit: ContentLimit,
        allowed: u64,
        raise: String,
    },
}

/// Answer the host's asks for `bytes`, posted by device `poster`, until it
/// keeps the file or refuses it.
async fn deliver(
    workspaces: &tilo_server::workspaces::Workspaces,
    kept: &KeptContent,
    content: &tilo_content::WorkspaceStores,
    host: &[u8; 32],
    poster: &[u8; 32],
    workspace: &str,
    bytes: &[u8],
) -> Posted {
    let hash = blake3::hash(bytes).to_hex().to_string();
    tilo_test_env::within("the host answers the post", async {
        loop {
            workspaces.content_for_host(
                workspace,
                *poster,
                &answer(host, poster, workspace, bytes),
            );
            if content.get(workspace).is_ok_and(|store| store.has(&hash)) {
                return Posted::Kept;
            }
            if let Some(refusal) = kept
                .refusals(workspace)
                .await
                .into_iter()
                .find(|refusal| refusal.hash == hash)
            {
                return Posted::Refused {
                    limit: refusal.limit,
                    allowed: refusal.allowed,
                    raise: refusal.raise,
                };
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
}

/// Set the workspace's content policy as its owners do, and have the host read
/// it at once.
async fn settle(
    workspaces: &tilo_server::workspaces::Workspaces,
    kept: &KeptContent,
    host_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    policy: ContentPolicy,
) {
    workspaces
        .settle_content_policy(workspace, host_cert, policy)
        .await
        .expect("the workspace's owners settle its content policy");
    kept.refresh_limits().await;
}

/// The refusal a limit makes, naming who can raise it.
fn refused(limit: ContentLimit, allowed: u64) -> Posted {
    Posted::Refused {
        limit,
        allowed,
        raise: "the workspace's owners, in its content policy".to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one story: each limit refuses the member who went over it, and raising one lets it through"
)]
async fn a_members_share_a_files_size_and_the_hourly_intake_bound_what_the_host_keeps() {
    let scratch = tempfile::tempdir().unwrap();
    let (root, host_cert) = governor();
    let host = test_host(scratch.path(), &root, &host_cert).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Lounge").await.unwrap();
    let workspace = summary.workspace.clone();
    let host_device = host_cert.device_public_key;

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

    let ada = [3_u8; 32];
    let bo = [4_u8; 32];
    let cy = [5_u8; 32];
    let file = |len: usize, fill: u8| vec![fill; len];
    let ada_one = file(800, 1);
    let ada_two = file(800, 2);
    let bo_one = file(800, 3);
    let big = file(1_200, 7);
    let cy_one = file(800, 8);
    let cy_two = file(800, 9);

    // The members' messages are one record, written once: each names its file,
    // and the writer's device is the one that keeps it until the host does.
    let posted = [&ada_one, &ada_two, &bo_one, &big, &cy_one, &cy_two];
    workspaces
        .author_workspace_document(
            &workspace,
            &host_cert,
            (CHAT, ""),
            |doc| -> Result<(), String> {
                for bytes in posted {
                    post_into(doc, &blake3::hash(bytes).to_hex().to_string());
                }
                Ok(())
            },
        )
        .await
        .expect("the messages reach the workspace");

    // The host keeps what the workspace's records name: the same keeper its
    // daemon runs.
    let content = tilo_server::content::stores(scratch.path());
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());

    // A file is capped at 1000 bytes and a member's share at 1500; the hourly
    // intake is wide enough not to bind in this phase.
    settle(
        &workspaces,
        &kept,
        &host_cert,
        &workspace,
        ContentPolicy {
            file_bytes: Some(1_000),
            member_bytes: Some(1_500),
            hourly_bytes: Some(5_000),
        },
    )
    .await;

    // Ada posts within her share, and past it: the second file is refused,
    // naming her share and the workspace's owners, who can raise it.
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &ada,
            &workspace,
            &ada_one
        )
        .await,
        Posted::Kept
    );
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &ada,
            &workspace,
            &ada_two
        )
        .await,
        refused(ContentLimit::Member, 1_500)
    );

    // Bo still posts: one member's refusal narrows nobody else.
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &bo,
            &workspace,
            &bo_one
        )
        .await,
        Posted::Kept
    );

    // The owners raise Ada's share: the file refused before is kept now.
    settle(
        &workspaces,
        &kept,
        &host_cert,
        &workspace,
        ContentPolicy {
            file_bytes: Some(1_000),
            member_bytes: Some(5_000),
            hourly_bytes: Some(5_000),
        },
    )
    .await;
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &ada,
            &workspace,
            &ada_two
        )
        .await,
        Posted::Kept,
        "raising a limit lets the same content through"
    );

    // A file over the cap stays with whoever posted it, whenever it was posted.
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &bo,
            &workspace,
            &big
        )
        .await,
        refused(ContentLimit::File, 1_000)
    );

    // Cy posts within the hour's intake once, and the second file in the same
    // hour is refused, their share being no limit at all.
    settle(
        &workspaces,
        &kept,
        &host_cert,
        &workspace,
        ContentPolicy {
            file_bytes: Some(1_000),
            member_bytes: Some(5_000),
            hourly_bytes: Some(1_500),
        },
    )
    .await;
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &cy,
            &workspace,
            &cy_one
        )
        .await,
        Posted::Kept
    );
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &cy,
            &workspace,
            &cy_two
        )
        .await,
        refused(ContentLimit::Hourly, 1_500)
    );

    // Raising the hourly intake lets it through as well.
    settle(
        &workspaces,
        &kept,
        &host_cert,
        &workspace,
        ContentPolicy {
            file_bytes: Some(5_000),
            member_bytes: Some(5_000),
            hourly_bytes: Some(5_000),
        },
    )
    .await;
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &cy,
            &workspace,
            &cy_two
        )
        .await,
        Posted::Kept
    );

    // What the member who posted reads through the workspace's action: each
    // refusal named a limit and who can raise it, and none stands once its
    // limit was raised.
    let refused = workspaces.content_refused(&workspace).await;
    assert!(
        refused.is_empty(),
        "every refusal was reconsidered once its limit was raised: {refused:?}"
    );

    keeping.abort();
    host.shutdown().await;
}

/// A workspace folder is cloud storage: a member's own folder reaches their
/// own server whole when nobody set a limit — no size is compiled in per file,
/// per member or per hour. The folder is larger than every size the defaults
/// stopped (25 MiB a file, 250 MiB a member and an hour), and the policy's own
/// tests cover that no size at all bounds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one story: a whole folder is posted and streamed to the host piece by piece"
)]
async fn with_no_policy_a_members_own_folder_reaches_their_own_server_whole() {
    /// The folder's size: files of the largest size a content address is asked
    /// for whole, adding up to more than every size the defaults stopped.
    const FILES: usize = 7;
    const FILE_BYTES: usize = 64 * 1024 * 1024;

    let scratch = tempfile::tempdir().unwrap();
    let (root, host_cert) = governor();
    let host = test_host(scratch.path(), &root, &host_cert).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Lounge").await.unwrap();
    let workspace = summary.workspace.clone();
    let host_device = host_cert.device_public_key;

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

    // The folder, one file at a time: each has its own bytes, so each is its
    // own content.
    let mut folder = Vec::with_capacity(FILES);
    for file in 0..FILES {
        let bytes = vec![u8::try_from(file).unwrap_or(0); FILE_BYTES];
        let hash = blake3::hash(&bytes).to_hex().to_string();
        folder.push(hash);
    }
    workspaces
        .author_workspace_document(
            &workspace,
            &host_cert,
            (CHAT, ""),
            |doc| -> Result<(), String> {
                for hash in &folder {
                    post_into(doc, hash);
                }
                Ok(())
            },
        )
        .await
        .expect("the folder's messages reach the workspace");

    let content = tilo_server::content::stores(scratch.path());
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());

    // Nobody set a limit: no policy on the workspace, no budget on the host's
    // entry among its person's servers.
    let ada = [3_u8; 32];
    tilo_test_env::within("the host keeps a 3 GiB folder whole", async {
        for (file, hash) in folder.iter().enumerate() {
            let bytes = vec![u8::try_from(file).unwrap_or(0); FILE_BYTES];
            let mut sent = 0_usize;
            loop {
                if content.get(&workspace).is_ok_and(|store| store.has(hash)) {
                    break;
                }
                // A piece the host did not ask for yet is dropped and sent
                // again: the same bytes, the same address.
                let from = if sent >= bytes.len() { 0 } else { sent };
                let end = (from + neighbour_wire::content::PIECE_BYTES).min(bytes.len());
                let frame = postcard::to_allocvec(&NeighbourMsg::DocumentContentPiece {
                    responder: neighbour_wire::peer_handle(&ada),
                    requester: neighbour_wire::peer_handle(&host_device),
                    app_id: CHAT.into(),
                    cluster_id: workspace.clone(),
                    hash: hash.clone(),
                    total: u64::try_from(bytes.len()).unwrap(),
                    offset: u64::try_from(from).unwrap(),
                    bytes: bytes[from..end].to_vec(),
                })
                .unwrap();
                workspaces.content_for_host(&workspace, ada, &frame);
                sent = end;
                assert!(
                    kept.refusals(&workspace)
                        .await
                        .iter()
                        .all(|refusal| refusal.hash != *hash),
                    "a file of the member's own folder was refused"
                );
                tokio::task::yield_now().await;
            }
        }
    })
    .await;

    keeping.abort();
    host.shutdown().await;
}
