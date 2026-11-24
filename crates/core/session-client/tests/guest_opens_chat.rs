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
//! its entry, one of whom wrote the chat's 200 messages, each a write of its
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
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_server_client::ServerAddress;
use tilo_session_client::device::{Entering, entry_request};
use tilo_session_client::{Cmd, Event, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{
    admitted_to, commit_native, durable, enrol, enter_page, start_workspace_host_relaying,
    wait_keyed,
};
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

/// How many people come through the entry at once while the crowd is made.
const ARRIVING: usize = 16;

/// The messages the chat holds, each written on its own as a member posts it.
const MESSAGES: usize = 200;

/// The longest a guest's page may take to read the chat whole after it opens it, on a host in
/// the same process: the host sending what it holds and the page taking it in. A refused read
/// waiting for the next landing, or a write let go and asked for again, costs a timer or a
/// round trip each, and many of those over the chat's writes exceed it.
const OPENED_TO_READ: Duration = Duration::from_secs(3);

/// A page of `name` entering `workspace` through its entry.
async fn arrive(
    host: ServerAddress,
    server_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> Link {
    let (root, key) = (new_key(), new_key());
    let (cert, entry) = entry_request(
        server_cert,
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
        server_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    )
}

/// A person coming through `workspace`'s entry with a device of their own,
/// leaving no session behind: the member the workspace made of them.
async fn through_the_entry(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    name: &str,
) {
    use tilo_server_protocol::workspaces::JoinReply;
    let root = new_key();
    let key = new_key();
    let cert = tilo_server_client::invite::guest_certificate(
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
            &tilo_server_protocol::directory::EntryJoin {
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
/// a Chat window on the layout does, and the chat must read whole by the
/// landing after which nothing more arrives.
async fn chat_read_by_guest(
    page: &Link,
    workspace_id: WorkspaceId,
    chat: &DocumentRoute,
) -> Duration {
    let head = DocumentRoute {
        document: neighbour_wire::chain_head(CHAT_CHAIN),
        ..chat.clone()
    };
    let opened = Instant::now();
    let events = page.events();
    page.send(Cmd::Join(workspace_id)).unwrap();
    page.send(Cmd::OpenRoute(chat.clone())).unwrap();
    page.send(Cmd::OpenDocumentAt(workspace_id, head.clone()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let (mut shown, mut answered, mut refusals) = (false, false, Vec::new());
    let mut quiet_since = Instant::now();
    loop {
        for event in events.drain() {
            match event {
                Event::Landed { route, .. } if route == *chat => {
                    quiet_since = Instant::now();
                    match crate::support::reading_of(page, chat, None) {
                        Ok(DocumentRead::Whole { bytes, .. }) => {
                            shown = holds_every_message(&bytes);
                        }
                        // Still arriving (its keys): it lands again.
                        Ok(_) => {}
                        Err(error) => refusals.push(error),
                    }
                }
                Event::DocumentOpened {
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
            page.status().key_state
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
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_server_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let address = host.server.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let server_address =
        || ServerAddress::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let chat = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: lounge.workspace.clone(),
        document: neighbour_wire::document_part(CHAT_CHAIN, 0),
    };

    // The first member writes the chat; the rest of the crowd came through
    // the entry.
    let author = arrive(
        server_address(),
        &host_device.cert,
        &lounge.workspace,
        "Member 0",
    )
    .await;
    author.send(Cmd::Join(lounge.workspace_id)).unwrap();
    // The chat's history is what the host holds once it is durable: the author opens the chat
    // and holds its keys, says its messages one behind the other, each still a write of its own,
    // and one barrier covers them all.
    // It writes once its host let it into the workspace and replayed it: a write said before that
    // can be refused for the governance the page does not hold yet, and is never answered.
    admitted_to(&author, lounge.workspace_id).await;
    author.send(Cmd::OpenRoute(chat.clone())).unwrap();
    wait_keyed(&author, &chat).await;
    let mut history = automerge::AutoCommit::new();
    for message in 0..MESSAGES {
        use automerge::transaction::Transactable as _;
        let before = history.get_heads();
        history
            .put(automerge::ROOT, format!("message-{message}"), "hello")
            .unwrap();
        commit_native(&mut history);
        let post = if before.is_empty() {
            DocumentMutation::Snapshot(history.save())
        } else {
            DocumentMutation::Incremental(history.save_after(&before))
        };
        author.write(chat.clone(), &post).unwrap();
    }
    durable(&author).await;
    wait_keyed(&author, &chat).await;
    let mut arriving = tokio::task::JoinSet::new();
    for n in 1..members {
        while arriving.len() >= ARRIVING {
            arriving.join_next().await.unwrap().unwrap();
        }
        let (entries, workspace) = (workspaces.clone(), lounge.workspace.clone());
        arriving.spawn(async move {
            through_the_entry(&entries, &workspace, &format!("Member {n}")).await;
        });
    }
    while let Some(arrived) = arriving.join_next().await {
        arrived.unwrap();
    }

    for guest in 0..guests {
        let started = Instant::now();
        let page = arrive(
            server_address(),
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
