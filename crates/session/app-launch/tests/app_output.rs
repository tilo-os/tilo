//! What an app writes goes into the session's log, labelled with the app.
//!
//! A launch is started through the real [`Spawner`], with no launcher stand-in:
//! the process is the app's own, and what it writes reaches the log. An app
//! that fails before its window appears is the case that matters — the only
//! account of it is what it said, and the log has to keep it.
//!
//! The session's log is one stream for the whole process, and a launch reads
//! its app's pipes on threads of its own, so the subscriber here is the
//! process's and every test reads only the lines of the app it started.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_app_launch::spawner::{Event, Spawner};
use tilo_app_launch::{Kind, Planned};

/// Everything the session's log kept, shared by every test of this binary.
#[derive(Clone, Default)]
struct Said(Arc<Mutex<Vec<u8>>>);

impl Said {
    /// The lines the log kept about `app`: its own, and what the log said
    /// about them.
    fn of(&self, app: &str) -> Vec<String> {
        let labelled = format!("[app {app} ");
        let named = format!("app={app} ");
        String::from_utf8_lossy(&self.0.lock().expect("the log"))
            .lines()
            .filter(|line| line.contains(&labelled) || line.contains(&named))
            .map(str::to_owned)
            .collect()
    }

    /// Every line the log kept, for a failure that says what was there.
    fn all(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().expect("the log"))
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl std::io::Write for Said {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("the log").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The process's log, kept as the session log keeps an app's own output.
fn logging() -> Said {
    static SAID: std::sync::OnceLock<Said> = std::sync::OnceLock::new();
    SAID.get_or_init(|| {
        let said = Said::default();
        let writer = said.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        // Another test binary's subscriber, or one installed here already:
        // the log is the process's, and it is installed once.
        let _ = tracing::subscriber::set_global_default(subscriber);
        said
    })
    .clone()
}

/// The planned launch of `/bin/sh` running `script`, as the app `app`.
fn shell(app: &str, script: &str) -> Planned {
    Planned {
        app: app.to_owned(),
        title: "Notes".to_owned(),
        kind: Kind::System,
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".to_owned(), script.to_owned()],
        env: BTreeMap::new(),
        window_identities: Vec::new(),
    }
}

/// Start `planned` and wait until its launch reports how it ended.
fn ended(spawner: &Spawner, what: &str) -> Event {
    let started = Instant::now();
    loop {
        if let Some(event) = spawner.events().into_iter().next() {
            return event;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{what}: the app never reported how it ended"
        );
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

/// Wait until the log holds a line of `app` for which `done` holds: the
/// reader threads write at their own pace after the app's process ends.
#[track_caller]
fn until_logged(
    said: &Said,
    app: &str,
    what: &str,
    done: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let started = Instant::now();
    loop {
        let lines = said.of(app);
        if done(&lines) {
            return lines;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{what}; what the log kept: {:#?}",
            said.all()
        );
        std::thread::park_timeout(Duration::from_millis(20));
    }
}

#[test]
fn an_app_that_writes_and_exits_leaves_its_lines_in_the_log() {
    let said = logging();
    let app = "so.tilo.probe.writes";
    let mut spawner = Spawner::default();
    spawner
        .start(&shell(
            app,
            "printf 'a note it could not open\\n' >&2\n\
             printf 'and what it read first\\n'\n\
             exit 1",
        ))
        .expect("the app starts");
    let event = ended(&spawner, "the app exited");
    assert_eq!(event.end.to_string(), "exited with 1");
    let lines = until_logged(&said, app, "the app's own lines reach the log", |lines| {
        lines
            .iter()
            .any(|line| line.contains("a note it could not open"))
            && lines
                .iter()
                .any(|line| line.contains("and what it read first"))
    });
    assert!(
        lines
            .iter()
            .any(|line| line.contains("[app so.tilo.probe.writes stderr] a note it could not open")),
        "its standard error is labelled with the app and the stream: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("[app so.tilo.probe.writes stdout] and what it read first")),
        "its standard output too: {lines:#?}"
    );
    // What it said last is kept for the session's own report of the end.
    assert_eq!(
        event.said.stderr,
        ["a note it could not open"],
        "the launch hands its stderr tail to the session"
    );
}

#[test]
fn an_app_that_chatters_does_not_flood_the_log() {
    let said = logging();
    let app = "so.tilo.probe.chatty";
    let mut spawner = Spawner::default();
    // Far more lines than a relay allows, written as fast as the pipe takes
    // them.
    spawner
        .start(&shell(
            app,
            "i=0\nwhile [ $i -lt 3000 ]; do printf 'line %s\\n' \"$i\" >&2; i=$((i+1)); done\nexit 1",
        ))
        .expect("the app starts");
    let event = ended(&spawner, "the app exited");
    let lines = until_logged(&said, app, "the log says how much it left out", |lines| {
        lines
            .iter()
            .any(|line| line.contains("lines of this app were not relayed"))
    });
    let relayed = lines.iter().filter(|line| line.contains("] line ")).count();
    assert!(
        relayed < 3000,
        "the log does not carry every line a chatty app wrote: {relayed}"
    );
    assert_eq!(
        event.said.stderr.last().map(String::as_str),
        Some("line 2999"),
        "the tail keeps the newest line, which is the one a failure is near"
    );
}
