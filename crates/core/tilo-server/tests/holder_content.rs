//! A holder handed a workspace is pushed the content the holder handing it
//! over keeps, and every holder pushes the others what members push it.
//!
//! A record names its binaries by reference alone: the bytes live in a
//! content store beside it. A new holder copies every record, and a record
//! that names content its store lacks is worth nothing once the device that
//! had those bytes is gone. So the holder handing a workspace over pushes the
//! new one what it keeps and counts the hand-over done once the new one says
//! it keeps it all; a holder a member pushes to pushes the content on to the
//! workspace's other holders, again as each comes back; and until another
//! holder keeps it, the workspace's copy status says how much is on this one
//! alone.
use std::time::Duration;

use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use tilo_peer_state::{CopyRole, Missing, Shortfall};
use tilo_server::daemon::ServerIdentity;
use tilo_server::{Server, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, start_server};

/// The app a record of a kept picture is written in.
const PHOTOS: &str = "so.tilo.photos";

fn identity(user: &TestUser, device: &TestDevice) -> ServerIdentity {
    ServerIdentity {
        user_public_key: user.user_pk,
        device_signing_key: device.sk.clone(),
        device_cert: device.cert.clone(),
        revocations: RevocationList::new(),
    }
}

/// A server of `user`'s on `device`, keeping its state under `state`.
async fn server(
    user: &TestUser,
    device: &TestDevice,
    state: &std::path::Path,
    iroh_listen: std::net::SocketAddr,
) -> Server {
    if iroh_listen.port() != 0 {
        // A stopped server's connections let go of its socket shortly after
        // its shutdown returns.
        tilo_test_env::wait_for_within(
            "the stopped server's address to be free",
            std::time::Duration::from_secs(2),
            || std::net::UdpSocket::bind(iroh_listen).ok().map(drop),
        );
    }
    start_server(
        ServerConfig {
            state_root: state.to_path_buf(),
            iroh_listen,
            ..ServerConfig::default()
        },
        identity(user, device),
    )
    .await
    .expect("the server starts")
}

/// An address no other socket on this machine listens on, kept across a
/// restart so the host comes back where it was.
fn free_address() -> std::net::SocketAddr {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap()
}

/// The blobs of content `holder` says are on it alone in `workspace`'s copy
/// status, when it says any are.
async fn content_blobs(holder: &Server, workspace: &str) -> Option<u32> {
    let workspaces = holder.workspaces().expect("the server holds workspaces");
    let status = workspaces
        .copies(workspace)
        .await
        .expect("the workspace is held");
    status.shortfall.iter().find_map(|short| match short {
        Shortfall::Holder {
            missing: Missing::Content { blobs },
            ..
        } => Some(*blobs),
        _ => None,
    })
}

/// Write `workspace`'s album record naming the picture at `hash`, `size` bytes
/// long, as a device's write of it.
async fn name_picture(server: &Server, workspace: &str, hash: &str, size: usize) {
    crate::common::write_record(
        server.workspaces().unwrap(),
        workspace,
        (PHOTOS, "album"),
        |doc| {
            doc.put(ROOT, "photo", format!("blake3:{hash}:{size}"))
                .map_err(|error| error.to_string())
        },
    )
    .await
    .unwrap();
}

