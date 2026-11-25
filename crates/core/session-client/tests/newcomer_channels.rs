//! A new visitor's page in a workspace's channel group, as the Lounge has it:
//! the page only follows the group (it admits no one), so its host sends it no
//! member's request to be admitted and its own Welcome once. What it needs of
//! the group it still has: it reads the shared desktop, writes to it once keyed,
//! follows the group's later epochs as others arrive, and keeps all of that
//! after its connection drops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one visit, in order; the host lives until it ends"
)]

use std::sync::Arc;
use tilo_session_client::{Cmd, Link, Outbox};

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::MemoryPeerStore;
use tilo_session_client::device::{Entering, entry_request};
use tilo_user_identity::{DeviceCert, UserIdentity};

use crate::support::{
    enrol, enter_page, hears, in_workspace, person_of, set_field, start_workspace_host_relaying,
    wait_for, wait_keyed,
};
use collab_types::WorkspaceId;

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// A change to the desktop of `base`, in an entry `key` of its own, by a
/// writer of its own.
fn change(base: &mut automerge::AutoCommit, writer: &Link, key: &str) -> DocumentMutation {
    let mut doc = base.fork();
    doc.save_incremental();
    set_field(&mut doc, person_of(writer), "desktop", key, key);
    DocumentMutation::Incremental(doc.save_incremental())
}

/// A visitor's page entering `workspace` through its entry, its session
/// opening the desktop.
async fn visitor(
    host: &crate::support::TestHost,
    server_cert: &DeviceCert,
    workspace: &str,
    workspace_id: WorkspaceId,
    desktop: &DocumentRoute,
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
    let page = enter_page(
        host.iroh(),
        cert,
        server_cert.clone(),
        key,
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    page.send(Cmd::Join(workspace_id)).unwrap();
    page.send(Cmd::OpenRoute(desktop.clone())).unwrap();
    in_workspace(&page, workspace_id).await;
    wait_keyed(&page, desktop).await;
    page
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newcomer_reads_writes_follows_and_rejoins_its_channel_group() {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    // The host takes part in its workspaces' channels, as a hosted
    // workspace host does: it admits each page.
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
    let desktop = DocumentRoute {
        app_id: "so.tilo.browser-desktop".into(),
        cluster_id: lounge.workspace.clone(),
        document: "desktop".into(),
    };
    let server_cert = host_device.cert.clone();
    let enter = |name: &'static str| {
        visitor(
            &host,
            &server_cert,
            &lounge.workspace,
            lounge.workspace_id,
            &desktop,
            name,
        )
    };

    // A member writes the desktop first.
    let member = enter("Member").await;
    let mut base = {
        let mut doc = automerge::AutoCommit::new();
        set_field(&mut doc, person_of(&member), "desktop", "windows", "none");
        doc
    };
    member
        .write(desktop.clone(), &DocumentMutation::Snapshot(base.save()))
        .unwrap();

    // The newcomer reads it.
    let newcomer = enter("Newcomer").await;
    hears(&newcomer, &desktop, "windows").await;

    // Keyed, it writes, and the member reads that.
    let written = change(&mut base, &newcomer, "newcomer");
    newcomer.write(desktop.clone(), &written).unwrap();
    hears(&member, &desktop, "newcomer").await;

    // Someone else arrives: the group moves to a later epoch, which the
    // newcomer follows, reading what is sealed under it.
    let later = enter("Later").await;
    let after = change(&mut base, &later, "later");
    later.write(desktop.clone(), &after).unwrap();
    hears(&newcomer, &desktop, "later").await;

    // Its connection drops; once back it reads and writes as before.
    newcomer.send(Cmd::Reconnect).unwrap();
    in_workspace(&newcomer, lounge.workspace_id).await;
    wait_keyed(&newcomer, &desktop).await;
    let again = change(&mut base, &member, "member-again");
    member.write(desktop.clone(), &again).unwrap();
    hears(&newcomer, &desktop, "member-again").await;
    let back = change(&mut base, &newcomer, "newcomer-back");
    newcomer.write(desktop.clone(), &back).unwrap();
    hears(&later, &desktop, "newcomer-back").await;

    for page in [member, newcomer, later] {
        page.close();
    }
    drop(host.web);
    host.server.shutdown().await;
}

/// A page that entered a workspace through its entry asks for that workspace's
/// live session once it holds the workspace's keys, and joins it: presence, calls
/// and the people overlay all ride it. The page's channel state names the live session
/// it joined, and another member hears the page's presence there — a page that
/// never asked for a live session lists no one, and nobody hears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newcomer_asks_for_its_workspaces_live_session_and_joins_it() {
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
    let desktop = DocumentRoute {
        app_id: "so.tilo.browser-desktop".into(),
        cluster_id: lounge.workspace.clone(),
        document: "desktop".into(),
    };
    let server_cert = host_device.cert.clone();

    let member = visitor(
        &host,
        &server_cert,
        &lounge.workspace,
        lounge.workspace_id,
        &desktop,
        "Member",
    )
    .await;
    let newcomer = visitor(
        &host,
        &server_cert,
        &lounge.workspace,
        lounge.workspace_id,
        &desktop,
        "Newcomer",
    )
    .await;

    // The joined live session's line: the epoch it seals under and the devices it reads.
    let named = format!("live session {}: epoch ", lounge.workspace_id);
    let joined = wait_for("the page to join its workspace's live session", || {
        let line = newcomer
            .status()
            .key_state
            .lines()
            .find(|line| line.starts_with(&named))
            .map(str::to_owned);
        async move { line }
    })
    .await;
    assert!(joined.contains("device(s)"), "{joined}");

    // The page is on the live session: its presence reaches another member of the
    // workspace, on the workspace scope's track.
    let events = member.events();
    let said = collab_types::presence::Presence::pointing(&lounge.workspace, 1, 0.5, 0.5);
    wait_for(
        "the member to hear the newcomer on the live session",
        || {
            newcomer
                .send(Cmd::Presence(lounge.workspace_id, vec![said.clone()]))
                .unwrap();
            let events = events.drain();
            async move {
                events.into_iter().find_map(|event| match event {
                    tilo_session_client::Event::LiveSession { from, .. } => Some(from),
                    _ => None,
                })
            }
        },
    )
    .await;

    for page in [member, newcomer] {
        page.close();
    }
    drop(host.web);
    host.server.shutdown().await;
}
