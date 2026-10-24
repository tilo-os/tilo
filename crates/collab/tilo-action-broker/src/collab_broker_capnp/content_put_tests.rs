//! The content put door against a store that records what reached it.
#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures hold the fake store's log while asserting on it"
)]
use std::sync::Mutex;

use super::*;
use crate::services::HostServices;
use crate::test_support::{Grants as _, KindSet, Subject, WorkspaceId, WorkspaceTree};

fn owner_id() -> String {
    "a1".repeat(32)
}

/// What reached the fake store.
#[derive(Default)]
struct Shelf {
    /// The room of each upload begun.
    begun: Vec<String>,
    /// Each stored upload's room, hash and bytes.
    stored: Vec<(String, String, Vec<u8>)>,
    /// Uploads dropped unfinished.
    aborted: usize,
}

struct FakeStore(Arc<Mutex<Shelf>>);

impl ContentStoreServices for FakeStore {
    fn max_bytes(&self) -> u64 {
        1024
    }

    fn begin(&self, room: &str) -> Result<Box<dyn ContentSink>, String> {
        self.0.lock().unwrap().begun.push(room.to_owned());
        Ok(Box::new(FakeSink {
            shelf: Arc::clone(&self.0),
            room: room.to_owned(),
            bytes: Vec::new(),
            done: false,
        }))
    }
}

struct FakeSink {
    shelf: Arc<Mutex<Shelf>>,
    room: String,
    bytes: Vec<u8>,
    done: bool,
}

impl ContentSink for FakeSink {
    fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(String, u64), String> {
        self.done = true;
        let hash = blake3::hash(&self.bytes).to_hex().to_string();
        let len = self.bytes.len() as u64;
        self.shelf.lock().unwrap().stored.push((
            self.room.clone(),
            hash.clone(),
            std::mem::take(&mut self.bytes),
        ));
        Ok((hash, len))
    }
}

impl Drop for FakeSink {
    fn drop(&mut self) {
        if !self.done {
            let mut log = self.shelf.lock().unwrap();
            log.aborted = log.aborted.saturating_add(1);
        }
    }
}

struct Fixture {
    /// The door holds the broker weakly; the fixture keeps it.
    _state: Arc<CapnpBrokerState>,
    puts: Arc<dyn ContentPuts>,
    shelf: Arc<Mutex<Shelf>>,
    room: String,
    journal: std::path::PathBuf,
    _home: tempfile::TempDir,
}

/// A room its owner governs, with agent `scribe` that may author only in
/// Notes, and agent `porter` that may upload content to the room.
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new(WorkspaceId(61), "Content puts");
    workspace.set_owner(owner_id());
    let room = workspace.room().to_string();
    workspace.admit_agent("scribe", KindSet::READ);
    workspace.allow_app(Subject::agent("scribe"), "so.tilo.notes", KindSet::ALL);
    workspace.admit_agent("porter", KindSet::READ);
    workspace.allow_app(Subject::agent("porter"), CONTENT_PUT_SCOPE, KindSet::ALL);
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    tree.persist_path = Some(home.path().join("state.json"));
    let membership = tree.shared();
    let shelf = Arc::new(Mutex::new(Shelf::default()));
    let services = HostServices {
        content_store: Some(Arc::new(FakeStore(Arc::clone(&shelf)))),
        ..membership.services()
    };
    let broker = CapnpBrokerState::test_broker_with_authority(membership.authority(), services);
    let state = Arc::clone(broker.state());
    let puts = state.content_puts().expect("a host with a content store");
    Fixture {
        _state: state,
        puts,
        shelf,
        room,
        journal: home.path().join("state.json.invocations"),
        _home: home,
    }
}

fn owner(room: &str) -> Caller {
    Caller::local_person(owner_id())
        .in_room(room)
        .in_new_execution()
}

fn agent(id: &str, room: &str) -> Caller {
    Caller::local_agent(id).in_room(room).in_new_execution()
}

fn reason(error: &ActionHostError) -> &str {
    match error {
        ActionHostError::Refused { reason }
        | ActionHostError::InvalidRequest { reason }
        | ActionHostError::Unavailable { reason } => reason,
        _ => panic!("no reason in {error:?}"),
    }
}

#[test]
fn a_chunked_put_stores_the_bytes_in_the_callers_room_and_answers_their_reference() {
    let fixture = fixture();
    let bytes: Vec<u8> = (0..700_u32).map(|n| (n % 251) as u8).collect();
    let mut upload = fixture.puts.begin(&owner(&fixture.room), 700).unwrap();
    let mut offset = 0;
    for chunk in bytes.chunks(256) {
        offset = upload.append(offset, chunk).unwrap();
    }
    assert_eq!(offset, 700);
    let reference = upload.finish().unwrap();
    let hash = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(reference, format!("tilo.content:{hash}"));
    let shelf = fixture.shelf.lock().unwrap();
    assert_eq!(shelf.begun, std::slice::from_ref(&fixture.room));
    assert_eq!(shelf.stored, [(fixture.room.clone(), hash.clone(), bytes)]);
    assert_eq!(shelf.aborted, 0);
    // Who put it is kept, without the bytes.
    let attributions = std::fs::read_to_string(
        fixture
            .journal
            .join(super::super::invocation_journal::CONTENT_PUTS_FILE),
    )
    .unwrap();
    let records: Vec<ContentPutRecord> = attributions
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1, "{attributions}");
    assert_eq!(records[0].actor, owner_id());
    assert_eq!(records[0].room, fixture.room);
    assert_eq!(records[0].hash, hash);
    assert_eq!(records[0].len, 700);
}

