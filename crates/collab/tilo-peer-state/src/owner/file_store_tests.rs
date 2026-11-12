//! The owner's record on disk: an index and one file per workspace, written
//! apart. A change writes its own workspace's file and the index; a record
//! written as one file before the split reads once and is kept split, with
//! the file it came from left as it is. A later change of a workspace alone
//! appends to that workspace's journal.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use super::{index_of, part_bytes, parts_of};
use crate::owner::{
    NativeAuthorityStorage, NativeWorkspaceOwner, WorkspaceId, open_file, read_file, testing,
};
use collab_types::signer::at_once;
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

const PREDECESSOR: &str =
    include_str!("../../tests/fixtures/owner-record/single-file.owner.json.hex");
const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";
const RESEARCH: &str = "7fdaf11b-68aa-4e34-b01d-4ee2424fadcf";
const JOINING: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn directory() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn record_path(directory: &tempfile::TempDir) -> PathBuf {
    directory.path().join("state.owner.json")
}

/// The record the build before the split wrote, as one file.
fn predecessor() -> Vec<u8> {
    PREDECESSOR
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

/// The record's index on disk.
fn index_path(path: &Path) -> PathBuf {
    index_of(&parts_of(path))
}

fn index_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(index_path(path)).unwrap()
}

/// The index on disk.
fn index(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&index_bytes(path)).unwrap()
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

fn name(owner: &NativeWorkspaceOwner, workspace: WorkspaceId) -> Option<String> {
    owner
        .with_current(workspace, |view| {
            Ok(view.peer().governance().workspace_name())
        })
        .unwrap()
}

#[test]
fn a_record_written_as_one_file_reads_once_and_is_then_kept_split() {
    let directory = directory();
    let path = record_path(&directory);
    std::fs::write(&path, predecessor()).unwrap();

    // Opening it splits it: one file per workspace and its journal, then the
    // index that names them.
    let (opened, replaced) = replacements(|| open_file(&path));
    let owner = opened.unwrap();
    assert_eq!(
        replaced, 5,
        "two workspace files, their journals and the index"
    );

    let record = read_file(&path).unwrap();
    assert_eq!(record.revision(), 3);
    assert!(record.holds(workspace(FOUNDERS)));
    assert!(record.holds(workspace(RESEARCH)));
    assert!(record.requires(workspace(JOINING)));
    assert_eq!(
        name(&owner, workspace(FOUNDERS)).as_deref(),
        Some("Founders")
    );
    assert_eq!(
        name(&owner, workspace(RESEARCH)).as_deref(),
        Some("Research")
    );

    let index = index(&path);
    assert_eq!(
        index.get("version").and_then(serde_json::Value::as_u64),
        Some(2)
    );
    assert_eq!(
        index.get("revision").and_then(serde_json::Value::as_u64),
        Some(3)
    );
    assert_eq!(
        index
            .get("held")
            .and_then(|held| held.as_object())
            .map(serde_json::Map::len),
        Some(2)
    );
    assert_eq!(
        part_files(&path).keys().cloned().collect::<Vec<_>>(),
        vec![format!("{FOUNDERS}.3.json"), format!("{RESEARCH}.3.json")]
    );
    // The record was 16208 bytes as one file; the index that a publication
    // rewrites beside the workspaces is smaller than it, and the file the
    // record came from is left as it is.
    assert!(index_bytes(&path).len() < predecessor().len());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        predecessor(),
        "the file the record came from is not rewritten"
    );

    // Reading a split record again publishes nothing.
    drop(owner);
    let (reopened, replaced) = replacements(|| open_file(&path));
    assert_eq!(replaced, 0);
    assert_eq!(reopened.unwrap().storage().held().count(), 2);
}

#[tokio::test]
async fn a_change_writes_only_its_own_workspace_file_and_the_index() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let first = testing::founded(&owner, "One", &device, &key, now)
        .await
        .unwrap();
    let second = testing::founded(&owner, "Two", &device, &key, now)
        .await
        .unwrap();
    let before = part_files(&path);
    assert_eq!(before.len(), 2);
    let journals_before = journal_files(&path);

    let (created, replaced) =
        replacements(|| at_once(testing::founded(&owner, "Three", &device, &key, now)));
    created.unwrap().unwrap();
    assert_eq!(
        replaced, 3,
        "the workspace's file, its journal and the index"
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

#[tokio::test]
async fn a_publication_that_stops_before_its_index_leaves_the_record_as_it_was() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let first = testing::founded(&owner, "One", &device, &key, now)
        .await
        .unwrap();
    let durable = index_bytes(&path);

    // The workspace's journal and file are written; the index that would
    // name them is not.
    let stopped = with_failure_on_nth(Checkpoint::BeforePublish, 3, || {
        at_once(testing::founded(&owner, "Two", &device, &key, now)).unwrap()
    });
    assert!(stopped.is_err());
    assert_eq!(index_bytes(&path), durable);
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
    testing::founded(&reopened, "Two", &device, &key, now)
        .await
        .unwrap();
    assert_eq!(part_files(&path).len(), 2);
    assert_eq!(read_file(&path).unwrap().held().count(), 2);
}

