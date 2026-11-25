//! What a host keeps of a workspace's content is bounded, and the bounds are
//! policy: the workspace's owners say how large one file may be and how much
//! one member's files may take, now and in any hour.
//!
//! Past a limit the poster is refused, plainly: the refusal names the limit
//! and who can raise it, while the record itself is untouched and syncs
//! between the members' devices as any does. Raising the limit lets the same
//! content through.
use std::io::Read as _;
use std::sync::Arc;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _, transaction::Transactable as _};
use neighbour_wire::NeighbourMsg;
use tilo_action_policy::{ContentLimit, ContentPolicy, Raises};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server::kept_content::KeptContent;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, entry_join, governor, guest_device, test_host, test_host_holding,
};

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

/// A device of a person of its own, admitted into `workspace` through its
/// entry: the key it pushes its member's files from.
async fn joined(
    workspaces: &tilo_server::workspaces::Workspaces,
    host_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    seed: u8,
) -> [u8; 32] {
    let (key, cert) = guest_device();
    entry_join(workspaces, host_cert, workspace, (&key, &cert), seed).await;
    cert.device_public_key
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

/// What became of one posted file.
#[derive(Debug, PartialEq, Eq)]
enum Posted {
    /// The host keeps it.
    Kept,
    /// The host refused it, past a limit: what the refusal says.
    Refused {
        limit: ContentLimit,
        allowed: u64,
        raise: Raises,
    },
}

/// Push `bytes`, posted by device `poster`, until the host keeps the file or
/// refuses it.
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
            workspaces.content_for_host(workspace, *poster, &push(host, poster, workspace, bytes));
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

/// Set the workspace's content policy as its owners do, and wait for the host
/// to obey it: its keeper follows the workspace's governance, and nothing polls
/// it, so this waits on the keeper rather than on a clock.
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
    tilo_test_env::within("the host obeys the policy its owners settled", async {
        let wanted =
            tilo_action_policy::KeptLimits::of(&policy, kept.limits_of(workspace).workspace_bytes);
        while kept.limits_of(workspace) != wanted {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

/// The refusal a limit makes, naming who can raise it.
fn refused(limit: ContentLimit, allowed: u64) -> Posted {
    Posted::Refused {
        limit,
        allowed,
        raise: Raises::Owners,
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

    let ada = joined(&workspaces, &host_cert, &workspace, 13).await;
    let bo = joined(&workspaces, &host_cert, &workspace, 14).await;
    let cy = joined(&workspaces, &host_cert, &workspace, 15).await;
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
    crate::common::write_record(
        &workspaces,
        &workspace,
        (CHAT, "c_general"),
        |doc| -> Result<(), String> {
            for bytes in posted {
                post_into(doc, blake3::hash(bytes).to_hex().as_str());
            }
            Ok(())
        },
    )
    .await
    .expect("the messages reach the workspace");

    // The host keeps what the workspace's members push: the same keeper its
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

    // What a window reads as `workspace.content`: the limit the host obeys, and the refusal it
    // recorded, with the record and content it is about. A device whose person reads none of it
    // is served none.
    let reading = workspaces
        .content_reading(&workspace, host_cert.user_public_key)
        .await;
    assert_eq!(reading["file_bytes"], 1_000, "{reading}");
    let refusals = reading["refusals"].as_array().expect("the refusals");
    assert!(
        refusals.iter().any(|refusal| {
            refusal["record"]
                .as_str()
                .is_some_and(|record| record.starts_with(CHAT))
                && refusal["hash"] == blake3::hash(&big).to_hex().to_string()
                && refusal["limit"] == "file"
                && refusal["allowed"] == 1_000
        }),
        "the refused file is named in the root this device serves: {reading}"
    );
    let outside = workspaces.content_reading(&workspace, [9_u8; 32]).await;
    assert_eq!(
        outside["refusals"].as_array().map(Vec::len),
        Some(0),
        "a person the refusal is not for reads none of it: {outside}"
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

    // The file the file size refused is kept now its limit was raised: with
    // every limit raised and every refused file posted again, no refusal
    // stands, and each one the host kept is withdrawn from the workspace's
    // records.
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
        Posted::Kept
    );
    tilo_test_env::within("every raised refusal is withdrawn", async {
        while !workspaces.content_refused(&workspace).await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let after = workspaces
        .content_reading(&workspace, host_cert.user_public_key)
        .await;
    assert_eq!(
        after["refusals"].as_array().map(Vec::len),
        Some(0),
        "a raised limit's refusal stands for no reader: {after}"
    );
    assert_eq!(
        after["file_bytes"], 5_000,
        "the root reads the limits its owners now set: {after}"
    );

    keeping.abort();
    host.shutdown().await;
}

/// A refusal is a record of the workspace, not the host's memory: a host that
/// restarts refuses the same content no second time, and counts what it kept by
/// member and hour from the store, so a restart frees no member's share. A
/// raised limit lets the content through, and its refusal leaves with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one story: a refusal, a restart, and the totals the restart counts"
)]
async fn a_restart_refuses_the_same_content_no_second_time_and_keeps_the_totals() {
    let scratch = tempfile::tempdir().unwrap();
    let (root, host_cert) = governor();
    // One key service across both hosts: the device that comes up again opens
    // what the one before it sealed.
    let keys = Arc::new(TestGroupKeys::default());
    let mut host = test_host_holding(scratch.path(), &root, &host_cert, Arc::clone(&keys)).await;
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

    let ada = joined(&workspaces, &host_cert, &workspace, 13).await;
    let one = vec![1_u8; 800];
    let two = vec![2_u8; 800];
    let three = vec![3_u8; 800];
    let posted = [&one, &two, &three];
    crate::common::write_record(
        &workspaces,
        &workspace,
        (CHAT, "c_general"),
        |doc| -> Result<(), String> {
            for bytes in posted {
                post_into(doc, blake3::hash(bytes).to_hex().as_str());
            }
            Ok(())
        },
    )
    .await
    .expect("the messages reach the workspace");

    let content = tilo_server::content::stores(scratch.path());
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));

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

    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &ada,
            &workspace,
            &one
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
            &two
        )
        .await,
        refused(ContentLimit::Member, 1_500)
    );
    let first = kept.refusals(&workspace).await;
    let [refusal] = first.as_slice() else {
        panic!("one refusal was recorded: {first:?}");
    };
    assert_eq!(refusal.limit, ContentLimit::Member);
    assert_eq!(refusal.raise, Raises::Owners);
    assert_eq!(refusal.bytes, 800);
    let at = refusal.at;

    // The host restarts: its keeper and the workspaces it governs open again
    // from the store, with nothing of the refusal or the member's share in
    // memory.
    keeping.abort();
    drop(kept);
    host.shutdown().await;
    host = test_host_holding(scratch.path(), &root, &host_cert, Arc::clone(&keys)).await;
    let workspaces = host.workspaces().clone();
    workspaces.serve_content(Arc::new(tilo_server::content::StoresContent(Arc::clone(
        &content,
    ))));
    let kept = KeptContent::new(workspaces.clone(), Arc::clone(&content));
    let keeping = tokio::spawn(Arc::clone(&kept).keep_content());
    tilo_test_env::within("the host reads its refusals back from the store", async {
        while kept.refusals(&workspace).await.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await;

    // The same content is refused again, as the refusal the host already made:
    // one record, with the moment it was first refused.
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &ada,
            &workspace,
            &two
        )
        .await,
        refused(ContentLimit::Member, 1_500)
    );
    let again = kept.refusals(&workspace).await;
    let [refusal] = again.as_slice() else {
        panic!("still one refusal after a restart: {again:?}");
    };
    assert_eq!(refusal.at, at, "the same refusal, not a second one");

    // The member's share is counted from what the host kept, not from what it
    // saw since it started: another file is still over it.
    assert_eq!(
        deliver(
            &workspaces,
            &kept,
            &content,
            &host_device,
            &ada,
            &workspace,
            &three
        )
        .await,
        refused(ContentLimit::Member, 1_500),
        "a restart frees no member's share"
    );

    // Raising the share lets the refused file through, without the host
    // waiting on any interval, and its refusal is withdrawn once it is kept.
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
            &two
        )
        .await,
        Posted::Kept
    );
    tilo_test_env::within("the refusal leaves with the content it refused", async {
        while kept
            .refusals(&workspace)
            .await
            .iter()
            .any(|refusal| refusal.hash == blake3::hash(&two).to_hex().to_string())
        {
            tokio::task::yield_now().await;
        }
    })
    .await;

    keeping.abort();
    host.shutdown().await;
}