/// A workspace on server A whose record names a megabyte of content A's store
/// keeps. Server B is added and takes a copy, and A pushes it the content
/// although no member of the workspace is there to, before A counts it handed
/// over. A then goes away at once, and B serves the same bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_added_to_a_workspace_is_pushed_the_content_the_other_keeps() {
    let alice = TestUser::fresh("Alice");
    let first = alice.enrol("first-server", DeviceCapabilities::member());
    let second = alice.enrol("second-server", DeviceCapabilities::member());
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    let a = server(
        &alice,
        &first,
        first_state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let b = server(
        &alice,
        &second,
        second_state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let workspace = a
        .workspaces()
        .expect("the server holds workspaces")
        .create_workspace("Lounge", None)
        .await
        .unwrap();

    // A picture only A keeps, named by the record alone: a reference, never
    // the bytes, as every record names its binaries.
    let picture: Vec<u8> = b"a picture only the first server keeps. "
        .iter()
        .copied()
        .cycle()
        .take(1024 * 1024 + 7)
        .collect();
    let hash = blake3::hash(&picture).to_hex().to_string();
    a.content()
        .get(&workspace.workspace)
        .unwrap()
        .put(&picture)
        .unwrap();
    name_picture(&a, &workspace.workspace, &hash, picture.len()).await;

    // B is added: A hands it the workspace as one of its holders.
    a.workspaces()
        .unwrap()
        .attach_server(
            &workspace.workspace,
            second.device_pubkey(),
            b.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();

    // A counts the hand-over done only once B keeps the picture, fetched
    // into its own content store.
    assert_eq!(
        b.content()
            .get(&workspace.workspace)
            .ok()
            .and_then(|store| store.read(&hash))
            .as_deref(),
        Some(picture.as_slice()),
        "the server added keeps the resource's content once it is handed the workspace"
    );

    // The first server goes away: the bytes are the added holder's now.
    a.shutdown().await;
    assert_eq!(
        b.workspaces()
            .unwrap()
            .kept_content(workspace.workspace_id, &hash)
            .await
            .as_deref(),
        Some(picture.as_slice()),
        "the added holder serves what its record names"
    );
}

/// What `member` pushes `holder`: the picture `bytes` of `workspace`, whole.
fn pushed(holder: &Server, member: &TestDevice, workspace: &str, bytes: &[u8]) {
    let mut frames = Vec::new();
    let host = holder.workspaces().unwrap().device_cert().device_public_key;
    let mut packer = neighbour_wire::content::Packer::new(
        tilo_peer_state::peer_handle(&member.device_pubkey()),
        tilo_peer_state::peer_handle(&host),
        workspace,
        |frame| frames.push(frame),
    );
    let total = u64::try_from(bytes.len()).unwrap();
    for (at, piece) in bytes
        .chunks(neighbour_wire::content::PIECE_BYTES)
        .enumerate()
    {
        packer.piece(neighbour_wire::content::Piece {
            app_id: PHOTOS.into(),
            hash: blake3::hash(bytes).to_hex().to_string(),
            total,
            offset: u64::try_from(at.saturating_mul(neighbour_wire::content::PIECE_BYTES)).unwrap(),
            bytes: piece.to_vec(),
        });
    }
    packer.finish();
    for frame in frames {
        holder.workspaces().unwrap().content_for_host(
            workspace,
            member.device_pubkey(),
            &frame.frame(),
        );
    }
}

/// A holder that was stopped when a member pushed the other holder a picture
/// is pushed it once it starts again: the holder the member reached says, in
/// the workspace's copy status, that the picture is on it alone until then,
/// and stops saying so once the other keeps it, which it keeps across its own
/// restart as a member's hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_stopped_when_a_member_pushed_content_is_pushed_it_when_it_starts_again() {
    let alice = TestUser::fresh("Alice");
    let first = alice.enrol("first-server", DeviceCapabilities::member());
    let second = alice.enrol("second-server", DeviceCapabilities::member());
    let laptop = alice.enrol("laptop", DeviceCapabilities::member());
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    let a = server(
        &alice,
        &first,
        first_state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    // B keeps its address across the restart, so A reaches it where it was.
    let listen = free_address();
    let b = server(&alice, &second, second_state.path(), listen).await;
    let workspace = a
        .workspaces()
        .expect("the server holds workspaces")
        .create_workspace("Lounge", None)
        .await
        .unwrap();
    a.workspaces()
        .unwrap()
        .enter(&workspace.workspace, (&laptop.cert, &laptop.sk))
        .await
        .expect("Alice's laptop enters the workspace");
    a.workspaces()
        .unwrap()
        .attach_server(
            &workspace.workspace,
            second.device_pubkey(),
            b.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();

    // B stops; Alice's laptop pushes A a picture and names it in a record.
    b.shutdown().await;
    let picture: Vec<u8> = b"a picture the stopped holder is pushed once it is back. "
        .iter()
        .copied()
        .cycle()
        .take(1024 * 1024 + 7)
        .collect();
    let hash = blake3::hash(&picture).to_hex().to_string();
    name_picture(&a, &workspace.workspace, &hash, picture.len()).await;
    pushed(&a, &laptop, &workspace.workspace, &picture);
    tilo_test_env::within("A says the picture is on it alone", async {
        while content_blobs(&a, &workspace.workspace).await != Some(1) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    // B starts again on its files, and A pushes it what it missed.
    let b = server(&alice, &second, second_state.path(), listen).await;
    tilo_test_env::within(
        "B keeps the picture and A says nothing is on it alone",
        async {
            loop {
                let kept = b
                    .content()
                    .get(&workspace.workspace)
                    .ok()
                    .and_then(|store| store.read(&hash));
                if kept.as_deref() == Some(picture.as_slice())
                    && content_blobs(&a, &workspace.workspace).await.is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        },
    )
    .await;

    // B holds it across its own restart, and serves it once A is gone.
    b.shutdown().await;
    let b = server(&alice, &second, second_state.path(), listen).await;
    a.shutdown().await;
    assert_eq!(
        b.workspaces()
            .unwrap()
            .content_held(&workspace.workspace)
            .held,
        1,
        "the restarted holder still holds the picture for the member who pushed it"
    );
    assert_eq!(
        b.workspaces()
            .unwrap()
            .kept_content(workspace.workspace_id, &hash)
            .await
            .as_deref(),
        Some(picture.as_slice()),
        "the restarted holder serves what its record names"
    );
}

/// How many pictures the workspace handed over names that no device keeps:
/// their addresses go to the server taking it, at about seventy bytes each,
/// far more than one action frame carries
/// ([`tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES`]).
const NOWHERE_KEPT: usize = 60_000;

/// How many of the pictures one write of the record names: a write carries
/// at most 64 KiB of changes.
const PER_WRITE: usize = 400;

/// A workspace whose record names far more content than one frame's worth of
/// addresses, none of which the server handing it over keeps: its files
/// live on a member's device, as a laptop's do once it signed in. The server
/// added takes the workspace all the same: it is not held to what the
/// handing server keeps none of, so the hand-over finishes once the changes
/// are across, and the content follows as the members that keep it push it. The
/// server added says where its store stands two cursors at a time, in pages
/// as it does for a workspace of many records and authors. Had the list or the
/// cursors not arrived whole, the server would still lack what they leave
/// out, and the hand-over would fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_whose_records_name_more_content_than_a_frame_lists_is_handed_over() {
    let alice = TestUser::fresh("Alice");
    let first = alice.enrol("first-server", DeviceCapabilities::member());
    let second = alice.enrol("second-server", DeviceCapabilities::member());
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    let a = server(
        &alice,
        &first,
        first_state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let b = server(
        &alice,
        &second,
        second_state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    b.workspaces().unwrap().answer_cursors_in_pages_of(2);
    let workspace = a
        .workspaces()
        .expect("the server holds workspaces")
        .create_workspace("Lounge", None)
        .await
        .unwrap();

    // Pictures no server keeps, each named by reference alone.
    for write in 0..NOWHERE_KEPT / PER_WRITE {
        crate::common::write_record(
            a.workspaces().unwrap(),
            &workspace.workspace,
            (PHOTOS, "album"),
            |doc| {
                let photos = match doc.get(ROOT, "photos").map_err(|error| error.to_string())? {
                    Some((automerge::Value::Object(automerge::ObjType::Map), photos)) => photos,
                    _ => doc
                        .put_object(ROOT, "photos", automerge::ObjType::Map)
                        .map_err(|error| error.to_string())?,
                };
                for picture in 0..PER_WRITE {
                    let named = write * PER_WRITE + picture;
                    let hash = blake3::hash(format!("picture {named}").as_bytes()).to_hex();
                    doc.put(
                        &photos,
                        format!("photo-{named}"),
                        format!("blake3:{hash}:4096"),
                    )
                    .map_err(|error| error.to_string())?;
                }
                Ok(())
            },
        )
        .await
        .unwrap();
    }

    let hint = a
        .workspaces()
        .unwrap()
        .attach_server(
            &workspace.workspace,
            second.device_pubkey(),
            b.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .expect("the server added takes the workspace although no server keeps its pictures");
    assert!(
        a.workspaces()
            .unwrap()
            .hosts(&workspace.workspace)
            .await
            .unwrap()
            .contains(&hint),
        "the workspace names the server added among those keeping it"
    );
}
