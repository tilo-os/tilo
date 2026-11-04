//! The file clipboard: copied files pasted into a folder on a thread of its
//! own, through a receipt that outlives the session that started it.

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

mod common;

use std::fs;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tilo_compositor::{OutputConfig, Selection};
use tilo_session::{CLIPBOARD_RESOURCE, Options, Session};
use tilo_test_env::{SpawnUnderEnvLock as _, TestHome};
use ui::dnd::{COPIED_FILES_MIME, copied_files_list};

/// The name a paste stages its copies under, inside the receiving folder.
const STAGING_PREFIX: &str = ".tilo-transfer-";
/// How many files the folder of the crash test holds.
const BIG: usize = 500;
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

/// Step the session until `done`, so a paste on its own thread has time to
/// finish. The steps move the session's clock on, not the machine's: the paste
/// copies on its own thread, so the suite's hang bound is what ends the wait,
/// never a count of turns a loaded machine would run through too fast.
fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    common::until(session, what, done);
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
fn paste_into(session: &mut Session, destination: &Path, conflict: Option<&str>) {
    let mut arguments = serde_json::json!({
        "destination": destination.to_string_lossy(),
    });
    if let Some(conflict) = conflict {
        arguments["conflict"] = serde_json::Value::String(conflict.to_owned());
    }
    session
        .invoke(
            CLIPBOARD_RESOURCE,
            "paste-files",
            &serde_json::from_value(arguments).expect("an object of arguments"),
        )
        .expect("paste the copied files");
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The receipt a paste is written down in: this session's own state.
fn receipt(home: &TestHome) -> PathBuf {
    home.state().join("tilo/file-paste/paste.json")
}

/// The staging folders a paste leaves inside `destination` while it runs.
fn staging(destination: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(destination) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(STAGING_PREFIX))
        })
        .collect()
}

/// Write `count` files named `0`, `1`, ... , each holding `file <index>`, in
/// `folder`.
fn tree(folder: &Path, count: usize) -> Vec<PathBuf> {
    fs::create_dir_all(folder).expect("a folder");
    (0..count)
        .map(|index| {
            let path = folder.join(index.to_string());
            fs::write(&path, format!("file {index}")).expect("a file");
            path
        })
        .collect()
}

/// Whether `folder` holds exactly the `count` files [`tree`] writes, each with
/// its own contents.
fn tree_is_whole(folder: &Path, count: usize) -> bool {
    let Ok(entries) = fs::read_dir(folder) else {
        return false;
    };
    let mut seen = 0usize;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Ok(index) = name.to_string_lossy().parse::<usize>() else {
            return false;
        };
        if index >= count {
            return false;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            return false;
        };
        if bytes != format!("file {index}").into_bytes() {
            return false;
        }
        seen = seen.saturating_add(1);
    }
    seen == count
}

#[test]
fn copied_files_are_pasted_beside_the_ones_already_in_the_folder() {
    let home = TestHome::new().expect("a home of this test's own");
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
    paste_into(&mut session, &destination, None);
    let pasted = destination.join("notes (2).txt");
    until(&mut session, "the paste finished", |_| pasted.exists());
    until(&mut session, "the receipt went", |_| {
        !receipt(&home).exists()
    });

    assert_eq!(
        read(&pasted),
        b"the new one",
        "the file already there is left alone and the copy is numbered"
    );
    assert_eq!(read(&destination.join("notes.txt")), b"already here");
    assert_eq!(read(&destination.join("plan.txt")), b"the other one");
    assert!(
        source.join("notes.txt").exists() && source.join("plan.txt").exists(),
        "a copy leaves its sources where they are"
    );
    assert!(staging(&destination).is_empty(), "nothing is left staged");
}

#[test]
fn a_cut_takes_its_sources_once_the_copies_stand_under_their_names() {
    let home = TestHome::new().expect("a home of this test's own");
    let dir = scratch("cut");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&destination).expect("a destination folder");
    let paths = tree(&source, 6);

    let mut session = session();
    copy(&mut session, &paths, true);
    paste_into(&mut session, &destination, None);
    until(&mut session, "the cut finished", |_| {
        paths.iter().all(|path| !path.exists())
    });
    until(&mut session, "the receipt went", |_| {
        !receipt(&home).exists()
    });

    for (index, path) in paths.iter().enumerate() {
        let target = destination.join(path.file_name().expect("a name"));
        assert_eq!(
            read(&target),
            format!("file {index}").into_bytes(),
            "{} arrived",
            target.display()
        );
    }
    assert!(staging(&destination).is_empty(), "nothing is left staged");
}

