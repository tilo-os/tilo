//! A window shown before its room's record: it opens over the indexes an earlier window kept,
//! reads nothing of the record for its first frame, and what the person does before the record
//! is had waits for it and lands on it, in order, once the view of the record takes the window
//! over.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ui::record::{RecordCheckout, RecordKind, RecordOwners, RecordStore};

use super::*;

/// The room the windows of these tests show.
const KEPT_ROOM: &str = "kept-room";
/// The package the room's record is stored under in the profile.
const PACKAGE: &str = "so.tilo.lang";
const NOTES: &str = "so.tilo.lang.notes";
/// The rows a window shows of a list: its first screenful.
const SHOWN: usize = 20;

/// A directory of its own, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tilolang-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn indexes(&self) -> PathBuf {
        self.0.join("indexes")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Room {
    /// The window of `app` over `doc`, keeping its room's indexes in `indexes`, as a live
    /// window keeps them.
    fn keeping(app: &str, doc: AutoCommit, indexes: &Path) -> Self {
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window = Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live)
            .with_sink(sink.clone())
            .keep_indexes_in(indexes.to_owned())
            .in_room(KEPT_ROOM);
        let mut room = Self { doc, window, sink };
        room.settle();
        room
    }

    /// The window of `app` shown before the room's record, which `records` reads off the frame.
    fn opening(app: &str, records: &RecordStore, indexes: &Path) -> Self {
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window = Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live)
            .with_sink(sink.clone())
            .keep_indexes_in(indexes.to_owned())
            .in_room(KEPT_ROOM)
            .opening(records.opening(&primary()));
        Self {
            doc: AutoCommit::new(),
            window,
            sink,
        }
    }
}

fn primary() -> RecordCheckout {
    RecordCheckout::package_primary(RecordKind::new(crate::RECORD_KIND))
}

/// A room kept on this device by an earlier window of `app`: `doc` as the room's record in a
/// profile's store, the indexes that window kept, and the store of the profile's records with
/// the record opening off the frame, as a launch opens it.
fn kept(app: &str, doc: AutoCommit, scratch: &Scratch) -> RecordStore {
    let mut earlier = Room::keeping(app, doc, &scratch.indexes());
    let host = earlier.window.host().expect("the host started");
    host.wait_index();
    assert!(host.index_ready());
    earlier.settle();
    let mut doc = earlier.doc.clone();
    // What is published is read off the frame from the indexes: done reading, the window
    // closes, and its indexes reach the disk.
    let _ = Room::resources();
    let meta = crate::app::builtin::package_named(app).expect("a built-in package");
    let schema = tilolang_tilo::MetaExt::schema_text(meta);
    let app_id = meta.app.clone();
    drop(host);
    drop(earlier);
    let file = tilolang_tilo::index::file_of(
        &tilolang_tilo::index::room_dir(&scratch.indexes(), KEPT_ROOM),
        &app_id,
    );
    match tilolang_tilo::index::Index::open(&file, &app_id, &schema) {
        Ok(tilolang_tilo::index::Opened::Ready(..)) => {}
        Ok(tilolang_tilo::index::Opened::Empty(_)) => {
            panic!("the closed window's index of {app_id} is not on the disk")
        }
        Err(error) => panic!("the closed window still holds its index of {app_id}: {error}"),
    }
    let state = scratch.0.join("state");
    let profile = ui::application::NativeProfile::open(&state, None).unwrap();
    let landing =
        ui::record::transfer::install_room_document(profile.store(), PACKAGE, KEPT_ROOM, &mut doc)
            .unwrap();
    profile.store().flush(None).unwrap();
    assert!(matches!(
        landing.durability(&**profile.store()),
        ui::record::transfer::Durability::Durable
    ));
    drop(profile);
    let records = RecordOwners::new(state).open(PACKAGE, KEPT_ROOM).unwrap();
    assert!(records.begin_opening(&primary()).unwrap());
    records
}

/// The window of `app` shown before the record `records` reads, after its first frame.
fn opened(app: &str, records: &RecordStore, scratch: &Scratch) -> Room {
    let mut room = Room::opening(app, records, &scratch.indexes());
    room.frame();
    let host = room
        .window
        .host()
        .expect("the window opens over the kept indexes");
    let kept: Vec<String> = std::fs::read_dir(scratch.indexes().join("rooms").join(KEPT_ROOM))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        host.index_ready(),
        "the indexes the earlier window kept are read from: {kept:?}"
    );
    room
}

