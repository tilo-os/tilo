//! A page that holds a record's history written by an app version it does not
//! run asks its workspace for that version's surface by hash, on the same
//! content plane it asks for a picture with, keeps it, and decides what waited
//! for it. The hold names the surface as the reason it waits, so a reader is
//! told what a record wants instead of being told it wants its keys.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; one test is one transcript between a native participant and a page"
)]

use std::sync::Arc;
use std::time::SystemTime;
use tilo_session_client::{Link, Outbox};

use automerge::transaction::Transactable as _;
use collab_types::{
    DocumentMutation, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id,
};
use tilo_relay::workspaces::WorkspaceContent;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::native::NativeDevice;
use crate::support::{WORKSPACE, enrol, ready, route, start_host, start_peer, wait_for, workspace};

/// The surface the laptop's write names: the notes app's own `surface.toml`,
/// a version of it this page does not run.
fn older_surface() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/notes/surface.toml"
    ))
    .unwrap()
}

/// A host that keeps app surfaces, as one that decided changes against them
/// does, answering only once this test lets it: the page's hold on the write,
/// and the request it makes for the surface, are the same while the host has
/// not answered.
struct Keeps {
    bytes: Vec<u8>,
    answers: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl Keeps {
    /// Let the host answer what it was asked.
    fn answer(&self) {
        let (open, answered) = &*self.answers;
        *open.lock().unwrap() = true;
        answered.notify_all();
    }
}

impl WorkspaceContent for Keeps {
    fn read(&self, _workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        let (open, answered) = &*self.answers;
        let mut open = open.lock().unwrap();
        while !*open {
            open = answered.wait(open).unwrap();
        }
        (app == "so.tilo.notes" && hash == blake3::hash(&self.bytes).to_hex().as_str())
            .then(|| self.bytes.clone())
    }
}

/// Whether the page says it holds something for the note, and why.
fn held(peer: &Link) -> Option<String> {
    let note = crate::support::route();
    let held = format!("held {}/{}:", note.app_id, note.record);
    peer.status()
        .key_state
        .lines()
        .find(|line| line.starts_with(&held))
        .map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_fetches_the_surface_the_history_it_holds_was_written_with_and_decides_it() {
    // The page runs no app of its own: every change it receives names a
    // surface it must fetch, the one this test's host keeps.
    tilo_peer_state::decide_with_surfaces(tilo_session_client::surfaces::surfaces(), |_| None);

    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let surface = older_surface();
    let keeps = Arc::new(Keeps {
        bytes: surface.clone(),
        answers: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
    });
    host.relay
        .workspaces()
        .unwrap()
        .serve_content(keeps.clone());

    // The laptop joins the group the host founded, announces itself and
    // writes the record's history, sealed and naming the other version's
    // surface.
    let note = route();
    let artifact = document_artifact_id(&note.app_id, &note.cluster_id, &note.record);
    let laptop_device = laptop.key.verifying_key().to_bytes();
    crate::support::place(&host.relay, &laptop.cert).await;
    let mut laptop = NativeDevice::start(
        &host.relay,
        root.verifying_key().to_bytes(),
        laptop,
        workspace(),
    )
    .await;
    laptop.join().await;

    // The record's history: the record started empty, as an app starts one,
    // then three page edits, each naming a field of the notes app as its
    // executor does, so every edit carries the claim the receiver decides
    // against the app's surface. Every write is sealed with the record's
    // content key.
    let mut document = automerge::AutoCommit::new();
    let mut writes = vec![(DocumentMutation::Snapshot(document.save()), None)];
    for page_number in 1..=3 {
        document
            .put(automerge::ROOT, "pages", format!("page {page_number}"))
            .unwrap();
        collab_types::CommitNamed::commit_edit(&mut document, "notes.notebook", "pages");
        writes.push((
            DocumentMutation::Incremental(document.save_incremental()),
            Some(*blake3::hash(&surface).as_bytes()),
        ));
    }
    for (seq, (mutation, named)) in (1..).zip(writes) {
        let claims = collab_types::mutation_claims(&mutation, collab_types::record_unit).unwrap();
        let frame = mutation.encode().unwrap();
        let (metadata, sealed) = laptop.seal(&note, &frame);
        let op = SignedOp::new_unsigned_with_encryption(
            sealed,
            artifact,
            Ed25519PublicKey::from_bytes(laptop_device),
            document_attestation_id(&note.app_id, &note.cluster_id, &note.record),
            OpKind::Change,
            SystemTime::now(),
            seq,
            metadata,
        )
        .with_claims(claims)
        .with_surface(named);
        laptop.write(&note, op);
    }

    // The page joins: it asks for the workspace's keys though it holds the
    // note's first write, undecided, and the host admits it.
    let page = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&page]).await;
    crate::support::admitted(&page).await;

    // The page holds the write for the surface it does not run, and says so:
    // it waits for the app surface alone, not for keys or governance.
    wait_for("the write held for its app surface", || {
        let held = held(&page);
        async move {
            held.is_some_and(|held| held.contains("for its app surface"))
                .then_some(())
        }
    })
    .await;
    assert!(
        crate::support::said(&page, &note).is_empty(),
        "no edit of the record is opened before its surface arrives"
    );

    // The page asked its workspace for that surface. The host answers, and
    // what the page waited for is decided: it is the bytes hashed to the
    // address asked for that decide it, which the page keeps by that hash.
    keeps.answer();
    crate::support::holds(&page, &note, &document).await;
    wait_for("nothing held for the note", || async {
        held(&page).is_none().then_some(())
    })
    .await;
    assert_eq!(
        tilo_session_client::surfaces::read("so.tilo.notes", &blake3::hash(&surface).to_hex()),
        Some(surface),
        "the page keeps the surface it fetched, by its hash"
    );
}