/// A workspace folder is cloud storage: a member's own folder reaches their
/// own server whole when nobody set a limit — no size is compiled in per file,
/// per member or per hour. The folder is larger than every size the defaults
/// stopped (25 MiB a file, 250 MiB a member and an hour), and the policy's own
/// tests cover that no size at all bounds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    crate::common::write_record(
        &workspaces,
        &workspace,
        (CHAT, "c_general"),
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
    let ada = joined(&workspaces, &host_cert, &workspace, 13).await;
    tilo_test_env::within(
        "the host keeps a folder larger than every default whole",
        async {
            for (file, hash) in folder.iter().enumerate() {
                let bytes = vec![u8::try_from(file).unwrap_or(0); FILE_BYTES];
                let mut sent = 0_usize;
                loop {
                    if content.get(&workspace).is_ok_and(|store| store.has(hash)) {
                        break;
                    }
                    // A piece the host dropped is sent again: the same bytes,
                    // the same address.
                    let from = if sent >= bytes.len() { 0 } else { sent };
                    let end = (from + neighbour_wire::content::PIECE_BYTES).min(bytes.len());
                    let frame = postcard::to_allocvec(&NeighbourMsg::DocumentContentPieces {
                        responder: neighbour_wire::peer_handle(&ada),
                        requester: neighbour_wire::peer_handle(&host_device),
                        cluster_id: workspace.clone(),
                        pieces: vec![neighbour_wire::content::Piece {
                            app_id: CHAT.into(),
                            hash: hash.clone(),
                            total: u64::try_from(bytes.len()).unwrap(),
                            offset: u64::try_from(from).unwrap(),
                            bytes: bytes[from..end].to_vec(),
                        }],
                    })
                    .unwrap();
                    workspaces.content_for_host(&workspace, ada, &frame);
                    sent = end;
                    tokio::task::yield_now().await;
                }
                assert!(
                    kept.refusals(&workspace)
                        .await
                        .iter()
                        .all(|refusal| refusal.hash != *hash),
                    "a file of the member's own folder was refused"
                );
            }
        },
    )
    .await;

    keeping.abort();
    host.shutdown().await;
}

