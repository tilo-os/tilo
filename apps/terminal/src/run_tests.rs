//! A session's command runs end to end: the marks its shell prints become
//! run items with their output and parsed items, and an agent's `run`,
//! `rerun` and `rerun-test` are typed into the session and answered with
//! the run they became.

use std::sync::mpsc::{Receiver, Sender, channel};

use base64::Engine as _;
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{Bytes, Settled};

use crate::TerminalWidget;
use crate::resource::{ItemKind, RunAnswer, Terminal};
use crate::sessions::Sessions;
use crate::target::RecentTarget;

type App = ui::framework::testing::TestApp<crate::TerminalShell>;

const KEY: &str = "0123456789abcdef0123456789abcdef";
use tilo_test_env::hang_bound;

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn prompt() -> String {
    format!("\x1b]133;A;k={KEY}\x07$ ")
}

/// What the session's shell prints for one command: its marks around
/// `output`.
fn ran(command: &str, output: &str, status: i32) -> Vec<u8> {
    format!(
        "{command}\r\n\x1b]133;C;k={KEY};cmd={};cwd={}\x07{output}\x1b]133;D;{status};k={KEY}\x07{}",
        b64(command),
        b64("/src/demo"),
        prompt()
    )
    .into_bytes()
}

/// The output side of the session's pty, written by the test's shell.
struct Printed(Receiver<Vec<u8>>, Vec<u8>);

impl std::io::Read for Printed {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(bytes) => self.1 = bytes,
                Err(_) => return Ok(0),
            }
        }
        let n = buffer.len().min(self.1.len());
        buffer[..n].copy_from_slice(&self.1[..n]);
        self.1.drain(..n);
        Ok(n)
    }
}

/// The input side, read by the test's shell.
struct Typed(Sender<Vec<u8>>);