/// The room record of a notebook of `n` notes, as the bench seeds it.
fn notebook(n: usize) -> AutoCommit {
    crate::app::builtin::choose("notes");
    let notes = crate::app::builtin::compiled()
        .iter()
        .find(|built| built.compiled.meta.app == NOTES)
        .expect("the notes package");
    tilolang_tilo::bench::seeded_record(std::slice::from_ref(&notes.compiled), n).unwrap()
}

/// What the first frame of a window over a kept room of `n` notes read: of the index and of
/// the record.
fn first_frame_over(n: usize) -> (tilolang_tilo::index::IndexReads, u64, u64, bool) {
    let scratch = Scratch::new("first-frame");
    let records = kept("notes", notebook(n), &scratch);
    let room = opened("notes", &records, &scratch);
    // The rows a window shows: its first screenful.
    let rows = room.root_rows();
    rows.borrow_mut().request_range(0..SHOWN);
    for index in 0..SHOWN {
        let key = rows.borrow().key_at(index).expect("a row");
        rows.borrow_mut().read(&key, &mut |_| Ok(())).unwrap();
    }
    let host = room.window.host().expect("the host started");
    let reads = host.store().read_stats();
    assert_eq!(rows.borrow().len(), n, "the list is the notebook's");
    assert!(
        records.is_opening(&primary().id),
        "the record is still read off the frame"
    );
    (
        reads.index,
        reads.doc.entries,
        reads.doc.located,
        host.store().has_record(),
    )
}

#[test]
fn the_first_frame_of_a_kept_room_reads_its_index_and_not_its_record_whatever_its_size() {
    let small = first_frame_over(1000);
    let large = first_frame_over(10_000);
    assert!(!small.3 && !large.3, "neither window had the record");
    assert_eq!((small.1, small.2), (0, 0), "nothing of the record was read");
    assert_eq!(
        small, large,
        "the first frame reads the same of 10k notes as of 1k"
    );
}

#[test]
fn what_is_done_before_the_record_arrives_waits_for_it_and_lands_on_it_in_order() {
    let scratch = Scratch::new("before-the-record");
    let records = kept("notes", notebook(3), &scratch);
    let mut room = opened("notes", &records, &scratch);
    let host = room.window.host().expect("the host started");
    assert!(!host.store().has_record());
    assert_eq!(room.rows().len(), 3);

    // A note made and named before the record is had: the write waits for it.
    let new_note = room.key_of("ui.Button", "label", "New note");
    room.window
        .act(UiEvent::Fire(new_note, "press".to_owned(), None));
    room.frame();
    room.frame();
    assert!(host.store().has_record(), "the write waited for the record");
    assert!(!records.is_opening(&primary().id), "and took it in");
    let title = first_input(&room.composed())
        .expect("the new note's title")
        .key
        .clone();
    room.window
        .act(UiEvent::Edit(title.clone(), "Pl".to_owned()));
    room.frame();
    room.window.act(UiEvent::Edit(title, "Plans".to_owned()));
    room.frame();

    // The view of the record takes the window over, and hands the door what was done.
    room.doc = records.opening(&primary()).document().expect("the record");
    let shown = std::mem::replace(
        &mut room.window,
        Window::new(NOTES.to_owned(), "ada".to_owned(), RunMode::Live),
    );
    room.window = shown.taken_over();
    room.settle();
    assert_eq!(room.items_of(NOTES, "notes"), 4, "the note was made");
    let titles: Vec<String> = room.rows().into_iter().map(|(_, title)| title).collect();
    assert!(
        titles.iter().any(|title| title == "Plans"),
        "named after it was made, as it was typed: {titles:?}"
    );
    // What the window shows is what the record holds now, not its own writes over it.
    let committed = Host::new(
        &room.doc,
        Principal::new("reader"),
        |_: &Principal, _: &ActionInfo| Decision::Deny,
    )
    .unwrap();
    crate::app::load(&committed);
    let notebook = committed.root_of(NOTES).unwrap();
    let Value::List(notes) = committed.runtime().read(&notebook, "notes").unwrap() else {
        panic!("the notebook lists its notes");
    };
    assert_eq!(notes.len(), 4, "the record holds the new note");
}
