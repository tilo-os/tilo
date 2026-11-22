//! A holder handed a workspace takes in the content its records name, from
//! the holder that hands it over as from any other device that keeps it.
//!
//! A record names its binaries by reference alone: the bytes live in a
//! content store beside it. A new holder copies every record, and a record
//! that names content its store lacks is worth nothing once the device that
//! had those bytes is gone. So a holder that arrives, or one already holding,
//! fetches each content its records reference from a device that keeps it,
//! puts it in its own store, and says how much it still lacks in the
//! workspace's copy status until it lacks none.
use std::time::Duration;

use automerge::{ROOT, transaction::Transactable as _};
use tilo_peer_state::{CopyRole, Missing, Shortfall};
use tilo_server::daemon::ServerIdentity;
use tilo_server::{Server, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, start_server};

/// The app a record of a kept picture is written in: a record whose content
/// no keeper of the workspace's own fetches for it.
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
        tilo_test_env::wait_for("the stopped server's address to be free", || {
            std::net::UdpSocket::bind(iroh_listen).ok().map(drop)
        });
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

/// Whether `holder` says it still lacks content: the row it wrote in the
/// workspace's copy status, as every holder reads it.
async fn lacks_content(holder: &Server, workspace: &str) -> bool {
    content_blobs(holder, workspace).await.is_some()
}

/// The blobs of content `holder` says it lacks in `workspace`'s copy status,
/// when it says it lacks any.
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

/// A workspace on server A whose record names a megabyte of content A's store
/// keeps. Server B is added and takes a copy, and fetches the content from A
/// although no member of the workspace holds it. A then goes away, and B
/// serves the same bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the servers stay live until their consuming shutdown"
)]
async fn a_holder_added_to_a_workspace_fetches_the_content_its_records_name() {
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
    a.workspaces()
        .unwrap()
        .author_workspace_document(&workspace.workspace, &first.cert, (PHOTOS, ""), |doc| {
            doc.put(ROOT, "photo", format!("blake3:{hash}:{}", picture.len()))
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();

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

    // B fetches the picture from A, into its own content store, and says it
    // lacks no content once it has it.
    tilo_test_env::within("the added holder keeps the record's content", async {
        loop {
            let kept = b
                .content()
                .get(&workspace.workspace)
                .ok()
                .and_then(|store| store.read(&hash));
            if kept.as_deref() == Some(picture.as_slice())
                && !lacks_content(&b, &workspace.workspace).await
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

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

/// A holder stopped before it finished fetching fetches again when it starts:
/// what its records name is measured against what its store holds each time
/// it comes up, never remembered as done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the servers stay live until their consuming shutdown"
)]
async fn a_holder_stopped_before_it_fetched_fetches_when_it_starts_again() {
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
    // B keeps its address across the restart, so A reaches it where it was.
    let listen = free_address();
    let b = server(&alice, &second, second_state.path(), listen).await;
    let workspace = a
        .workspaces()
        .expect("the server holds workspaces")
        .create_workspace("Lounge", None)
        .await
        .unwrap();
    let picture: Vec<u8> = b"a picture the new holder fetches after it restarts. "
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
    a.workspaces()
        .unwrap()
        .author_workspace_document(&workspace.workspace, &first.cert, (PHOTOS, ""), |doc| {
            doc.put(ROOT, "photo", format!("blake3:{hash}:{}", picture.len()))
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
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

    // The fetch had not finished: B stops holding none of it, and starts
    // again on its files, where its records still name the picture.
    b.shutdown().await;
    let b = server(&alice, &second, second_state.path(), listen).await;

    tilo_test_env::within("the restarted holder fetches the record's content", async {
        loop {
            let kept = b
                .content()
                .get(&workspace.workspace)
                .ok()
                .and_then(|store| store.read(&hash));
            if kept.as_deref() == Some(picture.as_slice())
                && !lacks_content(&b, &workspace.workspace).await
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    a.shutdown().await;
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

/// A holder that lacks a record's content says how much, and stops saying so
/// once its store holds it: the number is what its records reference minus
/// what its store holds, read afresh whenever either changes, never a count
/// kept from the moment it took the copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the servers stay live until their consuming shutdown"
)]
async fn a_holder_says_how_much_content_it_lacks_until_it_keeps_it() {
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

    // The record names a picture that no store holds yet: not even the server
    // that wrote the record has the bytes.
    let picture: Vec<u8> = b"a picture the servers fetch once one keeps it. "
        .iter()
        .copied()
        .cycle()
        .take(1024 * 1024 + 7)
        .collect();
    let hash = blake3::hash(&picture).to_hex().to_string();
    a.workspaces()
        .unwrap()
        .author_workspace_document(&workspace.workspace, &first.cert, (PHOTOS, ""), |doc| {
            doc.put(ROOT, "photo", format!("blake3:{hash}:{}", picture.len()))
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
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

    // The added holder says it lacks the picture: one blob its records name
    // and its store does not hold.
    tilo_test_env::within("the added holder says it lacks the picture", async {
        loop {
            if content_blobs(&b, &workspace.workspace)
                .await
                .is_some_and(|blobs| blobs >= 1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    // The first server comes to keep it, and says so to whoever asked.
    a.content()
        .get(&workspace.workspace)
        .unwrap()
        .put(&picture)
        .unwrap();
    a.workspaces()
        .unwrap()
        .content_kept(&workspace.workspace, &hash);

    // The added holder takes it into its store, and says it lacks none.
    tilo_test_env::within(
        "the added holder fetches it and says it lacks none",
        async {
            loop {
                let kept = b
                    .content()
                    .get(&workspace.workspace)
                    .ok()
                    .and_then(|store| store.read(&hash));
                if kept.as_deref() == Some(picture.as_slice())
                    && content_blobs(&b, &workspace.workspace).await.is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        },
    )
    .await;

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