#[test]
fn an_agent_allowed_content_uploads_puts_content() {
    let fixture = fixture();
    let mut upload = fixture
        .puts
        .begin(&agent("porter", &fixture.room), 3)
        .unwrap();
    upload.append(0, b"abc").unwrap();
    upload.finish().unwrap();
    assert_eq!(fixture.shelf.lock().unwrap().stored.len(), 1);
}

#[test]
fn a_chunk_at_the_wrong_offset_is_refused_with_where_to_resume() {
    let fixture = fixture();
    let mut upload = fixture.puts.begin(&owner(&fixture.room), 6).unwrap();
    assert_eq!(upload.append(0, b"abc").unwrap(), 3);
    // The same chunk again: refused, not stored twice.
    let again = upload.append(0, b"abc").unwrap_err();
    assert_eq!(reason(&again), "the upload has 3 bytes: append at offset 3");
    let ahead = upload.append(5, b"f").unwrap_err();
    assert_eq!(reason(&ahead), "the upload has 3 bytes: append at offset 3");
    let past = upload.append(3, b"defg").unwrap_err();
    assert!(reason(&past).contains("past the 6 bytes"), "{past:?}");
    assert_eq!(upload.append(3, b"def").unwrap(), 6);
    upload.finish().unwrap();
    assert_eq!(fixture.shelf.lock().unwrap().stored[0].2, b"abcdef");
}

#[test]
fn a_finish_short_of_the_size_stores_nothing() {
    let fixture = fixture();
    let mut upload = fixture.puts.begin(&owner(&fixture.room), 6).unwrap();
    upload.append(0, b"abc").unwrap();
    let short = upload.finish().unwrap_err();
    assert_eq!(reason(&short), "the upload has 3 of 6 bytes");
    let shelf = fixture.shelf.lock().unwrap();
    assert!(shelf.stored.is_empty());
    assert_eq!(shelf.aborted, 1);
}

#[test]
fn a_dropped_upload_stores_nothing() {
    let fixture = fixture();
    let mut upload = fixture.puts.begin(&owner(&fixture.room), 6).unwrap();
    upload.append(0, b"abc").unwrap();
    drop(upload);
    let shelf = fixture.shelf.lock().unwrap();
    assert!(shelf.stored.is_empty());
    assert_eq!(shelf.aborted, 1);
}

#[test]
fn sizes_the_store_does_not_take_are_refused_before_anything_opens() {
    let fixture = fixture();
    for size in [0, 1025] {
        let refused = fixture
            .puts
            .begin(&owner(&fixture.room), size)
            .err()
            .unwrap();
        assert_eq!(
            reason(&refused),
            format!("an upload is 1 to 1024 bytes, not {size}")
        );
    }
    assert!(fixture.shelf.lock().unwrap().begun.is_empty());
}

/// An agent that may author only in Notes may not add content to the room:
/// authoring in an app is no authority over the room's content store. It is
/// told the one rule that would let it, and nothing is opened or stored.
#[test]
fn an_agent_authoring_only_in_notes_is_refused_a_put_with_the_rule_that_would_allow_it() {
    let fixture = fixture();
    let refused = fixture
        .puts
        .begin(&agent("scribe", &fixture.room), 3)
        .err()
        .unwrap();
    assert_eq!(
        refused,
        ActionHostError::Refused {
            reason: "not allowed to add content to this room: allow content uploads in this room to let it".into()
        }
    );
    let shelf = fixture.shelf.lock().unwrap();
    assert!(shelf.begun.is_empty() && shelf.stored.is_empty());
}

/// A caller who is not in the room at all stores nothing there.
#[test]
fn a_caller_outside_the_room_is_refused_and_nothing_is_stored() {
    let fixture = fixture();
    let refused = fixture
        .puts
        .begin(&agent("stranger", &fixture.room), 3)
        .err()
        .unwrap();
    assert_eq!(refused, ActionHostError::NotPermitted);
    let shelf = fixture.shelf.lock().unwrap();
    assert!(shelf.begun.is_empty() && shelf.stored.is_empty());
}

/// Authority is decided again at finish: an upload whose session ended
/// while the bytes arrived stores nothing.
#[test]
fn a_put_whose_session_ended_before_finish_stores_nothing() {
    let fixture = fixture();
    let caller = agent("porter", &fixture.room);
    let mut upload = fixture.puts.begin(&caller, 3).unwrap();
    upload.append(0, b"abc").unwrap();
    caller.retire_execution();
    assert!(upload.finish().is_err());
    let shelf = fixture.shelf.lock().unwrap();
    assert!(shelf.stored.is_empty());
    assert_eq!(shelf.aborted, 1);
}

/// The put is recorded before the room references its content: when the
/// record cannot be written, the put is refused and the room holds nothing.
#[test]
fn a_put_whose_attribution_cannot_be_written_is_refused_and_tags_nothing() {
    let fixture = fixture();
    // Where the attribution line goes, something that takes no line.
    std::fs::create_dir_all(
        fixture
            .journal
            .join(super::super::invocation_journal::CONTENT_PUTS_FILE),
    )
    .unwrap();
    let mut upload = fixture.puts.begin(&owner(&fixture.room), 3).unwrap();
    upload.append(0, b"abc").unwrap();
    let refused = upload.finish().expect_err("an unrecorded put is refused");
    assert!(
        reason(&refused).contains("content put attribution"),
        "{refused:?}"
    );
    let shelf = fixture.shelf.lock().unwrap();
    assert!(shelf.stored.is_empty(), "nothing is kept or tagged");
    assert_eq!(shelf.aborted, 1, "the store's upload is dropped");
}
