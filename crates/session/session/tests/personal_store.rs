//! The session keeps the person's own records in their store, and keeps that store open to their
//! other devices: a contact an app adds in Settings' own record lands in the store this device
//! serves, beside the session's state.
//! The store's link itself is answered by the session's endpoint on the pass it binds, so a session
//! that runs keeps one for as long as it does; this test starts a real session and drives the app
//! that writes.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

// A host admits its members' records through an Automerge worker: this binary is that worker, as it
// is the session's own.
tilo_automerge_isolate::test_worker!();

mod common;

use std::path::Path;

use automerge::transaction::Transactable as _;

use common::personal::{self, Personal};

/// Contacts, and the record kind of the person's it keeps: an identity they added, which is theirs
/// alone until their other devices hear of it.
const CONTACTS: &str = "so.tilo.contacts";
const KIND: &str = "tilo.contacts.identities";

/// What Contacts declares of the person's own records, as its package ships.
const SURFACE: &[u8] = b"[[resources]]\nkind = \"tilo.contacts.identities\"\nlabel = \"Contact identities\"\nversion = 2\nperson = true\nfields = []\n";

/// A signed change of the record, authored by `device` , at the sequence a document signer takes.
fn op(device: &ed25519_dalek::SigningKey, cluster: &str, key: &str, value: &str) -> Vec<u8> {
    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, key, value)
        .expect("the record takes the key");
    document.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Native.change_message()),
    );
    // An app's first write of a record it created is the record whole, as every participant's is.
    let mutation = collab_types::DocumentMutation::Snapshot(document.save())
        .encode()
        .expect("a mutation encodes");
    let author = collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
    let now = std::time::SystemTime::now();
    let mut op = collab_types::SignedOp::new_unsigned(
        mutation,
        collab_types::document_artifact_id(CONTACTS, cluster, &personal::record(KIND)),
        author,
        collab_types::document_attestation_id(CONTACTS, cluster, &personal::record(KIND)),
        collab_types::OpKind::Change,
        now,
        collab_types::next_unordered_document_sequence(now),
    );
    collab_types::sign_op(&mut op, device).expect("the app signs its change");
    postcard::to_stdvec(&op).expect("an op encodes for the wire")
}

/// The person adds a contact in Contacts, on this device.
fn add_contact(socket: &Path, machine: &Personal, cluster: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = personal::connect(socket, CONTACTS, cluster);
        let handle = client
            .join_channel(
                &personal::channel(CONTACTS, cluster, KIND),
                &personal::presentation(&machine.device, &machine.cert, cluster),
            )
            .await
            .expect("Contacts joins the person's own record");
        handle
            .send(&op(&machine.device, cluster, "name", "Ada"))
            .await
            .expect("the session admits the contact");
    });
}

#[test]
fn a_contact_added_here_is_kept_in_the_persons_own_store() {
    let machine = Personal::new("Ada");
    let cluster = machine.cluster();
    // Contacts declares the kind of the record the app writes, as its package ships it: what the
    // session admits the join against.
    tilo_action_broker::register_package_surface(CONTACTS, SURFACE);
    let socket = machine.socket().to_path_buf();
    let state = tempfile::tempdir().expect("a state for this session");
    let mut session = personal::session(&socket, "Ada", &cluster, state.path(), CONTACTS, false);

    add_contact(&socket, &machine, &cluster);

    // The record reaches the person's own store, written beside the session's state as it changes:
    // what this device serves their other devices.
    let path = personal::store_path(state.path());
    let bytes = common::wait_for(&mut session, "the person's store", |session| {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(5))
            .expect("step");
        std::fs::read(&path).ok()
    });
    let store = tilo_peer_state::PersonalStore::restore(
        &bytes,
        &machine.cert,
        std::time::SystemTime::now(),
    )
    .expect("the store this session keeps is this device's to open");
    assert_eq!(
        store.store_key(),
        cluster,
        "it is the store of the person whose records route on their own cluster"
    );
    assert!(
        store.holds_document(CONTACTS, &personal::record(KIND)),
        "the contact the app added is in the person's own store, for their other devices"
    );
}
