//! A window shown before its workspace's record: it opens over the indexes an earlier window kept,
//! reads nothing of the record for its first frame, and what the person does before the record
//! is had waits for it and lands on it, in order, once the view of the record takes the window
//! over.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ui::record::{RecordCheckout, RecordKind, RecordOwners, RecordStore};

use super::*;

/// The workspace the windows of these tests show.
const KEPT_WORKSPACE: &str = "kept-workspace";
/// The package the workspace's record is stored under in the profile.
const PACKAGE: &str = "so.tilo.lang";
const NOTES: &str = "so.tilo.lang.notes";
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

impl Workspace {
    /// The window of `app` over `doc`, keeping its workspace's indexes in `indexes`, as a live
    /// window keeps them.
    fn keeping(app: &str, doc: AutoCommit, indexes: &Path) -> Self {
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window = Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live)
            .with_sink(sink.clone())
            .keep_indexes_in(indexes.to_owned())
            .in_workspace(KEPT_WORKSPACE);
        let mut workspace = Self { doc, window, sink };
        workspace.settle();
        workspace
    }

    /// The window of `app` shown before the workspace's record, which `records` reads off the frame.
    fn opening(app: &str, records: &RecordStore, indexes: &Path) -> Self {
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window = Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live)
            .with_sink(sink.clone())
            .keep_indexes_in(indexes.to_owned())
            .in_workspace(KEPT_WORKSPACE)
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

