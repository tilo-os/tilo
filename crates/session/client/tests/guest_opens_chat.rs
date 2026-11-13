//! A fresh guest who opens a public workspace's chat the moment it enters
//! reads the chat's history: whatever the page is sent of the chat, it reads
//! under its own admission, never before it.
//!
//! A public workspace sends a record only once a page opens it, so a page
//! that opens its chat as it enters is sent the chat's newest part while the
//! workspace's governance still comes in. The page reads each record as it
//! lands in its replica, for the reader its own device is: a record landing
//! before the governance that admits that device was read as a stranger's
//! and refused, and nothing read it again, so the chat stayed empty.
//!
//! The workspace here is the Lounge's shape: 300 members who came through
//! its door, one of whom wrote the chat's 200 messages, each a write of its
//! own. Each guest opens the chat's head with its entry, and every landing of
//! the chat must read. The chat reaches the guest in the one exchange its
//! opening starts: nothing about it waits on a timer or a retry, so it reads
//! whole within [`OPENED_TO_READ`] of the guest asking for it, however many
//! writes it is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_session_client::device::{Entering, door_entry};
use tilo_session_client::{Cmd, Event, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{enrol, enter_page, start_workspace_host_relaying, wait_keyed, written};
use collab_types::WorkspaceId;

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// The chain the chat keeps its messages in.
const CHAT_CHAIN: &str = "messages";

/// How many people come through the door at once while the crowd is made.
const ARRIVING: usize = 16;

/// The messages the chat holds, each written on its own as a member posts it.
const MESSAGES: usize = 200;

/// The longest a guest's page may take to read the chat whole after it opens it, on a host in
/// the same process: the host sending what it holds and the page taking it in. A refused read
/// waiting for the next landing, or a write let go and asked for again, costs a timer or a
/// round trip each, and many of those over the chat's writes exceed it.
const OPENED_TO_READ: Duration = Duration::from_secs(3);

/// A page of `name` entering `workspace` through its door.
async fn arrive(
    host: Host,
    host_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> Link {
    let (root, key) = (new_key(), new_key());
    let (cert, entry) = door_entry(
        host_cert,
        workspace,
        &key,
        Entering {
            held: None,
            root: Some(&root),
            label: "Browser",
            name,
            token: None,
        },
    )
    .await
    .unwrap();
    enter_page(
        host,
        cert,
        host_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    )
}

/// A person coming through `workspace`'s door with a device of their own,
/// leaving no session behind: the member the workspace made of them.
async fn through_the_door(
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    name: &str,
) {
    use tilo_relay_protocol::workspaces::JoinReply;
    let root = new_key();
    let key = new_key();
    let cert = tilo_relay_client::invite::guest_certificate(
        &root,
        key.verifying_key().to_bytes(),
        "Browser",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let (issued, token) = workspaces
        .admit_listed(
            workspace,
            &tilo_relay_protocol::directory::DoorJoin {
                user_public_key: hex::encode(cert.user_public_key),
                display_name: name.to_owned(),
            },
        )
        .await
        .unwrap();
    let admitted = workspaces
        .admit_proven(
            issued.signed_invitation,
            cert,
            rand::random(),
            name,
            Some(token.as_bytes()),
        )
        .await;
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
}

/// The chat's history as the guest's page reads it: the page reads a record
/// each time it lands and takes what that read gives, so a landing whose read
/// is refused leaves the chat as it was until the record lands again. The
/// guest opens the chat's head and its newest part's route as it enters, as
/// a Chat window on the desk does, and the chat must read whole by the
/// landing after which nothing more arrives.
async fn chat_read_by_guest(page: &Link, workspace_id: WorkspaceId, chat: &DocumentRoute) -> Duration {
    let head = DocumentRoute {
        record: neighbour_wire::chain_head(CHAT_CHAIN),
        ..chat.clone()
    };
    let opened = Instant::now();
    page.send(Cmd::Join(workspace_id)).unwrap();
    page.send(Cmd::OpenRoute(chat.clone())).unwrap();
    page.send(Cmd::OpenRecordAt(workspace_id, head.clone()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let (mut shown, mut answered, mut refusals) = (false, false, Vec::new());
    let mut quiet_since = Instant::now();
    loop {
        for event in page.drain() {
            match event {
                Event::Landed { route, .. } if route == *chat => {
                    quiet_since = Instant::now();
                    match page
                        .engine()
                        .record(&chat.cluster_id, &chat.app_id, &chat.record)
                    {
                        Ok(Some(bytes)) => shown = holds_every_message(&bytes),
                        // Still arriving (its keys): it lands again.
                        Ok(None) => {}
                        Err(error) => refusals.push(error),
                    }
                }
                Event::RecordOpened {
                    route: Some(route), ..
                } if route == head => answered = true,
                _ => {}
            }
        }
        if shown {
            return opened.elapsed();
        }
        // The host sent everything of the chat, and nothing of it landed for
        // a while: what the page shows is final.
        assert!(
            !(answered && quiet_since.elapsed() > Duration::from_secs(5)),
            "the guest's page never read the chat whole; its reads were refused: {refusals:?}"
        );
        assert!(
            Instant::now() < deadline,
            "the chat never landed whole at the guest (refused: {refusals:?}); its channels:\n{}",
            page.status().channels
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Whether `bytes`, the chat as the guest's page reads it, holds every message.
fn holds_every_message(bytes: &[u8]) -> bool {
    use automerge::ReadDoc as _;
    automerge::AutoCommit::load(bytes)
        .is_ok_and(|doc| doc.keys(automerge::ROOT).count() >= MESSAGES)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_fresh_guest_reads_the_chat_it_opens_as_it_enters() {
    let members = env_usize("TILO_GUEST_CHAT_MEMBERS", 300);
    let guests = env_usize("TILO_GUEST_CHAT_GUESTS", 8);
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::DeviceCapabilities::host(),
    );
    let state = crate::support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_relay_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let address = host.relay.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let page_host = || Host::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let chat = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: lounge.workspace.clone(),
        record: neighbour_wire::record_part(CHAT_CHAIN, 0),
    };

    // The first member writes the chat; the rest of the crowd came through
    // the door.
    let author = arrive(
        page_host(),
        &host_device.cert,
        &lounge.workspace,
        "Member 0",
    )
    .await;
    author.send(Cmd::Join(lounge.workspace_id)).unwrap();
    let mut history = automerge::AutoCommit::new();
    for message in 0..MESSAGES {
        use automerge::transaction::Transactable as _;
        let before = history.get_heads();
        history
            .put(automerge::ROOT, format!("message-{message}"), "hello")
            .unwrap();
        history.commit();
        let post = if before.is_empty() {
            DocumentMutation::Snapshot(history.save())
        } else {
            DocumentMutation::Incremental(history.save_after(&before))
        };
        written(&author, &chat, &post).await;
    }
    wait_keyed(&author, &chat).await;
    let mut arriving = tokio::task::JoinSet::new();
    for n in 1..members {
        while arriving.len() >= ARRIVING {
            arriving.join_next().await.unwrap().unwrap();
        }
        let (doors, workspace) = (workspaces.clone(), lounge.workspace.clone());
        arriving.spawn(async move {
            through_the_door(&doors, &workspace, &format!("Member {n}")).await;
        });
    }
    while let Some(arrived) = arriving.join_next().await {
        arrived.unwrap();
    }

    for guest in 0..guests {
        let started = Instant::now();
        let page = arrive(
            page_host(),
            &host_device.cert,
            &lounge.workspace,
            &format!("Guest {guest}"),
        )
        .await;
        let read = chat_read_by_guest(&page, lounge.workspace_id, &chat).await;
        eprintln!(
            "guest {guest} read the chat {read:?} after opening it, {:?} after arriving",
            started.elapsed()
        );
        assert!(
            read < OPENED_TO_READ,
            "guest {guest} read the chat only {read:?} after opening it"
        );
        page.close();
    }
    author.close();
}