/// The folder the 3 GiB check streams: this many files of this size, 3 GiB
/// exactly, each the largest content whose address a host is asked for whole.
const THREE_GIB_FILES: usize = 48;
const THREE_GIB_FILE_BYTES: usize = 64 * 1024 * 1024;

/// Bytes `len` from `offset` of file `file`, generated where they are sent: a
/// 4 KiB block is filled from where it sits in which file, so no file is ever
/// held whole, in the test or anywhere but the store that keeps it, and no two
/// files are the same content.
fn folder_piece(file: usize, offset: usize, len: usize) -> Vec<u8> {
    const BLOCK: usize = 4096;
    let mut bytes = vec![0_u8; len];
    for (index, block) in bytes.chunks_mut(BLOCK).enumerate() {
        let at = u64::try_from(
            offset
                .checked_div(BLOCK)
                .and_then(|block| block.checked_add(index))
                .unwrap_or(usize::MAX),
        )
        .unwrap_or(u64::MAX);
        let seed = at.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ u64::try_from(file)
                .unwrap_or(u64::MAX)
                .wrapping_mul(0xd6e8_feb8_6659_fd93);
        block.fill(u8::try_from((seed >> 32) & 0xff).unwrap_or(0));
    }
    bytes
}

/// File `file`'s address, from the pieces [`folder_piece`] generates for it.
fn folder_hash(file: usize) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0;
    while offset < THREE_GIB_FILE_BYTES {
        let end = offset
            .saturating_add(neighbour_wire::content::PIECE_BYTES)
            .min(THREE_GIB_FILE_BYTES);
        hasher.update(&folder_piece(file, offset, end.saturating_sub(offset)));
        offset = end;
    }
    hasher.finalize().to_hex().to_string()
}

/// One piece of file `file`'s content at `from`, as the member's device answers
/// the host with it.
fn folder_piece_frame(
    host: &[u8; 32],
    poster: &[u8; 32],
    workspace: &str,
    file: usize,
    hash: &str,
    from: usize,
) -> Vec<u8> {
    let end = from
        .saturating_add(neighbour_wire::content::PIECE_BYTES)
        .min(THREE_GIB_FILE_BYTES);
    postcard::to_allocvec(&NeighbourMsg::DocumentContentPieces {
        responder: neighbour_wire::peer_handle(poster),
        requester: neighbour_wire::peer_handle(host),
        cluster_id: workspace.into(),
        pieces: vec![neighbour_wire::content::Piece {
            app_id: CHAT.into(),
            hash: hash.into(),
            total: u64::try_from(THREE_GIB_FILE_BYTES).unwrap(),
            offset: u64::try_from(from).unwrap(),
            bytes: folder_piece(file, from, end.saturating_sub(from)),
        }],
    })
    .unwrap()
}