/// A workspace kept on this device by an earlier window of `app`: `doc` as the workspace's record in a
/// profile's store, the indexes that window kept, and the store of the profile's records with
/// the record opening off the frame, as a launch opens it.
fn kept(app: &str, doc: AutoCommit, scratch: &Scratch) -> RecordStore {
    let mut earlier = Workspace::keeping(app, doc, &scratch.indexes());
    let host = earlier.window.host().expect("the host started");
    host.wait_index();
    assert!(host.index_ready());
    earlier.settle();
    let mut doc = earlier.doc.clone();
    // What is published is read off the frame from the indexes: done reading, the window
    // closes, and its indexes reach the disk.
    let _ = Workspace::resources();
    let meta = crate::app::builtin::package_named(app).expect("a built-in package");
    let schema = tilolang_tilo::MetaExt::schema_text(meta);
    let app_id = meta.app.clone();
    drop(host);
    drop(earlier);
    let file = tilolang_tilo::index::file_of(
        &tilolang_tilo::index::workspace_dir(&scratch.indexes(), KEPT_WORKSPACE),
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
    let landing = ui::record::transfer::install_workspace_document(
        profile.store(),
        PACKAGE,
        KEPT_WORKSPACE,
        &mut doc,
    )
    .unwrap();
    profile.store().flush(None).unwrap();
    assert!(matches!(
        landing.durability(&**profile.store()),
        ui::record::transfer::Durability::Durable
    ));
    drop(profile);
    let records = RecordOwners::new(state)
        .open(PACKAGE, KEPT_WORKSPACE)
        .unwrap();
    assert!(records.begin_opening(&primary()).unwrap());
    records
}

/// The window of `app` shown before the record `records` reads, after its first frame.
fn opened(app: &str, records: &RecordStore, scratch: &Scratch) -> Workspace {
    let mut workspace = Workspace::opening(app, records, &scratch.indexes());
    workspace.frame();
    let host = workspace
        .window
        .host()
        .expect("the window opens over the kept indexes");
    let kept: Vec<String> =
        std::fs::read_dir(scratch.indexes().join("workspaces").join(KEPT_WORKSPACE))
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
    workspace
}

/// The workspace record of a notebook of `n` notes, as the bench seeds it.
fn notebook(n: usize) -> AutoCommit {
    crate::app::builtin::choose("notes");
    let notes = crate::app::builtin::compiled()
        .iter()
        .find(|built| built.compiled.meta.app == NOTES)
        .expect("the notes package");
    tilolang_tilo::bench::seeded_record(std::slice::from_ref(&notes.compiled), n).unwrap()
}

/// What the first frame of a window over a kept workspace of `n` notes read: of the index and of
/// the record.
fn first_frame_over(n: usize) -> (tilolang_tilo::index::IndexReads, u64, u64, bool) {
    let scratch = Scratch::new("first-frame");
    let records = kept("notes", notebook(n), &scratch);
    let mut workspace = opened("notes", &records, &scratch);
    let host = workspace.window.host().expect("the host started");
    let reads = host.store().read_stats();
    assert_eq!(
        workspace.tree_labels().len(),
        n,
        "the tree is the notebook's"
    );
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
fn the_first_frame_of_a_kept_workspace_reads_its_index_and_not_its_record_whatever_its_size() {
    for size in [100, 500] {
        let (index, entries, located, had_record) = first_frame_over(size);
        assert!(!had_record, "the window opens over the index: {size} notes");
        assert_eq!(
            (entries, located),
            (0, 0),
            "nothing of the record was read: {size} notes"
        );
        assert!(
            index.dirs > 0,
            "the index answered the frame: {size} notes, {index:?}"
        );
    }
}

/// The notes of the workspace's notebook, as the window's store reads them now.
fn notes_of(host: &Host) -> Vec<ResourceRef> {
    let notebook = host.root_of(NOTES).expect("the notebook");
    let RtValue::List(notes) = host.runtime().read(&notebook, "notes").expect("its notes") else {
        panic!("the notebook lists its notes");
    };
    notes
        .into_iter()
        .filter_map(|note| match note {
            RtValue::Ref(note) => Some((*note.target).clone()),
            _ => None,
        })
        .collect()
}

/// What the note `note` is titled, as a window reads it.
fn title_of(host: &Host, note: &ResourceRef) -> String {
    match host.runtime().read(note, "title").expect("its title") {
        RtValue::Text(title) => title,
        other => panic!("a title is text: {other:?}"),
    }
}

/// The note `note` stands under, if it stands under one.
fn parent_of(host: &Host, note: &ResourceRef) -> Option<ResourceRef> {
    match host.runtime().read(note, "parent").expect("its parent") {
        RtValue::Opt(Some(held)) => match *held {
            RtValue::Ref(parent) => Some((*parent.target).clone()),
            other => panic!("a parent is a note: {other:?}"),
        },
        RtValue::Opt(None) => None,
        other => panic!("a parent is a note: {other:?}"),
    }
}

/// The id the notes package's action `name` has.
fn note_action(host: &Host, name: &str) -> String {
    host.package(NOTES)
        .expect("the notes package")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the notes package declares `{name}`"))
        .id
        .clone()
}

#[test]
fn a_window_on_a_note_that_left_with_the_note_it_stood_under_says_it_is_gone() {
    let scratch = Scratch::new("note-gone");
    // A notebook whose top note is deleted: the notes under it leave with it. The record still
    // holds what they held — a deleted note's map stays in the record — so a copy of it answers
    // their fields, and only the index they left says they are gone.
    let (doc, gone, gone_title) = {
        let mut workspace = Workspace::seeded("notes");
        let host = workspace.window.current_host().expect("the host started");
        let (parent, under) = notes_of(&host)
            .into_iter()
            .find_map(|note| parent_of(&host, &note).map(|parent| (parent, note)))
            .expect("a note of the scenario stands under another");
        let title = title_of(&host, &under);
        let notebook = host.root_of(NOTES).expect("the notebook");
        let delete = note_action(&host, "delete");
        host.runtime()
            .invoke_action(
                &Principal::new("ada"),
                (
                    delete,
                    vec![RtValue::resource(&notebook), RtValue::resource(&parent)],
                ),
            )
            .expect("the person deletes the note");
        workspace.window.commit_planned();
        workspace.settle();
        assert!(
            !notes_of(&host).contains(&under),
            "the note went with the one it stood under"
        );
        (workspace.doc.clone(), under, title)
    };
    // The indexes a window kept over that record: they name the note nowhere. The window opens
    // over them, before its record.
    let records = kept("notes", doc, &scratch);
    let mut opened = opened("notes", &records, &scratch);
    let host = opened.window.host().expect("the host started");
    assert!(
        !host.store().has_record(),
        "the window opens over the index"
    );

    // The note is gone: nothing of it reads, and the record is not read for it.
    let read = host.runtime().read(&gone, "title");
    assert!(
        read.is_err(),
        "the note went with the one it stood under: {read:?}"
    );
    assert!(
        !host.store().has_record(),
        "the record is not read for a note the workspace no longer holds"
    );
    assert!(
        tilolang_tilo::runtime::store::Store::type_of(host.store(), &gone).is_err(),
        "and its window says it is gone"
    );
    // Its page does not show its title either.
    opened
        .window
        .open_requests()
        .borrow_mut()
        .push(tilolang_tilo::OpenEffect::Open {
            anchor: gone.id_string(),
        });
    opened.frame();
    opened.frame();
    let page = format!("{:?}", opened.window.detail_tree());
    assert!(
        !page.contains(&gone_title),
        "the page of a note that is gone shows no title: {page}"
    );
}

#[test]
fn a_note_a_write_just_made_opens_and_shows_its_fields_before_the_index_names_it() {
    let scratch = Scratch::new("note-made");
    let records = kept("notes", notebook(3), &scratch);
    let mut workspace = opened("notes", &records, &scratch);
    let before = notes_of(&workspace.window.host().expect("the host started"));
    assert_eq!(before.len(), 3, "the notebook the index kept");

    // A note made in the window shown before the record: the write is the store's own, and the
    // index — which derives the record — names the note nowhere while the record does not hold
    // it. It opens and reads anyway, from the write.
    let new_note = workspace.key_of("ui.Button", "label", "New note");
    workspace
        .window
        .act(UiEvent::Fire(new_note, "press".to_owned(), None));
    workspace.frame();
    workspace.frame();
    let host = workspace.window.host().expect("the host started");
    assert!(host.store().has_record(), "the write waited for the record");
    let made = notes_of(&host)
        .into_iter()
        .find(|note| !before.contains(note))
        .expect("the press made a note");
    let open: usize = host
        .store()
        .held_chunks()
        .iter()
        .map(|(_, _, open)| *open)
        .sum();
    assert!(
        open > 0,
        "the made note is the store's own write, which the record does not hold yet"
    );
    // It opens, and shows the fields the write did not write.
    assert!(
        tilolang_tilo::runtime::store::Store::type_of(host.store(), &made).is_ok(),
        "a note a write just made opens before the index names it"
    );
    assert_eq!(
        title_of(&host, &made),
        "",
        "and shows the title the write did not write"
    );
    assert_eq!(
        workspace.window.detail_target(),
        Some(&made),
        "the window shows the note it just made"
    );
    assert!(
        first_input(&workspace.composed()).is_some(),
        "with its title, as the page it opens in"
    );
}

#[test]
fn what_is_done_before_the_record_arrives_waits_for_it_and_lands_on_it_in_order() {
    let scratch = Scratch::new("before-the-record");
    let records = kept("notes", notebook(3), &scratch);
    let mut workspace = opened("notes", &records, &scratch);
    let host = workspace.window.host().expect("the host started");
    assert!(!host.store().has_record());
    assert_eq!(workspace.tree_labels().len(), 3);

    // A note made and named before the record is had: the write waits for it.
    let new_note = workspace.key_of("ui.Button", "label", "New note");
    workspace
        .window
        .act(UiEvent::Fire(new_note, "press".to_owned(), None));
    workspace.frame();
    workspace.frame();
    assert!(host.store().has_record(), "the write waited for the record");
    assert!(!records.is_opening(&primary().id), "and took it in");
    let title = first_input(&workspace.composed())
        .expect("the new note's title")
        .key
        .clone();
    workspace
        .window
        .act(UiEvent::Edit(title.clone(), "Pl".to_owned()));
    workspace.frame();
    workspace
        .window
        .act(UiEvent::Edit(title, "Plans".to_owned()));
    workspace.frame();

    // The view of the record takes the window over, and hands the door what was done.
    workspace.doc = records.opening(&primary()).document().expect("the record");
    let shown = std::mem::replace(
        &mut workspace.window,
        Window::new(NOTES.to_owned(), "ada".to_owned(), RunMode::Live),
    );
    workspace.window = shown.taken_over();
    workspace.settle();
    assert_eq!(workspace.items_of(NOTES, "notes"), 4, "the note was made");
    let titles: Vec<String> = workspace.tree_labels();
    assert!(
        titles.iter().any(|title| title == "Plans"),
        "named after it was made, as it was typed: {titles:?}"
    );
    // What the window shows is what the record holds now, not its own writes over it.
    let committed = Host::new(
        &workspace.doc,
        Principal::new("reader"),
        tilolang_tilo::authority::holding_nothing("reader"),
    )
    .unwrap();
    crate::app::load(&committed);
    let notebook = committed.root_of(NOTES).unwrap();
    let RtValue::List(notes) = committed.runtime().read(&notebook, "notes").unwrap() else {
        panic!("the notebook lists its notes");
    };
    assert_eq!(notes.len(), 4, "the record holds the new note");
}