#[test]
fn replacing_sends_the_name_that_was_there_to_the_trash() {
    let home = TestHome::new().expect("a home of this test's own");
    let dir = scratch("replace");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&source).expect("a source folder");
    fs::create_dir_all(&destination).expect("a destination folder");
    fs::write(source.join("note.txt"), b"the new one").expect("a file");
    fs::write(destination.join("note.txt"), b"the old one, and only here").expect("a file");

    let mut session = session();
    copy(&mut session, &[source.join("note.txt")], false);
    paste_into(&mut session, &destination, Some("replace"));
    until(&mut session, "the paste finished", |_| {
        read(&destination.join("note.txt")) == b"the new one"
    });
    until(&mut session, "the receipt went", |_| {
        !receipt(&home).exists()
    });

    let files = home.data().join("Trash/files");
    let kept = fs::read_dir(&files).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            let path = entry.path();
            path.is_file()
                && fs::read(&path).is_ok_and(|bytes| bytes == b"the old one, and only here")
        })
    });
    assert!(kept, "what was there is in the Trash, not gone");
}

#[test]
fn a_file_that_cannot_be_read_is_reported_and_the_rest_of_the_paste_lands() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = TestHome::new().expect("a home of this test's own");
    let dir = scratch("unreadable");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&source).expect("a source folder");
    fs::create_dir_all(&destination).expect("a destination folder");
    let secret = source.join("secret.txt");
    fs::write(&secret, b"closed").expect("a file");
    fs::write(source.join("fine.txt"), b"open").expect("a file");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("a closed file");
    if fs::read(&secret).is_ok() {
        // Root reads everything; there is no refusal to prove here.
        return;
    }

    let mut session = session();
    copy(&mut session, &[secret, source.join("fine.txt")], false);
    paste_into(&mut session, &destination, None);
    until(&mut session, "the readable one landed", |_| {
        destination.join("fine.txt").exists()
    });
    until(&mut session, "the receipt went", |_| {
        !receipt(&home).exists()
    });
    // The receipt goes before the worker says it is done, and the loop tells
    // the person on the pass after that: the notice is waited on, not read
    // from the receipt's going.
    until(
        &mut session,
        "the notice saying what did not land",
        |session| !session.notifications().log().active().is_empty(),
    );
    assert_eq!(read(&destination.join("fine.txt")), b"open");
    assert!(
        !destination.join("secret.txt").exists(),
        "the file that could not be read is not half there"
    );

    let said: Vec<(String, String)> = session
        .notifications()
        .log()
        .active()
        .to_vec()
        .into_iter()
        .map(|notice| (notice.title, notice.body))
        .collect();
    assert!(
        said.iter().any(|(_, body)| body.contains("secret.txt")),
        "the person is told which file did not land: {said:?}"
    );
}

/// A folder of many files pasted while the session keeps turning: no turn of
/// the loop carries a copy, so every frame keeps its time.
#[test]
fn a_big_folder_is_pasted_without_a_single_slow_turn_of_the_loop() {
    let home = TestHome::new().expect("a home of this test's own");
    let dir = scratch("budget");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&destination).expect("a destination folder");
    let paths = tree(&source.join("big"), 12000);

    let mut session = session();
    // What a turn costs on this machine before the paste starts: a loaded one
    // takes every turn slowly, and the test is about the paste being spread
    // over turns, not about the machine being idle.
    let mut before: Vec<Duration> = (0..5)
        .map(|_| {
            let turn = Instant::now();
            step(&mut session);
            turn.elapsed()
        })
        .collect();
    before.sort_unstable();
    let pace = before.get(before.len() / 2).copied().unwrap_or_default();
    copy(&mut session, &[source.join("big")], false);
    let started = Instant::now();
    paste_into(&mut session, &destination, None);
    let mut turns: Vec<Duration> = Vec::new();
    while !destination.join("big").exists() && started.elapsed() < tilo_test_env::hang_bound() {
        let turn = Instant::now();
        step(&mut session);
        turns.push(turn.elapsed());
    }
    let copied = started.elapsed();
    until(&mut session, "the receipt went", |_| {
        !receipt(&home).exists()
    });
    assert!(
        tree_is_whole(&destination.join("big"), paths.len()),
        "the whole folder landed"
    );

    let mut sorted = turns.clone();
    sorted.sort_unstable();
    let p50 = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
    let p95 = sorted
        .get(sorted.len().saturating_mul(95) / 100)
        .copied()
        .unwrap_or_default();
    let max = sorted.last().copied().unwrap_or_default();
    println!(
        "paste of {} files took {copied:?} over {} turns; turn p50 {p50:?}, p95 {p95:?}, max {max:?}",
        paths.len(),
        turns.len()
    );
    assert!(
        turns.len() > 20,
        "the loop kept turning while the paste ran: {} turns over {copied:?}",
        turns.len()
    );
    // A turn that waits on the disk stands out from the turns around it by
    // whole seconds, while a loaded machine only ever takes a turn off the CPU
    // for a few of its own turns' worth: so the bound is the loop's own pace,
    // never a wall-clock figure of this machine.
    assert!(
        p50 < pace.max(Duration::from_millis(50)),
        "the loop's usual turn kept its time: p50 {p50:?} of {} turns, over {copied:?}, \
         against {pace:?} a turn before the paste",
        turns.len()
    );
    let quiet = pace
        .max(Duration::from_millis(50))
        .max(p95.saturating_mul(4));
    assert!(
        max < quiet,
        "no turn waited on the disk: max {max:?} of {} turns, p50 {p50:?}, p95 {p95:?}, over {copied:?}",
        turns.len()
    );
}

