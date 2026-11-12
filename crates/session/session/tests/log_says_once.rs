//! What the session's log says of a state, and how often: once, when the state starts or ends.
//!
//! The session reads its package store and builds its surfaces on every frame, so a line about a
//! state that has not changed — a store whose registry does not read, a machine with no GPU to
//! draw a surface on — is written by the frame that finds it and by no frame after it. Written
//! every frame, those lines are thousands a second, and the one line that says what went wrong is
//! buried in them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session is the thing measured"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use tracing_subscriber::fmt::MakeWriter;

/// One display's size, and the person at the session.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";

/// What the log says of the store, as the frames of a test find it.
const UNREADABLE: &str = "the app store's registry does not read";
/// What the log says when the store reads again after it did not.
const READS_AGAIN: &str = "the app store's registry reads again";

/// The lines the log said, as text.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("the log").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Lines {
    /// How many lines said `what` since the log started.
    fn said(&self, what: &str) -> usize {
        let said = self.0.lock().expect("the log");
        let text = String::from_utf8_lossy(&said);
        text.lines().filter(|line| line.contains(what)).count()
    }

    /// Every line as text, for the message of an assertion that failed.
    fn text(&self) -> String {
        let said = self.0.lock().expect("the log");
        String::from_utf8_lossy(&said).into_owned()
    }
}

/// Run `inside` with the log going to `log`, as one process's session does.
fn logged<T>(log: &Lines, inside: impl FnOnce() -> T) -> T {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::with_default(subscriber, inside)
}

/// A headless session on one display.
fn session() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session")
}

/// Step `session` for `count` frames, as the display's refresh asks it to.
fn step(session: &mut Session, count: usize) {
    for _ in 0..count {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// The registry the person's package store keeps, under the home this test set up.
fn registry(data: &Path) -> PathBuf {
    data.join("tilo-apps").join("registry.toml")
}

/// A registry that is there and does not read: a link to itself, which no `stat` follows to a
/// file, on any machine and as any user.
fn unreadable_registry(data: &Path) {
    let path = registry(data);
    std::fs::create_dir_all(path.parent().expect("the store's directory")).expect("the store");
    std::os::unix::fs::symlink(&path, &path).expect("a link to itself");
}

#[test]
fn a_store_that_does_not_read_is_said_once_and_its_recovery_too() {
    let home = tilo_test_env::TestHome::new().expect("a home of its own");
    let log = Lines::default();
    logged(&log, || {
        let mut session = session();
        // A device with nothing installed from the store keeps no registry file: that is how
        // every device starts, so the frames that read the empty store say nothing of it.
        step(&mut session, 240);
        assert_eq!(
            log.said(UNREADABLE),
            0,
            "a store with no registry file yet is not a failure:\n{}",
            log.text()
        );

        // A registry that is there and does not read is said once, not once for each of the
        // frames that find it: the store is read on every frame of the session's loop.
        unreadable_registry(home.data());
        step(&mut session, 240);
        assert_eq!(
            log.said(UNREADABLE),
            1,
            "a store that does not read is said once, however many frames find it:\n{}",
            log.text()
        );

        // And the frame that finds it reading again says so, once.
        let registry = registry(home.data());
        std::fs::remove_file(&registry).expect("the broken registry is taken away");
        std::fs::write(&registry, "").expect("an empty registry");
        step(&mut session, 240);
        assert_eq!(
            log.said(UNREADABLE),
            1,
            "a store that reads again does not keep warning:\n{}",
            log.text()
        );
        assert_eq!(
            log.said(READS_AGAIN),
            1,
            "the recovery is said once:\n{}",
            log.text()
        );
    });
}

#[test]
fn drawing_on_the_cpu_is_said_once_for_the_process() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let log = Lines::default();
    // A session builds a surface for each sheet, panel and bar it shows, and each of them is
    // placed on its display once the displays are known, so the same surface is built more than
    // once before it draws. Where the toolkit draws them is one answer for the whole machine, so
    // it is said once, whatever happens to be built.
    logged(&log, || {
        let mut session = session();
        step(&mut session, 240);
    });
    assert!(
        log.said("frames are drawn on the CPU") <= 1,
        "a machine with no GPU says so once, not once per surface or frame:\n{}",
        log.text()
    );
}
