//! A change a page writes to a sealed record it holds the keys of but not yet the record itself
//! waits for the record, and is never let go.
//!
//! A page that opens a record of a public workspace is admitted to its channel, and can hold its
//! keys before its host's copy of the record lands. A change the page wrote then was refused for
//! the record it changes not being here, and let go; the page's next change, which builds on it,
//! was taken once the record landed. The record then held a change whose base no device ever
//! sends: every reader, a newcomer opening Chat among them, was refused it as incomplete.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use std::time::{Duration, Instant};

use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_session_client::device::{Entering, door_entry};
use tilo_session_client::engine::RecordReading;
use tilo_session_client::{Cmd, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use support::{durable, enrol, enter_page, start_workspace_host_relaying, wait_keyed, written};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

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

/// The change `edit` makes to `doc`, as the write that carries it: the record whole the first
/// time, what it changed after.
fn write(doc: &mut AutoCommit, edit: impl FnOnce(&mut AutoCommit)) -> DocumentMutation {
    let before = doc.get_heads();
    edit(doc);
    doc.commit();
    if before.is_empty() {
        DocumentMutation::Snapshot(doc.save())
    } else {
        DocumentMutation::Incremental(doc.save_after(&before))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_written_before_its_record_lands_waits_for_it() {
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
        record: String::new(),
    };

    let founder = arrive(page_host(), &host_device.cert, &lounge.workspace, "Founder").await;
    let early = arrive(page_host(), &host_device.cert, &lounge.workspace, "Early").await;
    for page in [&founder, &early] {
        page.send(Cmd::Join(lounge.workspace_id)).unwrap();
        page.send(Cmd::OpenRoute(chat.clone())).unwrap();
        wait_keyed(page, &chat).await;
    }

    // The early page holds the record's keys, and no record: nobody founded it yet. It writes a
    // change of the record as it will be founded.
    let mut record = AutoCommit::new();
    let founding = write(&mut record, |doc| {
        doc.put(automerge::ROOT, "kind", "chat").unwrap();
    });
    let mut early_copy = record.clone();
    let first = write(&mut early_copy, |doc| {
        doc.put(automerge::ROOT, "early-1", "first").unwrap();
    });
    assert!(
        !early
            .engine()
            .holds_record(&chat.cluster_id, &chat.app_id, &chat.record),
        "the early page holds no record before it is founded"
    );
    early.write(chat.clone(), &first).unwrap();

    // The record is founded; the early page's first change goes once it lands there, then its
    // second, which builds on the first.
    written(&founder, &chat, &founding).await;
    durable(&early).await;
    let second = write(&mut early_copy, |doc| {
        doc.put(automerge::ROOT, "early-2", "second").unwrap();
    });
    written(&early, &chat, &second).await;
    assert!(
        early.status().failures.is_empty(),
        "the early page's writes were all taken: {:?}",
        early.status().failures
    );

    // A newcomer reads the record whole: the founding and both changes.
    let guest = arrive(page_host(), &host_device.cert, &lounge.workspace, "Guest").await;
    guest.send(Cmd::Join(lounge.workspace_id)).unwrap();
    guest.send(Cmd::OpenRoute(chat.clone())).unwrap();
    guest
        .send(Cmd::OpenRecordAt(lounge.workspace_id, chat.clone()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let last =
            match guest
                .engine()
                .record_update(&chat.cluster_id, &chat.app_id, &chat.record, None)
            {
                Ok(RecordReading::Captured { bytes, .. }) => {
                    let doc = AutoCommit::load(&bytes).unwrap();
                    let keys: Vec<String> = doc.keys(automerge::ROOT).collect();
                    if keys.len() == 3 {
                        assert_eq!(keys, ["early-1", "early-2", "kind"]);
                        break;
                    }
                    format!("read {keys:?}")
                }
                Ok(_) => "nothing to read yet".into(),
                Err(error) => format!("refused: {error}"),
            };
        assert!(
            Instant::now() < deadline,
            "the newcomer never read the record whole; last: {last}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for page in [guest, early, founder] {
        page.close();
    }
}