/// Three gigabytes of a member's own folder reach their own server whole when
/// nobody set a limit, and every byte arrives as the bytes it was sent as. The
/// folder is the size of a real one — 48 files of 64 MiB, the largest content
/// whose address a host is asked for whole — and the test generates it,
/// streaming each piece as it is answered, so it proves the store holding 3 GiB
/// and not the test's memory. It writes and hashes gigabytes, so it is
/// `#[ignore]`d, and its own dispatched check runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "writes 3 GiB to the store and reads it back: `t test kept-content-3gib` runs it"]
#[expect(
    clippy::too_many_lines,
    reason = "one story: a 3 GiB folder is posted piece by piece and read back whole"
)]
async fn a_three_gib_folder_reaches_its_own_server_whole() {
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

    // The folder's addresses, from the pieces the test is about to stream.
    let folder = tilo_test_env::within(
        "the folder's addresses are computed",
        tokio::task::spawn_blocking(|| (0..THREE_GIB_FILES).map(folder_hash).collect::<Vec<_>>()),
    )
    .await
    .expect("the folder's addresses are computed");

    crate::common::write_record(
        &workspaces,
        &workspace,
        (CHAT, "c_general"),
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
    let ada = joined(&workspaces, &host_cert, &workspace, 13).await;
    for (file, hash) in folder.iter().enumerate() {
        tilo_test_env::within(
            format!("the host keeps file {file} of the member's three gigabytes whole"),
            async {
                let mut sent = 0_usize;
                loop {
                    if content.get(&workspace).is_ok_and(|store| store.has(hash)) {
                        break;
                    }
                    // A piece the host dropped is sent again: the same bytes,
                    // the same address.
                    let from = if sent >= THREE_GIB_FILE_BYTES {
                        0
                    } else {
                        sent
                    };
                    let frame =
                        folder_piece_frame(&host_device, &ada, &workspace, file, hash, from);
                    workspaces.content_for_host(&workspace, ada, &frame);
                    sent = (from + neighbour_wire::content::PIECE_BYTES).min(THREE_GIB_FILE_BYTES);
                    tokio::task::yield_now().await;
                }
                assert!(
                    kept.refusals(&workspace)
                        .await
                        .iter()
                        .all(|refusal| refusal.hash != *hash),
                    "a file of the member's own folder was refused"
                );
            },
        )
        .await;
        eprintln!("kept file {file} of {THREE_GIB_FILES}");
    }

    // Every byte arrived, and as the bytes it was sent as: each file is read
    // back from the store, counted and hashed, a piece at a time.
    let store = content.get(&workspace).expect("the workspace's store");
    let mut total = 0_u64;
    for (file, hash) in folder.iter().enumerate() {
        let reading = store.clone();
        let wanted = hash.clone();
        let (bytes, digest) = tilo_test_env::within(
            format!("file {file} of the member's folder reads back whole"),
            tokio::task::spawn_blocking(move || {
                let mut opened = reading
                    .open(&wanted)
                    .expect("the host keeps a file of the member's folder");
                let mut seen = 0_u64;
                let mut hasher = blake3::Hasher::new();
                let mut buf = vec![0_u8; neighbour_wire::content::PIECE_BYTES];
                loop {
                    let read = opened.read(&mut buf).expect("the kept content reads");
                    if read == 0 {
                        break;
                    }
                    seen = seen.saturating_add(u64::try_from(read).unwrap_or(0));
                    hasher.update(&buf[..read]);
                }
                (seen, hasher.finalize().to_hex().to_string())
            }),
        )
        .await
        .expect("the folder's files read back");
        assert_eq!(
            bytes,
            u64::try_from(THREE_GIB_FILE_BYTES).unwrap(),
            "file {file} reached the store short"
        );
        assert_eq!(
            digest, *hash,
            "file {file} reached the store as other bytes"
        );
        total = total.saturating_add(bytes);
        eprintln!("read back file {file} of {THREE_GIB_FILES} whole: {bytes} bytes");
    }
    assert_eq!(
        total,
        u64::try_from(THREE_GIB_FILES * THREE_GIB_FILE_BYTES).unwrap(),
        "the store holds less than the folder's 3 GiB"
    );
    assert!(
        kept.refusals(&workspace).await.is_empty(),
        "content nobody limited was refused"
    );

    keeping.abort();
    host.shutdown().await;
}