/// The paste a child process starts, and the moment it loses power: the
/// transfer ends the process at the moment `CLIPBOARD_KILL_AT` names, so
/// nothing of it survives to tidy up.
fn kill_mid_transfer_child() {
    let Some(stage) = std::env::var_os("CLIPBOARD_KILL_AT") else {
        return;
    };
    let source = PathBuf::from(std::env::var_os("CLIPBOARD_KILL_SOURCE").expect("a source"));
    let destination =
        PathBuf::from(std::env::var_os("CLIPBOARD_KILL_DESTINATION").expect("a destination"));
    let mut paths = tree(&source, 6);
    paths.push(source.join("big"));

    let mut session = session();
    copy(&mut session, &paths, true);
    paste_into(&mut session, &destination, None);
    // The paste dies at the moment `stage` names (it ends this process
    // there, as a power cut would), so this loop only lets it run: no reading
    // of the disk races the transfer, and the machine's pace, not a count of
    // turns, decides when it dies.
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the transfer never reached {}",
            stage.display()
        );
        step(&mut session);
    }
}

#[test]
#[ignore = "started by a_killed_paste_leaves_every_file_whole_or_untouched"]
fn kill_mid_transfer() {
    kill_mid_transfer_child();
}

/// Start the child above, which kills itself mid-transfer, and wait for it.
fn kill_under(stage: &str, source: &Path, destination: &Path) -> std::process::ExitStatus {
    Command::new(std::env::current_exe().expect("this test's binary"))
        .args(["--exact", "kill_mid_transfer", "--ignored", "--nocapture"])
        .env("CLIPBOARD_KILL_AT", stage)
        .env("TILO_FILE_PASTE_CRASH_POINT", stage)
        .env("CLIPBOARD_KILL_SOURCE", source)
        .env("CLIPBOARD_KILL_DESTINATION", destination)
        .spawn_locked()
        .expect("the child session")
        .wait()
        .expect("the child ended")
}

/// Every item of a cut ends as either its source, untouched, or its copy,
/// complete: never half of either, and never both gone.
fn assert_whole_or_untouched(source: &Path, target: &Path, whole: bool) {
    let source_there = source.exists();
    let target_there = target.exists();
    assert!(
        source_there || target_there,
        "{} and {} are both gone",
        source.display(),
        target.display()
    );
    if target_there {
        assert!(
            whole,
            "{} stands under its own name, but it is not whole",
            target.display()
        );
    }
}

fn assert_every_item_whole_or_untouched(source: &Path, destination: &Path) {
    for index in 0..6 {
        let from = source.join(index.to_string());
        let to = destination.join(index.to_string());
        let whole = fs::read(&to).is_ok_and(|bytes| bytes == format!("file {index}").into_bytes());
        assert_whole_or_untouched(&from, &to, whole);
    }
    let big_from = source.join("big");
    let big_to = destination.join("big");
    let whole = big_to.exists() && tree_is_whole(&big_to, BIG);
    assert_whole_or_untouched(&big_from, &big_to, whole);
    assert!(
        staging(destination).is_empty(),
        "no staging is left in {}",
        destination.display()
    );
}

#[test]
fn a_killed_paste_leaves_every_file_whole_or_untouched() {
    let home = TestHome::new().expect("a home of this test's own");
    let dir = scratch("killed");
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&source).expect("a source folder");
    fs::create_dir_all(&destination).expect("a destination folder");
    tree(&source.join("big"), BIG);

    // Killed while the copies are being staged: nothing is published, so
    // nothing of the paste survives it but the sources.
    let status = kill_under("staging", &source, &destination);
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the process was killed mid-transfer: {status:?}"
    );
    assert!(
        receipt(&home).exists(),
        "the paste was written down when the session died"
    );
    {
        let mut session = session();
        until(&mut session, "the paste was undone", |_| {
            !receipt(&home).exists()
        });
    }
    assert_every_item_whole_or_untouched(&source, &destination);

    // Killed while the copies are being published: what stands is whole and
    // what does not is back where it came from.
    let status = kill_under("publishing", &source, &destination);
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the process was killed mid-transfer: {status:?}"
    );
    assert!(receipt(&home).exists(), "the second paste was written down");
    {
        let mut session = session();
        until(&mut session, "the second paste was undone", |_| {
            !receipt(&home).exists()
        });
    }
    assert_every_item_whole_or_untouched(&source, &destination);
}
