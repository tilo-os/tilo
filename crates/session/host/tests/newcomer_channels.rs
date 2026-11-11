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

mod support;

tilo_automerge_isolate::test_worker!();

use std::sync::Arc;

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_host::device::{Entering, door_entry};
use tilo_session_host::{MemoryPeerStore, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCert, UserIdentity};

use support::{enrol, enter_page, in_workspace, start_workspace_host_relaying, wait_for};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// Whether `page`'s channel for `route` is ready.
fn keyed(page: &PeerHandle, route: &DocumentRoute) -> bool {
    let prefix = format!("route {}/{} ", route.app_id, route.record);
    page.status()
        .channels
        .lines()
        .any(|line| line.starts_with(&prefix) && line.contains(": ready;"))
}

async fn wait_keyed(page: &PeerHandle, route: &DocumentRoute) {
    wait_for("the page keyed for the desktop", || {
        let ready = keyed(page, route);
        async move { ready.then_some(()) }
    })
    .await;
}

/// A change to the desktop of `base`, in an entry `key` of its own, by a
/// writer of its own.
fn change(base: &mut automerge::AutoCommit, key: &str) -> DocumentMutation {
    use automerge::transaction::Transactable as _;
    let mut doc = base.fork();
    doc.save_incremental();
    doc.put(automerge::ROOT, key, key).unwrap();
    DocumentMutation::Incremental(doc.save_incremental())
}

/// Wait until `page` receives `mutation` on `route`, keeping what else came.
async fn receives(page: &PeerHandle, route: &DocumentRoute, mutation: &DocumentMutation) {
    wait_for("the page receives the desktop change", || {
        let found = page.drain().into_iter().any(|event| {
            matches!(&event, PeerEvent::Document(document)
                if document.route == *route && document.mutation().as_ref() == Some(mutation))
        });
        async move { found.then_some(()) }
    })
    .await;
}

/// A visitor's page entering `workspace` through its door, its session
/// opening the desktop.
async fn visitor(
    host: &support::TestHost,
    host_cert: &DeviceCert,
    workspace: &str,
    workspace_id: u64,
    desktop: &DocumentRoute,
    name: &str,
) -> PeerHandle {
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
    let page = enter_page(
        host.iroh(),
        cert,
        host_cert.clone(),
        key,
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    page.join(workspace_id).unwrap();
    page.open_route(desktop.clone()).unwrap();
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
    let desktop = DocumentRoute {
        app_id: "so.tilo.browser-desktop".into(),
        cluster_id: lounge.workspace.clone(),
        record: "desktop".into(),
    };
    let host_cert = host_device.cert.clone();
    let enter = |name: &'static str| {
        visitor(
            &host,
            &host_cert,
            &lounge.workspace,
            lounge.workspace_id,
            &desktop,
            name,
        )
    };

    // A member writes the desktop first.
    let member = enter("Member").await;
    let mut base = {
        use automerge::transaction::Transactable as _;
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "windows", "none").unwrap();
        doc
    };
    member
        .publish(desktop.clone(), &DocumentMutation::Snapshot(base.save()))
        .unwrap();

    // The newcomer reads it.
    let newcomer = enter("Newcomer").await;
    wait_for("the newcomer reads the desktop", || {
        let read = newcomer.drain().into_iter().any(
            |event| matches!(&event, PeerEvent::Document(document) if document.route == desktop),
        );
        async move { read.then_some(()) }
    })
    .await;

    // Keyed, it writes, and the member reads that.
    let written = change(&mut base, "newcomer");
    newcomer.publish(desktop.clone(), &written).unwrap();
    receives(&member, &desktop, &written).await;

    // Someone else arrives: the group moves to a later epoch, which the
    // newcomer follows, reading what is sealed under it.
    let later = enter("Later").await;
    let after = change(&mut base, "later");
    later.publish(desktop.clone(), &after).unwrap();
    receives(&newcomer, &desktop, &after).await;

    // Its connection drops; once back it reads and writes as before.
    newcomer.reconnect().unwrap();
    in_workspace(&newcomer, lounge.workspace_id).await;
    wait_keyed(&newcomer, &desktop).await;
    let again = change(&mut base, "member-again");
    member.publish(desktop.clone(), &again).unwrap();
    receives(&newcomer, &desktop, &again).await;
    let back = change(&mut base, "newcomer-back");
    newcomer.publish(desktop.clone(), &back).unwrap();
    receives(&later, &desktop, &back).await;

    for page in [member, newcomer, later] {
        page.close();
    }
    drop(host.web);
    host.relay.shutdown().await;
}
