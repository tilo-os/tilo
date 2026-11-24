//! The owner's record on disk: an index and one file per workspace, written
//! apart. A change writes its own workspace's file and the index; a later
//! change of a workspace alone appends to that workspace's journal.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use super::{index_of, part_bytes, parts_of};
use crate::owner::{NativeWorkspaceOwner, WorkspaceId, open_file, read_file, testing};
use ed25519_dalek::SigningKey;
use std::{
    cell::Cell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};
use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth, with_observer};
use tilo_user_identity::DeviceCert;

fn directory() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn resource_path(directory: &tempfile::TempDir) -> PathBuf {
    directory.path().join("state.owner.json")
}

/// The record's index on disk.
fn index_path(path: &Path) -> PathBuf {
    index_of(&parts_of(path))
}

fn index_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(index_path(path)).unwrap()
}

/// Every workspace file on disk, by name.
fn part_files(path: &Path) -> BTreeMap<String, Vec<u8>> {
    files_ending(path, ".json")
}

/// Every workspace file's journal on disk, by name.
fn journal_files(path: &Path) -> BTreeMap<String, Vec<u8>> {
    files_ending(path, ".log")
}

fn files_ending(path: &Path, suffix: &str) -> BTreeMap<String, Vec<u8>> {
    let Ok(entries) = std::fs::read_dir(parts_of(path)) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            name != "index.json" && name.to_string_lossy().ends_with(suffix)
        })
        .map(|entry| {
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn device() -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[12; 32]);
    let certificate = testing::certificate(&SigningKey::from_bytes(&[11; 32]), &key, "A's desktop");
    (key, certificate)
}

fn workspace(id: &str) -> WorkspaceId {
    WorkspaceId::parse(id).unwrap()
}

/// Every file replacement the store made while `operation` ran.
fn replacements<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let count = Rc::new(Cell::new(0_usize));
    let observed = Rc::clone(&count);
    let value = with_observer(
        move |event| {
            if event.checkpoint == Checkpoint::BeforePublish {
                observed.set(observed.get().saturating_add(1));
            }
        },
        operation,
    );
    (value, count.get())
}

#[test]
fn a_change_writes_only_its_own_workspace_file_and_the_index() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    let first = testing::founded(&owner, &device, &key).unwrap();
    let second = testing::founded(&owner, &device, &key).unwrap();
    let before = part_files(&path);
    assert_eq!(before.len(), 2);
    let journals_before = journal_files(&path);

    let (created, replaced) = replacements(|| testing::founded(&owner, &device, &key));
    created.unwrap();
    assert_eq!(
        replaced, 4,
        "the index naming the enrollment, then the workspace's file, its journal and the index"
    );

    let after = part_files(&path);
    assert_eq!(after.len(), 3);
    for (name, bytes) in &before {
        assert_eq!(
            after.get(name),
            Some(bytes),
            "{name} is another workspace's file and was not rewritten"
        );
    }

    // A publication writes one workspace's record and the index, not the
    // record: the third file and the index against the whole record.
    let record = read_file(&path).unwrap();
    let written = after
        .iter()
        .chain(&journal_files(&path))
        .filter(|(name, _)| !before.contains_key(*name) && !journals_before.contains_key(*name))
        .map(|(_, bytes)| bytes.len())
        .sum::<usize>()
        + index_bytes(&path).len();
    let whole = serde_json::to_vec(&record).unwrap().len();
    println!(
        "publishing into 3 workspaces writes {written} bytes; the whole record is {whole} bytes"
    );
    assert!(
        written * 2 < whole,
        "a publication writes {written} bytes of a {whole} byte record"
    );
    assert!(record.holds(first) && record.holds(second));
}

#[test]
fn a_publication_that_stops_before_its_index_leaves_the_record_as_it_was() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    let first = testing::founded(&owner, &device, &key).unwrap();

    // The enrollment's index, the workspace's journal and its file are
    // written; the index that would name them is not.
    let stopped = with_failure_on_nth(Checkpoint::BeforePublish, 4, || {
        testing::founded(&owner, &device, &key)
    });
    assert!(stopped.is_err());
    assert_eq!(
        part_files(&path).len(),
        2,
        "the file no index names is kept for now"
    );
    let record = read_file(&path).unwrap();
    assert_eq!(record.held().count(), 1);
    assert!(record.holds(first));

    // Reopening reads one workspace and lets the unnamed file go; the next
    // publication writes the index that names the workspace's file.
    drop(owner);
    let reopened = open_file(&path).unwrap();
    assert_eq!(reopened.storage().held().count(), 1);
    assert_eq!(part_files(&path).len(), 1);
    testing::founded(&reopened, &device, &key).unwrap();
    assert_eq!(part_files(&path).len(), 2);
    assert_eq!(read_file(&path).unwrap().held().count(), 2);
}

