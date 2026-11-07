//! A short-lived workspace is an ordinary workspace held by a store device.
//!
//! The workspace is made on one person's device, with no host service in
//! between; its history is kept, sealed, at a store device that never holds a
//! key. A guest is invited and joins over the workspace-join exchange, reads
//! the whole history back from the store, and keeps reading what changes
//! while it is away. When the device that made the workspace is gone, the
//! guest hands a later guest an invitation of its own: the workspace outlives
//! every device that takes part in it, and a device that has none of its
//! history starts from the store's copy alone.
//!
//! A share is the same thing at another scale: the device that shares an app
//! founds a workspace for it, and the people it shares with join and write in
//! it as members, sealed, while the store keeps every member's line.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::large_futures,
    reason = "in a test these are the assertion, not a hazard; the harness a test builds is one future"
)]
tilo_automerge_isolate::test_worker!();

mod common;

use automerge::transaction::Transactable;
use collab_types::CommitNamed as _;
use common::{
    Held, a_later_guest_reads, addr, endpoint, grant_invite, held, hint, invitation, joins,
    publish, read_document, read_value,
};
use tilo_peer_state::owner::{NativeWorkspaceOwner, open_file};
use tilo_store_link::{StoreSession, workspace_cursors};

/// A guest the maker's device has not yet told about, and what it is.
struct Guest {
    owner: NativeWorkspaceOwner,
    endpoint: iroh::Endpoint,
    session: StoreSession,
}

/// `guest` joins the workspace through the maker's device and catches up on
/// what the store keeps of it. Answers what the guest read.
async fn guest_catches_up(t: &Held) -> (Guest, String) {
    let b = open_file(&t.directory.path().join("b.json")).unwrap();
    let b_endpoint = endpoint(&t.b_key).await;
    joins(
        b_endpoint.clone(),
        &t.b_cert,
        &t.b_key,
        t.workspace,
        invitation(
            t.maker(),
            &t.workspace_id,
            &t.a_cert,
            &t.a_key,
            &t.b_cert,
            hint(
                &t.a_endpoint,
                &t.a_cert,
                &t.a_key,
                &t.workspace_id,
                t.workspace,
            ),
            t.now,
        ),
        &b,
        t.now,
    )
    .await;
    assert!(
        !b.with_current(t.workspace_id, |view| Ok(view.hosted_here()))
            .unwrap(),
        "a guest holds the workspace without having made it"
    );
    let mut session = StoreSession::join(
        &b_endpoint,
        addr(&t.store_endpoint),
        t.b_cert.clone(),
        Vec::new(),
        t.workspace,
    )
    .await
    .unwrap();
    assert_eq!(
        session
            .catch_up_workspace(&b, t.workspace_id, t.now)
            .await
            .unwrap(),
        1,
        "the store replays the checkpoint the workspace's history starts at"
    );
    let read = read_value(&b, t.workspace_id, &t.b_keys);
    (
        Guest {
            owner: b,
            endpoint: b_endpoint,
            session,
        },
        read,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_short_lived_workspace_is_held_by_a_store_device() {
    let mut t = held(
        tempfile::tempdir().unwrap(),
        21,
        "Launch review",
        "before the guests",
    )
    .await;

    // Everything the guest reads of it comes from the store.
    let (mut guest, read) = guest_catches_up(&t).await;
    assert_eq!(read, "\"before the guests\"");
    assert!(
        !workspace_cursors(&guest.owner, t.workspace_id)
            .unwrap()
            .is_empty(),
        "the guest stands at the history it read"
    );

    // The maker writes again after the guest was away, and the store carries
    // it to the guest.
    let heads = t.document.get_heads();
    t.document
        .put(automerge::ROOT, "value", "while B was away")
        .unwrap();
    t.document.commit_edit("fixture", "value");
    let change = t.document.save_after(&heads);
    publish(
        t.maker(),
        t.workspace_id,
        &change,
        &t.keys,
        t.artifact,
        &t.a_key,
        t.now,
    );
    assert!(t.keep().await >= 1);
    assert_eq!(
        guest
            .session
            .catch_up_workspace(&guest.owner, t.workspace_id, t.now)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        read_value(&guest.owner, t.workspace_id, &t.b_keys),
        "\"while B was away\""
    );

    // The maker lets its guest invite, and the guest takes that governance
    // in, before the maker goes away.
    grant_invite(
        &t.maker().clone(),
        &t.a_cert.clone(),
        &t.b_cert.clone(),
        &guest.owner,
        t.workspace_id,
        &t.a_key.clone(),
        t.now,
    );

    // The maker's device is gone: its workspace and its endpoint with it.
    t.maker_goes_away().await;

    // A later guest joins through the first, and reads the workspace from the
    // store, which is the only place its history was.
    let read = a_later_guest_reads(
        &guest.owner,
        &t.b_cert,
        &t.b_key,
        &guest.endpoint,
        (&t.c_cert, &t.c_key, &t.c_keys, "c"),
        &t,
    )
    .await;
    assert_eq!(
        read, "\"while B was away\"",
        "the guest reads everything, from the store alone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_app_is_a_workspace_its_guests_write_in() {
    let mut t = held(
        tempfile::tempdir().unwrap(),
        61,
        "Shared notes",
        "the sharer's line",
    )
    .await;

    // The guest joins by invitation and reads the share from the store.
    let (mut guest, read) = guest_catches_up(&t).await;
    assert_eq!(read, "\"the sharer's line\"");

    // The guest writes too: a member's own sealed line, kept at the store
    // beside the maker's.
    let mut document = read_document(&guest.owner, t.workspace_id, &t.b_keys);
    let heads = document.get_heads();
    document
        .put(automerge::ROOT, "value", "the guest's line")
        .unwrap();
    document.commit_edit("guest", "value");
    let change = document.save_after(&heads);
    publish(
        &guest.owner,
        t.workspace_id,
        &change,
        &t.b_keys,
        t.artifact,
        &t.b_key,
        t.now,
    );
    assert!(
        guest
            .session
            .keep_workspace(&guest.owner, t.workspace_id, t.now)
            .await
            .unwrap()
            >= 1
    );
    assert_eq!(
        t.store.lineages(t.workspace).len(),
        2,
        "the store holds the maker's sealed line and the guest's"
    );

    // The maker catches up from the store and sees the guest's edit.
    t.catch_up().await;
    assert_eq!(
        read_value(t.maker(), t.workspace_id, &t.keys),
        "\"the guest's line\""
    );

    // The guest may hand the share on, as the maker allows before it goes.
    grant_invite(
        &t.maker().clone(),
        &t.a_cert.clone(),
        &t.b_cert.clone(),
        &guest.owner,
        t.workspace_id,
        &t.a_key.clone(),
        t.now,
    );

    // The maker's device is gone; a later guest reads the whole share,
    // written by both, from the store alone.
    t.maker_goes_away().await;
    let read = a_later_guest_reads(
        &guest.owner,
        &t.b_cert,
        &t.b_key,
        &guest.endpoint,
        (&t.c_cert, &t.c_key, &t.c_keys, "c"),
        &t,
    )
    .await;
    assert_eq!(
        read, "\"the guest's line\"",
        "the later guest reads what both members wrote, from the store alone"
    );
}
