//! An owner publication holds the state file's writer across its data sync,
//! never the tree. The frame loop and the network read and write the tree
//! every tick, and a sync on a loaded disk takes seconds: a CI run once
//! caught the tree held for 29 s by a publication, long enough for the
//! network to drop its workspace host as silent. These tests hold a
//! publication at its write, as a slow disk would, through the writer's
//! own test seam.

use super::publication_tests::edit;
use super::tests::{test_enrolled_owner, tree_of};
use super::*;
use crate::shell::persist;
use ed25519_dalek::SigningKey;
use std::{sync::mpsc, time::SystemTime};

/// The workspace added while a publication writes.
const ADDED: &str = "Added while the owner wrote";

/// A document commit published on a thread of its own and held at its
/// state-file write until the returned sender sends or is dropped.
fn commit_held_at_its_write(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    key: &SigningKey,
    now: SystemTime,
) -> (
    mpsc::Sender<()>,
    std::thread::JoinHandle<Result<(), String>>,
) {
    let (request, _) = edit(owner, room, key, now, 7);
    let (writing, at_write) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let publisher = owner.clone();
    let thread = std::thread::spawn(move || {
        store::test_seam::with_before_write(
            move || {
                let _ = writing.send(());
                let _ = released.recv();
            },
            || publisher.commit_document(request, now).map(drop),
        )
    });
    tilo_test_env::recv("the commit to reach its state-file write", &at_write);
    (release, thread)
}

#[test]
fn the_tree_stays_free_while_an_owner_publication_writes() {
    let (_directory, owner, room, now, key) = test_enrolled_owner();
    let tree = tree_of(&owner);
    let (release, publisher) = commit_held_at_its_write(&owner, room, &key, now);

    // Not timed: a tree the publication held could not be taken at all.
    let read = tree.try_read().map(drop).is_some();
    let written = tree.try_write().map(drop).is_some();
    let in_progress = !publisher.is_finished();
    drop(release);
    publisher
        .join()
        .expect("the publishing thread")
        .expect("the commit is published");

    assert!(in_progress, "the commit finished before the tree was taken");
    assert!(read, "the tree could not be read while a publication wrote");
    assert!(
        written,
        "the tree could not be written while a publication wrote"
    );
}

#[test]
fn a_save_queued_while_an_owner_publication_writes_is_not_lost() {
    let (directory, owner, room, now, key) = test_enrolled_owner();
    let path = directory.path().join("state.json");
    let tree = tree_of(&owner);
    persist::wait_for_saves();
    let (release, publisher) = commit_held_at_its_write(&owner, room, &key, now);

    // The frame loop goes on meanwhile: a new workspace, saved as every
    // change is, from a tree that still holds the record being replaced.
    {
        let mut tree = tree.write();
        tree.append_empty_workspace(ADDED);
        tree.seed_doc_from_cache();
    }
    tree.read().queue_save();
    drop(release);
    publisher
        .join()
        .expect("the publishing thread")
        .expect("the commit is published, not superseded by the save");
    persist::wait_for_saves();

    let saved = persist::load_tree_from(&path).expect("the state file loads");
    let (live, runtime) = {
        let tree = tree.read();
        (tree.native_authority.clone(), tree.native_runtime.clone())
    };
    assert!(
        saved
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.name == ADDED),
        "the workspace saved during the publication is not in the state file"
    );
    assert_eq!(
        store::digest(&saved.native_authority).unwrap(),
        store::digest(&live).unwrap(),
        "the state file holds the published record"
    );
    assert!(
        runtime.holds(store::digest(&live).unwrap()),
        "the writer accepted the record the tree holds"
    );
}