#[test]
fn a_workspace_costs_its_own_record() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, &device, &key).unwrap();
    let record = read_file(&path).unwrap();
    let founded_bytes = part_bytes(record.workspaces.get(&founded).unwrap())
        .unwrap()
        .len();
    let index_one = index_bytes(&path).len();

    // A second person joins a workspace: the record this device keeps of it
    // is what a workspace with two people costs.
    let _governor = testing::add_interactive_device(&owner, &key, &device).unwrap();
    let record = read_file(&path).unwrap();
    let joined = workspace(testing::WORKSPACE);
    let joined_bytes = part_bytes(record.workspaces.get(&joined).unwrap())
        .unwrap()
        .len();
    let index_two = index_bytes(&path).len();
    let whole = serde_json::to_vec(&record).unwrap().len();
    println!(
        "a founded workspace's file is {founded_bytes} bytes; a workspace with two people is \
         {joined_bytes} bytes; the index of one workspace is {index_one} bytes and of two \
         {index_two}; both as one file: {whole} bytes"
    );
    assert!(record.holds(founded) && record.holds(joined));
    assert!(
        joined_bytes < 16 * 1024,
        "a two-person workspace's record is {joined_bytes} bytes"
    );
    assert!(
        index_two < 1024,
        "the index of two workspaces is {index_two} bytes"
    );
}

#[test]
fn a_released_workspace_leaves_no_file_behind() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, &device, &key).unwrap();
    assert!(owner.release_workspace(workspace).unwrap());

    assert!(part_files(&path).is_empty());
    assert!(read_file(&path).unwrap().is_empty());
    assert_eq!(open_file(&path).unwrap().storage().held().count(), 0);
}

#[test]
fn an_index_that_names_a_missing_workspace_file_is_not_a_record() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    testing::founded(&owner, &device, &key).unwrap();
    drop(owner);
    let files = part_files(&path);
    let (name, _) = files.iter().next().unwrap();
    std::fs::remove_file(parts_of(&path).join(name)).unwrap();
    assert!(read_file(&path).is_err());
    assert!(open_file(&path).is_err());
}

/// A change of one workspace after its file was written appends to that
/// file's journal, replacing no file, and reads back after a restart.
#[test]
fn a_later_change_appends_to_its_workspace_journal() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, &device, &key).unwrap();
    let files = part_files(&path);
    let index = index_bytes(&path);
    let journal = |path: &Path| journal_files(path).into_values().next().unwrap();
    let empty = journal(&path);

    testing::start_resource(&owner, founded, "so.tilo.chat", "a", &key, now);
    let one = journal(&path);
    testing::start_resource(&owner, founded, "so.tilo.chat", "b", &key, now);
    let two = journal(&path);
    assert!(empty.is_empty());
    assert!(
        two.len() > one.len() && two.starts_with(&one),
        "each change is appended"
    );
    assert_eq!(
        part_files(&path),
        files,
        "the workspace's file is as it was"
    );
    assert_eq!(index_bytes(&path), index, "the index is as it was");
    let revision = owner.storage().revision();
    drop(owner);

    let record = read_file(&path).unwrap();
    assert_eq!(record.revision(), revision);
    let reopened = open_file(&path).unwrap();
    let held = |reopened: &NativeWorkspaceOwner, record: &str| {
        reopened
            .with_current(founded, |view| {
                Ok(view.peer().document("so.tilo.chat", record).is_some())
            })
            .unwrap()
    };
    assert!(held(&reopened, "a") && held(&reopened, "b"));
}

/// An entry a crash cut short was never acknowledged: the record is what the
/// entries before it hold, and the next change is kept after it.
#[test]
fn a_journal_entry_cut_short_is_not_part_of_the_record() {
    let directory = directory();
    let path = resource_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, &device, &key).unwrap();
    testing::start_resource(&owner, founded, "so.tilo.chat", "a", &key, now);
    drop(owner);
    let journals = journal_files(&path);
    let (name, bytes) = journals.iter().next().unwrap();
    let mut torn = bytes.clone();
    torn.extend_from_slice(&[200, 0, 0, 0, 1, 2, 3]);
    std::fs::write(parts_of(&path).join(name), torn).unwrap();

    let reopened = open_file(&path).unwrap();
    let held = |owner: &NativeWorkspaceOwner, record: &str| {
        owner
            .with_current(founded, |view| {
                Ok(view.peer().document("so.tilo.chat", record).is_some())
            })
            .unwrap()
    };
    assert!(held(&reopened, "a"));
    testing::start_resource(&reopened, founded, "so.tilo.chat", "b", &key, now);
    drop(reopened);
    let reopened = open_file(&path).unwrap();
    assert!(held(&reopened, "a") && held(&reopened, "b"));
}
