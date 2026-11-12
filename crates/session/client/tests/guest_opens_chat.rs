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
//! its door, one of whom wrote the chat. Each guest opens the chat's head
//! with its entry, and every landing of the chat must read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_session_client::device::{Entering, door_entry};
use tilo_session_client::{Cmd, Event, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use support::{enrol, enter_page, start_workspace_host_relaying, wait_keyed, written};

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
async fn chat_read_by_guest(page: &Link, workspace_id: u64, chat: &DocumentRoute) {
    let head = DocumentRoute {
        record: neighbour_wire::chain_head(CHAT_CHAIN),
        ..chat.clone()
    };
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
                        Ok(Some(_)) => shown = true,
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
            return;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_fresh_guest_reads_the_chat_it_opens_as_it_enters() {
    let members = env_usize("TILO_GUEST_CHAT_MEMBERS", 300);
    let guests = env_usize("TILO_GUEST_CHAT_GUESTS", 8);
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = support::scratch_dir();
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
    {
        use automerge::transaction::Transactable as _;
        for message in 0..200 {
            history
                .put(automerge::ROOT, format!("message-{message}"), "hello")
                .unwrap();
        }
    }
    written(&author, &chat, &DocumentMutation::Snapshot(history.save())).await;
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
        chat_read_by_guest(&page, lounge.workspace_id, &chat).await;
        eprintln!("guest {guest} read the chat after {:?}", started.elapsed());
        page.close();
    }
    author.close();
}