#[tokio::test]
async fn a_workspace_costs_its_own_record() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, "Founders", &device, &key, now)
        .await
        .unwrap();
    let record = read_file(&path).unwrap();
    let founded_bytes = part_bytes(record.workspaces.get(&founded).unwrap())
        .unwrap()
        .len();
    let index_one = index_bytes(&path).len();

    // A second person joins a workspace: the record this device keeps of it
    // is what a workspace with two people costs.
    let _governor = testing::enroll_interactive_governor(&owner, &key, &device).unwrap();
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

#[tokio::test]
async fn a_split_record_leaves_the_file_it_came_from_for_an_older_build() {
    let directory = directory();
    let path = record_path(&directory);
    std::fs::write(&path, predecessor()).unwrap();
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    testing::founded(&owner, "Third", &device, &key, now)
        .await
        .unwrap();
    drop(owner);

    // A build before the split reads the file it wrote, whole, and holds two
    // workspaces: the split wrote the record beside that file, not over it.
    let older: NativeAuthorityStorage =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(older.revision(), 3);
    assert_eq!(older.held().count(), 2);

    // That build publishing over its file, as one does after a rollback,
    // does not take the record with it: this build reads its index.
    let split = read_file(&path).unwrap();
    assert_eq!(split.held().count(), 3);
    std::fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    let reread = read_file(&path).unwrap();
    assert_eq!(reread.revision(), split.revision());
    assert_eq!(reread.held().count(), 3);
}

#[tokio::test]
async fn a_released_workspace_leaves_no_file_behind() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, "One", &device, &key, SystemTime::now())
        .await
        .unwrap();
    assert!(owner.release_workspace(workspace).unwrap());

    assert!(part_files(&path).is_empty());
    assert!(read_file(&path).unwrap().is_empty());
    assert_eq!(open_file(&path).unwrap().storage().held().count(), 0);
}

#[tokio::test]
async fn an_index_that_names_a_missing_workspace_file_is_not_a_record() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let owner = open_file(&path).unwrap();
    testing::founded(&owner, "One", &device, &key, SystemTime::now())
        .await
        .unwrap();
    drop(owner);
    let files = part_files(&path);
    let (name, _) = files.iter().next().unwrap();
    std::fs::remove_file(parts_of(&path).join(name)).unwrap();
    assert!(read_file(&path).is_err());
    assert!(open_file(&path).is_err());
}

/// A change of one workspace after its file was written appends to that
/// file's journal, replacing no file, and reads back after a restart.
#[tokio::test]
async fn a_later_change_appends_to_its_workspace_journal() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, "One", &device, &key, now)
        .await
        .unwrap();
    let files = part_files(&path);
    let index = index_bytes(&path);
    let journal = |path: &Path| journal_files(path).into_values().next().unwrap();
    let empty = journal(&path);

    testing::start_record(&owner, founded, "so.tilo.chat", "a", &key, now);
    let one = journal(&path);
    testing::start_record(&owner, founded, "so.tilo.chat", "b", &key, now);
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
#[tokio::test]
async fn a_journal_entry_cut_short_is_not_part_of_the_record() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, "One", &device, &key, now)
        .await
        .unwrap();
    testing::start_record(&owner, founded, "so.tilo.chat", "a", &key, now);
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
    testing::start_record(&reopened, founded, "so.tilo.chat", "b", &key, now);
    drop(reopened);
    let reopened = open_file(&path).unwrap();
    assert!(held(&reopened, "a") && held(&reopened, "b"));
}

/// A record an earlier build kept, its index naming the workspace files
/// alone, reads as it was, and its next change starts a journal.
#[tokio::test]
async fn an_index_before_journals_reads_and_gains_them() {
    let directory = directory();
    let path = record_path(&directory);
    let (key, device) = device();
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let founded = testing::founded(&owner, "One", &device, &key, now)
        .await
        .unwrap();
    drop(owner);
    for name in journal_files(&path).keys() {
        std::fs::remove_file(parts_of(&path).join(name)).unwrap();
    }
    let mut index = index(&path);
    index["version"] = 1.into();
    std::fs::write(index_path(&path), serde_json::to_vec(&index).unwrap()).unwrap();

    let reopened = open_file(&path).unwrap();
    assert_eq!(name(&reopened, founded).as_deref(), Some("One"));
    testing::start_record(&reopened, founded, "so.tilo.chat", "a", &key, now);
    assert_eq!(journal_files(&path).len(), 1);
    drop(reopened);
    let reopened = open_file(&path).unwrap();
    assert!(
        reopened
            .with_current(founded, |view| {
                Ok(view.peer().document("so.tilo.chat", "a").is_some())
            })
            .unwrap()
    );
}
