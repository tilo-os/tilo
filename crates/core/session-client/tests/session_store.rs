//! A computer's device keeps its workspaces for as long as its session runs, and no copy of them
//! besides.
//!
//! `tilo login`'s device starts a session over a store that no later session starts over. The
//! session still made the whole record of the device's workspaces, every record it replicates in
//! it, after each write, and kept it in that store: a folder of twenty thousand files written
//! into one record made that record anew for every file, faster than the computer could give the
//! memory back, until the holder of the session ran out of it.

use std::sync::Arc;

use automerge::{AutoCommit, transaction::Transactable as _};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_session_client::device::{Entering, entry_request};
use tilo_session_client::membership_check::PeerStore as _;
use tilo_session_client::{Cmd, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use super::support::{
    commit_native, durable, enrol, enter_page, start_workspace_host_relaying, wait_keyed,
};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// A device of `name` entering `workspace` through its entry, its session over `store`.
async fn arrive(
    host: ServerAddress,
    server_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
    store: Arc<MemoryPeerStore>,
) -> Link {
    let (root, key) = (new_key(), new_key());
    let (cert, entry) = entry_request(
        server_cert,
        workspace,
        &key,
        Entering {
            held: None,
            root: Some(&root),
            label: "Computer",
            name,
            token: None,
        },
    )
    .await
    .unwrap();
    enter_page(host, cert, server_cert.clone(), key, store, entry)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_over_a_store_for_it_alone_keeps_no_copy_of_its_workspaces() {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = super::support::scratch_dir();
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
    let server_at =
        || ServerAddress::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let files = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: lounge.workspace.clone(),
        document: String::new(),
    };

    // A computer's session, and a page's whose profile a reload starts over.
    let session_only = Arc::new(MemoryPeerStore::for_session());
    let profile = Arc::new(MemoryPeerStore::default());
    let computer = arrive(
        server_at(),
        &host_device.cert,
        &lounge.workspace,
        "Computer",
        Arc::clone(&session_only),
    )
    .await;
    let page = arrive(
        server_at(),
        &host_device.cert,
        &lounge.workspace,
        "Page",
        Arc::clone(&profile),
    )
    .await;
    for device in [&computer, &page] {
        device.send(Cmd::Join(lounge.workspace_id)).unwrap();
        device.send(Cmd::OpenRoute(files.clone())).unwrap();
        wait_keyed(device, &files).await;
    }

    // The computer writes a folder's tree, a file at a time, as its synced folder does.
    let mut tree = AutoCommit::new();
    for file in 0..40 {
        let before = tree.get_heads();
        tree.put(automerge::ROOT, format!("file-{file}"), "x".repeat(4096))
            .unwrap();
        commit_native(&mut tree);
        let write = if before.is_empty() {
            DocumentMutation::Snapshot(tree.save())
        } else {
            DocumentMutation::Incremental(tree.save_after(&before))
        };
        computer.write(files.clone(), &write).unwrap();
    }
    durable(&computer).await;
    assert!(
        computer.status().failures.is_empty(),
        "every write was taken: {:?}",
        computer.status().failures
    );
    assert!(
        session_only.load_workspaces().is_none(),
        "the computer's session made a copy of its workspaces no later session reads"
    );

    // A page's profile keeps them, for the session a reload starts: the
    // record as the index and parts a later session reads it from.
    let mut note = AutoCommit::new();
    note.put(automerge::ROOT, "kind", "tree").unwrap();
    commit_native(&mut note);
    page.write(files.clone(), &DocumentMutation::Incremental(note.save()))
        .unwrap();
    durable(&page).await;
    assert!(
        tilo_peer_state::owner::Journaled::open(&Parts(Arc::clone(&profile)))
            .expect("the record a page keeps reads back")
            .is_some(),
        "a page's profile keeps its workspaces for its next session"
    );
    for device in [computer, page] {
        device.close();
    }
}

/// The record a page's store keeps as parts, as a later session reads it.
struct Parts(Arc<MemoryPeerStore>);

impl tilo_peer_state::owner::JournalParts for Parts {
    fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.load_workspaces_part(name))
    }
    fn write_part(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn append_part(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
}
