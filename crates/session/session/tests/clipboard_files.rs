//! The file clipboard: copied files pasted into a folder, through a journal
//! that outlives the session that started it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use settings_store::clipboard_history::PASTE;
use tilo_compositor::{OutputConfig, Selection};
use tilo_session::{CLIPBOARD_RESOURCE, Options, Session};
use ui::dnd::{COPIED_FILES_MIME, copied_files_list};

/// The clipboard's history and paste journal are this device's settings, so
/// the tests of it take turns rather than overwrite each other's journal.
static CLIPBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The lock, with the journal left empty: a test starts with no paste.
fn take_turns() -> std::sync::MutexGuard<'static, ()> {
    let guard = CLIPBOARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    PASTE.set(None).expect("an empty journal");
    guard
}

fn session() -> Session {
    Session::headless(
        Options::default(),
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session")
}

/// A folder of this test's own, gone when the test ends.
fn scratch(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("clipboard-files-{name}-"))
        .tempdir()
        .expect("a scratch folder")
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("step");
}

/// An app copies (`cut == false`) or cuts a list of files.
fn copy(session: &mut Session, paths: &[PathBuf], cut: bool) {
    let body = copied_files_list(paths, cut);
    session
        .compositor()
        .set_selection(
            Selection::Clipboard,
            vec![(COPIED_FILES_MIME.to_owned(), body.into_bytes())],
        )
        .expect("the app copies");
    // The copy is read off the selection over the turns that follow: the app
    // answers when it gets round to it.
    for _ in 0..3 {
        step(session);
    }
}

/// Paste the copied files into `destination`, through the clipboard's
/// published action.
fn paste_into(session: &mut Session, destination: &Path) {
    session
        .invoke(
            CLIPBOARD_RESOURCE,
            "paste-files",
            &serde_json::from_value(serde_json::json!({
                "destination": destination.to_string_lossy(),
            }))
            .expect("an object of arguments"),
        )
        .expect("paste the copied files");
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn copied_files_are_pasted_beside_the_ones_already_in_the_folder() {
    let _turns = take_turns();
    let dir = scratch("plain");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&source).expect("a source folder");
    fs::create_dir_all(&destination).expect("a destination folder");
    fs::write(source.join("notes.txt"), b"the new one").expect("a file");
    fs::write(source.join("plan.txt"), b"the other one").expect("a file");
    fs::write(destination.join("notes.txt"), b"already here").expect("a file in the way");

    let mut session = session();
    copy(
        &mut session,
        &[source.join("notes.txt"), source.join("plan.txt")],
        false,
    );
    paste_into(&mut session, &destination);
    assert!(
        PASTE.get().is_some(),
        "the copy of files was on the clipboard to paste"
    );

    // The paste is carried out a file at a time by the loop.
    for _ in 0..20 {
        if PASTE.get().is_none() {
            break;
        }
        step(&mut session);
    }
    assert_eq!(PASTE.get(), None, "the paste finished");
    assert_eq!(
        read(&destination.join("notes (2).txt")),
        b"the new one",
        "the file already there is left alone and the copy is numbered"
    );
    assert_eq!(read(&destination.join("notes.txt")), b"already here");
    assert_eq!(read(&destination.join("plan.txt")), b"the other one");
    assert!(
        source.join("notes.txt").exists() && source.join("plan.txt").exists(),
        "a copy leaves its sources where they are"
    );
}

#[test]
fn a_cut_takes_its_sources_and_a_paste_a_session_died_in_finishes_in_the_next() {
    let _turns = take_turns();
    let dir = scratch("resume");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&source).expect("a source folder");
    fs::create_dir_all(&destination).expect("a destination folder");
    let paths: Vec<PathBuf> = (0..6)
        .map(|index| {
            let path = source.join(format!("{index}.txt"));
            fs::write(&path, format!("file {index}")).expect("a file");
            path
        })
        .collect();

    {
        let mut session = session();
        copy(&mut session, &paths, true);
        paste_into(&mut session, &destination);
        // One turn of the loop, then the session dies: only some of the
        // files are across.
        step(&mut session);
        let journal = PASTE.get().expect("the paste is written down");
        assert!(
            journal.unfinished(),
            "the paste was in flight when the session ended: {journal:?}"
        );
        assert_eq!(journal.destination, destination.to_string_lossy());
        assert!(journal.moving);
        assert!(
            source.read_dir().expect("the source folder").count() > 0,
            "not every source was taken yet"
        );
    }

    // The person logs in again: the paste goes on where it stopped, without
    // anyone pasting again.
    let mut session = session();
    for _ in 0..200 {
        if PASTE.get().is_none() {
            break;
        }
        step(&mut session);
    }
    assert_eq!(PASTE.get(), None, "the next session finished the paste");
    for (index, path) in paths.iter().enumerate() {
        let target = destination.join(path.file_name().expect("a name"));
        assert_eq!(
            read(&target),
            format!("file {index}").into_bytes(),
            "{} arrived",
            target.display()
        );
        assert!(
            !path.exists(),
            "a cut takes {} once it is across",
            path.display()
        );
    }
}