impl std::io::Write for Typed {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(buffer.to_vec());
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A shell that marks its runs: each line typed into it runs, printing
/// `output` for it with status 0, and the lines are kept in `lines`.
fn shell(
    typed: Receiver<Vec<u8>>,
    print: Sender<Vec<u8>>,
    output: &'static str,
    lines: Sender<String>,
) {
    std::thread::spawn(move || {
        let mut line = Vec::new();
        while let Ok(bytes) = typed.recv() {
            for byte in bytes {
                match byte {
                    // Ctrl-U clears what was typed at the prompt.
                    0x15 => line.clear(),
                    b'\r' => {
                        let command = String::from_utf8_lossy(&line).into_owned();
                        line.clear();
                        let _ = print.send(ran(&command, output, 0));
                        let _ = lines.send(command);
                    }
                    byte => line.push(byte),
                }
            }
        }
    });
}

/// A terminal on its session record whose process is a marking shell:
/// what it printed so far goes through `print`; every line typed runs.
fn marked_session(output: &'static str) -> (App, Sender<Vec<u8>>, Receiver<String>) {
    let mut app = App::new().expect("terminal");
    let (print, lines) = run_shell(&mut app, output);
    (app, print, lines)
}

/// Give the session `app` shows a marking shell as its process.
fn run_shell(app: &mut App, output: &'static str) -> (Sender<Vec<u8>>, Receiver<String>) {
    let (print, printed) = channel();
    let (typed_tx, typed) = channel();
    let (lines_tx, lines) = channel();
    shell(typed, print.clone(), output, lines_tx);
    let mut parts = Some((printed, typed_tx));
    let sessions = Sessions::scripted(RecentTarget::none(), move |_, _, _| {
        let (printed, typed) = parts.take().expect("one process");
        let mut backend = crate::inner::TerminalBackend::for_test();
        backend.reader = Box::new(Printed(printed, Vec::new()));
        backend.writer = Box::new(Typed(typed));
        backend.runs = Some(KEY.into());
        Ok(backend)
    });
    app.with_surface(|terminal: &mut TerminalWidget| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    app.settle();
    print.send(prompt().into_bytes()).unwrap();
    (print, lines)
}

/// Settle `app` until its session record satisfies `done`.
fn settle_until(app: &mut App, done: impl Fn(&Terminal) -> bool) {
    let deadline = ui::time::now() + hang_bound();
    while ui::time::now() < deadline {
        app.settle();
        if done(&app.state()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let inspected = app
        .with_surface(|terminal: &mut TerminalWidget| terminal.inspect_session())
        .unwrap();
    panic!(
        "the session record never got there: {:#?} {inspected}",
        app.state()
    );
}

/// The next line typed at the session's shell. The terminal types a line
/// `run` asked for only on a frame after it saw the shell's prompt mark,
/// and that mark arrives from the reader thread whenever it does, so
/// keep settling until the shell reads the line.
fn typed_line(app: &mut App, lines: &Receiver<String>) -> String {
    let deadline = ui::time::now() + hang_bound();
    while ui::time::now() < deadline {
        app.settle();
        if let Ok(line) = lines.try_recv() {
            return line;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the shell never got a line: {:#?}", app.state());
}

/// A failing `cargo test` the person ran is a run with its output and one
/// item per test; an agent reruns the failing one by its name, and the
/// request settles with the run it became, by the agent.
#[test]
fn an_agent_reruns_the_failing_test_of_a_run() {
    let fixture = include_str!("parse/fixtures/nextest.txt");
    let (mut app, print, lines) = marked_session("test result: ok. 1 passed\r\n");
    print
        .send(ran("cargo test -p terminal --lib fixture_", fixture, 101))
        .unwrap();
    settle_until(&mut app, |state| state.runs.len() == 1);

    let state = app.state();
    let (first, run) = state.runs.iter().next().unwrap();
    assert_eq!(run.command.0, "cargo test -p terminal --lib fixture_");
    assert_eq!(run.cwd.0, "/src/demo");
    assert_eq!(run.exit, 101);
    assert_eq!(run.tool.0, "cargo");
    assert!(matches!(run.output, Bytes::Blob { len, .. } if len == fixture.len() as u64));
    let failed: Vec<_> = run
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::TestFailed)
        .collect();
    assert_eq!(failed.len(), 1, "{:#?}", run.items);
    let test = failed[0].code.clone().unwrap();
    let item = item_resource_id::<Terminal>("runs", first);
    let offered: Vec<String> = app
        .actions()
        .unwrap()
        .into_iter()
        .filter(|action| action.address.resource == item)
        .map(|action| action.address.action)
        .collect();
    for verb in ["rerun", "rerun-test", "open-item", "open"] {
        assert!(offered.iter().any(|id| id == verb), "{verb}: {offered:?}");
    }

    let agent = ActionInvoker::actor("agent:helper")
        .with_kind(ActorKind::Agent)
        .acting_for("person:ada");
    app.call_by(
        agent,
        &item_resource_id::<Terminal>("runs", first),
        "rerun-test",
        serde_json::json!({ "test": test }),
    )
    .expect("the agent's rerun runs and settles");
    assert_eq!(
        typed_line(&mut app, &lines),
        format!("cargo test -p terminal -- --exact {test}"),
        "typed at the prompt, once"
    );
    settle_until(&mut app, |state| {
        state.runs.len() == 2 && state.requests.pending().next().is_none()
    });

    let state = app.state();
    let (request, entry) = state
        .requests
        .entries()
        .next()
        .expect("the agent's request");
    assert_eq!(entry.value.requested_by.0, "agent:helper");
    let rerun = state
        .runs
        .get(request)
        .expect("the run is the request's item");
    assert_eq!(rerun.actor.0, "agent:helper");
    let ada =
        |person: &Option<ui::tilo_resource::Atom>| person.as_ref().map(|person| person.0.clone());
    assert_eq!(ada(&entry.value.for_person), Some("person:ada".to_owned()));
    assert_eq!(
        ada(&rerun.for_person),
        Some("person:ada".to_owned()),
        "the run names the person the agent ran it for"
    );
    assert_eq!(rerun.exit, 0);
    assert_eq!(
        entry.settled,
        Some(Settled::Done(RunAnswer {
            run: Some(request.clone()),
            exit: Some(0),
        }))
    );
}

/// A command line with a control character is refused before anything is
/// queued, and a session whose process runs nowhere refuses `run`.
#[test]
fn run_takes_one_command_line_for_a_running_session() {
    let mut app = App::new().expect("terminal");
    assert!(
        app.call("run", serde_json::json!({ "command": "ls" }))
            .is_err(),
        "no process runs the session"
    );
    let (mut app, _print, lines) = marked_session("");
    settle_until(&mut app, |state| !state.pane_id().is_empty());
    for command in ["", "ls\nrm -rf /", "printf \x1b[31m"] {
        assert!(
            app.call("run", serde_json::json!({ "command": command }))
                .is_err(),
            "{command:?}"
        );
    }
    assert!(app.state().requests.entries().next().is_none());
    app.call("run", serde_json::json!({ "command": "ls -la" }))
        .expect("runs");
    assert_eq!(typed_line(&mut app, &lines), "ls -la");
    settle_until(&mut app, |state| state.runs.len() == 1);
}

/// Runs of two sessions are one history: the person's own record keeps
/// both, each naming its session and run and who ran it, and its search
/// finds a command whichever session ran it.
#[test]
fn two_sessions_are_searchable_together() {
    use crate::history::CommandHistory;
    use ui::tilo_resource::Resource as _;

    let (mut app, print, _lines) = marked_session("");
    print
        .send(ran("cargo build -p first", "Finished\r\n", 0))
        .unwrap();
    settle_until(&mut app, |state| state.runs.len() == 1);
    let first = app.record().uri();
    let sentences = app.sentences(&app.history());
    assert!(
        sentences
            .iter()
            .any(|sentence| sentence == "Ran cargo build -p first"),
        "{sentences:?}"
    );

    // A new terminal in the same window is a new session.
    app.open_here(&[]).expect("a second session");
    let second = app.record().uri();
    assert_ne!(second, first, "a new terminal starts a new session");
    assert!(
        app.state().runs.is_empty(),
        "the new session ran nothing yet"
    );
    let (print, _lines) = run_shell(&mut app, "");
    print
        .send(ran("cargo test -p second", "test result: ok\r\n", 0))
        .unwrap();
    settle_until(&mut app, |state| state.runs.len() == 1);

    let history = app
        .person::<CommandHistory>()
        .expect("the person's history");
    let history = history.get();
    let sessions: Vec<(&str, &str)> = history
        .runs
        .values()
        .map(|run| (run.command.0.as_str(), run.session.0.as_str()))
        .collect();
    assert_eq!(
        sessions,
        [
            ("cargo build -p first", first.as_str()),
            ("cargo test -p second", second.as_str()),
        ]
    );
    let me = app.state().runs.values().next().unwrap().actor.clone();
    assert!(!me.0.is_empty());
    assert!(history.runs.values().all(|run| run.actor == me));
    assert_eq!(
        history
            .matching("cargo")
            .map(|run| run.command.0.as_str())
            .collect::<Vec<_>>(),
        ["cargo test -p second", "cargo build -p first"],
        "newest first"
    );
    let found: Vec<String> = CommandHistory::search(&history)
        .into_iter()
        .map(|entry| entry.title)
        .collect();
    assert!(
        found.iter().any(|title| title == "cargo build -p first")
            && found.iter().any(|title| title == "cargo test -p second"),
        "{found:?}"
    );
}
