//! A session's command runs end to end: the marks its shell prints become
//! run items with their output and parsed items, and an agent's `run`,
//! `rerun` and `rerun-test` are typed into the session and answered with
//! the run they became.

use std::sync::mpsc::{Receiver, Sender, channel};

use base64::Engine as _;
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{Bytes, Settled};

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
/// `output`, numbered as the shell's first command is.
fn ran(command: &str, output: &str, status: i32) -> Vec<u8> {
    numbered(1, command, output, status)
}

/// [`ran`] with the command's own place in the shell's history.
fn numbered(number: u64, command: &str, output: &str, status: i32) -> Vec<u8> {
    format!(
        "{command}\r\n\x1b]133;C;k={KEY};cmd={};cwd={};t={number}\x07{output}\
         \x1b]133;D;{status};k={KEY};t={number}\x07{}",
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
/// `output` for it with status 0, and the lines are kept in `lines`. Its
/// numbers count the lines typed into it, its history being its own.
fn shell(
    typed: Receiver<Vec<u8>>,
    print: Sender<Vec<u8>>,
    output: &'static str,
    lines: Sender<String>,
) {
    std::thread::spawn(move || {
        let mut line = Vec::new();
        let mut number = 0_u64;
        while let Ok(bytes) = typed.recv() {
            for byte in bytes {
                match byte {
                    0x15 => line.clear(),
                    b'\r' => {
                        let command = String::from_utf8_lossy(&line).into_owned();
                        line.clear();
                        // Only a line with something in it runs, and only it
                        // takes a place in the shell's history.
                        if command.trim().is_empty() {
                            let _ = print.send(prompt().into_bytes());
                            continue;
                        }
                        // The shell counts a line before it runs it, as
                        // its history does. The two count the same lines
                        // (this one receives every line the terminal types,
                        // and the person types none), so a line asked for is
                        // at the place the terminal counted for it.
                        number = number.saturating_add(1);
                        let _ = print.send(numbered(number, &command, output, 0));
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
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
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
        .with_surface(|terminal: &mut crate::TerminalView| terminal.inspect_session())
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
    // The run's package and target selection come back, so only the
    // library that reported the test builds again.
    assert_eq!(
        typed_line(&mut app, &lines),
        format!("cargo test -p terminal --lib -- --exact {test}"),
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

/// Tab and Shift+Tab are the shell's keys while the grid has the keyboard:
/// the window hands them to the session's process (completion, back-tab)
/// rather than moving focus to the next control.
#[test]
fn tab_reaches_the_session_while_the_grid_has_the_keyboard() {
    let mut app = App::new().expect("terminal");
    let (_print, printed) = channel::<Vec<u8>>();
    let (typed_tx, typed) = channel();
    let mut parts = Some((printed, typed_tx));
    let sessions = Sessions::scripted(RecentTarget::none(), move |_, _, _| {
        let (printed, typed) = parts.take().expect("one process");
        let mut backend = crate::inner::TerminalBackend::for_test();
        backend.reader = Box::new(Printed(printed, Vec::new()));
        backend.writer = Box::new(Typed(typed));
        Ok(backend)
    });
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    app.settle();
    let grid = app
        .with_surface(|terminal: &mut crate::TerminalView| terminal.grid_rect())
        .unwrap();
    assert!(grid.area() > 0.0, "the grid is laid out: {grid:?}");
    let centre = grid.center();
    app.harness().click_at(centre.x, centre.y);
    app.settle();
    while typed.try_recv().is_ok() {}
    let focused = app
        .with_surface(|terminal: &mut crate::TerminalView| {
            ui::widget::Widget::is_focused(terminal.terminal())
        })
        .unwrap();
    assert!(focused, "a click on the grid gives it the keyboard");

    let sent = |app: &mut App, modifiers: &[&str], key: &str| {
        app.harness().set_modifiers(
            &modifiers
                .iter()
                .map(|m| (*m).to_owned())
                .collect::<Vec<_>>(),
        );
        app.harness().key(key);
        app.harness().set_modifiers(&[]);
        app.settle();
        let mut bytes = Vec::new();
        while let Ok(chunk) = typed.try_recv() {
            bytes.extend(chunk);
        }
        bytes
    };
    assert_eq!(sent(&mut app, &[], "Tab"), b"\t", "Tab is the shell's");
    assert_eq!(
        sent(&mut app, &["shift"], "Tab"),
        b"\x1b[Z",
        "Shift+Tab is back-tab"
    );
    assert_eq!(
        sent(&mut app, &[], "Tab"),
        b"\t",
        "the grid still has the keyboard"
    );
}

/// The environment of a Tilo-side session's test: one granted folder, and
/// the actions asked of it.
struct Granted {
    grants: Vec<ush_tilo::CapOverride>,
    acted: std::sync::Mutex<Vec<(tilo_environment::files::FileAction, Option<String>)>>,
}
impl ush_tilo::Environment for Granted {
    fn grants(&self) -> Vec<ush_tilo::CapOverride> {
        self.grants.clone()
    }
    fn act(
        &self,
        action: tilo_environment::files::FileAction,
        run: Option<&ush_shell::Run>,
    ) -> Result<ush_tilo::Acted, String> {
        action.admit(&self.grants, None)?;
        self.acted
            .lock()
            .unwrap()
            .push((action, run.map(|run| run.label.clone())));
        Ok(ush_tilo::Acted {
            entry: Some("entry".into()),
            operations: 1,
        })
    }
    fn keep(&self) -> std::io::Result<Box<dyn ush_tilo::Keeping>> {
        // This session writes only what travels inside its actions.
        Err(std::io::Error::other("this test keeps no content"))
    }
}

/// A session whose shell runs on the Tilo side: its host reads only what
/// the environment grants and writes only through the environment's
/// actions, and Terminal records its runs from the marks as for any shell.
#[test]
fn a_tilo_side_session_records_its_runs() {
    use std::sync::{Arc, Mutex};
    use ush_shell::{Input, Output, Session, Shell, Stdio};
    use ush_tilo::{NativeFiles, NoPrograms, Nowhere, TiloHost};

    let dir = tempfile::tempdir().unwrap();
    let granted = dir.path().canonicalize().unwrap().join("demo");
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::write(granted.join("notes.md"), "one\ntwo\n").unwrap();
    let environment = Arc::new(Granted {
        grants: vec![tilo_environment::dir_grant(&granted, true).unwrap()],
        acted: Mutex::new(Vec::new()),
    });

    let mut app = App::new().expect("terminal");
    let (print, printed) = channel();
    let (typed_tx, typed) = channel();
    let shell_environment = Arc::clone(&environment);
    let cwd = granted.clone();
    std::thread::spawn(move || {
        let stderr = Output::new(Crlf(print.clone()));
        let host = TiloHost::new(
            shell_environment,
            Arc::new(NativeFiles),
            Arc::new(NoPrograms),
            Arc::new(Nowhere),
            stderr.clone(),
        );
        let io = Stdio {
            stdin: Input::new(Cooked {
                typed,
                echo: print.clone(),
                line: Vec::new(),
                ready: std::collections::VecDeque::new(),
            }),
            stdout: Output::new(Crlf(print)),
            stderr,
        };
        let env = [("PS1".to_string(), "$ ".to_string())];
        let mut shell = Shell::new(Arc::new(host), env, cwd, io);
        shell.interactive(&Session {
            runs_key: Some(KEY.into()),
        });
    });
    let mut parts = Some((printed, typed_tx));
    let sessions = Sessions::scripted(RecentTarget::none(), move |_, _, _| {
        let (printed, typed) = parts.take().expect("one process");
        let mut backend = crate::inner::TerminalBackend::for_test();
        backend.reader = Box::new(Printed(printed, Vec::new()));
        backend.writer = Box::new(Typed(typed));
        backend.runs = Some(KEY.into());
        Ok(backend)
    });
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    settle_until(&mut app, |state| !state.pane_id().is_empty());

    app.call(
        "run",
        serde_json::json!({ "command": "cat notes.md | wc -l" }),
    )
    .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 1 && state.requests.pending().next().is_none()
    });
    app.call("run", serde_json::json!({ "command": "echo hi > out.txt" }))
        .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 2 && state.requests.pending().next().is_none()
    });

    let state = app.state();
    let read = state
        .runs
        .iter()
        .map(|(_, run)| run)
        .find(|run| run.command.0 == "cat notes.md | wc -l")
        .expect("the read is a run");
    assert_eq!(read.cwd.0.clone(), granted.display().to_string());
    assert_eq!(read.exit, 0);
    assert!(
        matches!(read.output, Bytes::Blob { len, .. } if len == "2\r\n".len() as u64),
        "{:?}",
        read.output
    );
    let wrote = state
        .runs
        .iter()
        .map(|(_, run)| run)
        .find(|run| run.command.0 == "echo hi > out.txt")
        .expect("the write is a run");
    assert_eq!(wrote.exit, 0);
    assert!(
        !granted.join("out.txt").exists(),
        "the shell wrote nothing itself"
    );
    let acted = environment.acted.lock().unwrap().clone();
    assert_eq!(acted.len(), 1, "{acted:?}");
    assert_eq!(acted[0].0.verb(), "write");
    assert_eq!(
        acted[0].1.as_deref(),
        Some("echo hi > out.txt"),
        "the redirect is an action of that command line's run"
    );
}

/// What a pty's line discipline does between Terminal and the shell, for a
/// shell that runs on a thread here: typed bytes reach it a line at a time
/// (`\r` ends a line, Ctrl-U clears it) and are echoed; what it prints gets
/// `\r\n` line ends.
struct Cooked {
    typed: Receiver<Vec<u8>>,
    echo: Sender<Vec<u8>>,
    line: Vec<u8>,
    ready: std::collections::VecDeque<u8>,
}

impl std::io::Read for Cooked {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.ready.is_empty() {
            let Ok(bytes) = self.typed.recv() else {
                return Ok(0);
            };
            for byte in bytes {
                match byte {
                    0x15 => self.line.clear(),
                    b'\r' | b'\n' => {
                        let mut echoed = std::mem::take(&mut self.line);
                        self.ready.extend(echoed.iter().copied());
                        self.ready.push_back(b'\n');
                        echoed.extend_from_slice(b"\r\n");
                        let _ = self.echo.send(echoed);
                    }
                    byte => self.line.push(byte),
                }
            }
        }
        let n = buffer.len().min(self.ready.len());
        for (slot, byte) in buffer.iter_mut().zip(self.ready.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

struct Crlf(Sender<Vec<u8>>);

impl std::io::Write for Crlf {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let mut out = Vec::with_capacity(buffer.len());
        for &byte in buffer {
            if byte == b'\n' {
                out.push(b'\r');
            }
            out.push(byte);
        }
        let _ = self.0.send(out);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// ush's own shell as the session's process: `ls | wc -l` runs as two of
/// Tilo's own commands joined by a pipe in memory, and the marks it writes
/// become the session's run, with its line, directory, status and output.
#[test]
fn a_ush_session_records_the_run_of_a_pipeline() {
    use std::sync::Arc;
    use ush_shell::{Input, MemoryHost, Output, Session, Shell, Stdio};

    let mut app = App::new().expect("terminal");
    let (print, printed) = channel();
    let (typed_tx, typed) = channel();
    std::thread::spawn(move || {
        let host = MemoryHost::new();
        host.file("/src/demo/notes.md", "")
            .file("/src/demo/todo.md", "");
        let io = Stdio {
            stdin: Input::new(Cooked {
                typed,
                echo: print.clone(),
                line: Vec::new(),
                ready: std::collections::VecDeque::new(),
            }),
            stdout: Output::new(Crlf(print.clone())),
            stderr: Output::new(Crlf(print)),
        };
        let env = [("PS1".to_string(), "$ ".to_string())];
        let mut shell = Shell::new(Arc::new(host), env, "/src/demo".into(), io);
        shell.interactive(&Session {
            runs_key: Some(KEY.into()),
        });
    });
    let mut parts = Some((printed, typed_tx));
    let sessions = Sessions::scripted(RecentTarget::none(), move |_, _, _| {
        let (printed, typed) = parts.take().expect("one process");
        let mut backend = crate::inner::TerminalBackend::for_test();
        backend.reader = Box::new(Printed(printed, Vec::new()));
        backend.writer = Box::new(Typed(typed));
        backend.runs = Some(KEY.into());
        Ok(backend)
    });
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    settle_until(&mut app, |state| !state.pane_id().is_empty());

    app.call("run", serde_json::json!({ "command": "ls | wc -l" }))
        .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 1 && state.requests.pending().next().is_none()
    });

    let state = app.state();
    let (_, run) = state.runs.iter().next().unwrap();
    assert_eq!(run.command.0, "ls | wc -l");
    assert_eq!(run.cwd.0, "/src/demo");
    assert_eq!(run.exit, 0);
    assert!(
        matches!(run.output, Bytes::Blob { len, .. } if len == "2\r\n".len() as u64),
        "{:?}",
        run.output
    );
    assert!(!run.undoable, "it only looked");
}

/// What the terminal types reaches ush's shell as keys: no line discipline
/// between them, the shell's own editor reads each byte.
struct Raw(Receiver<Vec<u8>>, Vec<u8>);

impl std::io::Read for Raw {
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

/// Give the session `app` shows ush's own shell as its process, on `host`,
/// with its line editor reading the keys the terminal sends.
fn ush_shell(app: &mut App, host: ush_shell::MemoryHost) {
    use std::sync::Arc;
    use ush_shell::{Input, Output, Session, Shell, Stdio};

    let (print, printed) = channel();
    let (typed_tx, typed) = channel();
    std::thread::spawn(move || {
        let io = Stdio {
            stdin: Input::new(Raw(typed, Vec::new())),
            stdout: Output::new(Crlf(print.clone())),
            stderr: Output::new(Crlf(print)),
        };
        let env = [("PS1".to_string(), "$ ".to_string())];
        let mut shell = Shell::new(Arc::new(host), env, "/src/demo".into(), io);
        shell.interactive(&Session {
            runs_key: Some(KEY.into()),
        });
    });
    let mut parts = Some((printed, typed_tx));
    let sessions = Sessions::scripted(RecentTarget::none(), move |_, _, _| {
        let (printed, typed) = parts.take().expect("one process");
        let mut backend = crate::inner::TerminalBackend::for_test();
        backend.reader = Box::new(Printed(printed, Vec::new()));
        backend.writer = Box::new(Typed(typed));
        backend.runs = Some(KEY.into());
        Ok(backend)
    });
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.set_sessions(sessions))
        .expect("the terminal surface is hosted");
    settle_until(app, |state| !state.pane_id().is_empty());
}

fn demo_host() -> ush_shell::MemoryHost {
    let host = ush_shell::MemoryHost::new().with_terminal(80);
    host.file("/src/demo/notes.md", "")
        .file("/src/demo/todo.md", "");
    host
}

/// Give the grid the keyboard, as a click on it does.
fn focus_grid(app: &mut App) {
    let grid = app
        .with_surface(|terminal: &mut crate::TerminalView| terminal.grid_rect())
        .unwrap();
    let centre = grid.center();
    app.harness().click_at(centre.x, centre.y);
    app.settle();
}

fn press(app: &mut App, modifiers: &[&str], key: &str) {
    app.harness().set_modifiers(
        &modifiers
            .iter()
            .map(|m| (*m).to_owned())
            .collect::<Vec<_>>(),
    );
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

/// The person types with the editing keys at ush's prompt — Home, End,
/// Left, Ctrl+W, Backspace — and the line the editor made is the run the
/// session records.
#[test]
fn a_person_edits_at_ushs_prompt_and_the_run_is_recorded() {
    let mut app = App::new().expect("terminal");
    ush_shell(&mut app, demo_host());
    focus_grid(&mut app);
    app.harness().type_text("ls | wc");
    press(&mut app, &[], "Home");
    app.harness().type_text("echo x; ");
    press(&mut app, &[], "End");
    app.harness().type_text(" -l oops");
    press(&mut app, &["ctrl"], "w");
    press(&mut app, &[], "Backspace");
    press(&mut app, &[], "Left");
    press(&mut app, &[], "Left");
    press(&mut app, &[], "Right");
    press(&mut app, &[], "Right");
    press(&mut app, &[], "Enter");
    settle_until(&mut app, |state| state.runs.len() == 1);

    let state = app.state();
    let (_, run) = state.runs.iter().next().unwrap();
    assert_eq!(run.command.0, "echo x; ls | wc -l");
    assert_eq!(run.exit, 0);
    assert!(
        matches!(run.output, Bytes::Blob { len, .. } if len == "x\r\n2\r\n".len() as u64),
        "{:?}",
        run.output
    );
}

/// Up at ush's prompt recalls the person's last run from their own history
/// record, which Terminal answers the shell from: a new session, whose
/// shell ran nothing yet, gets the line the first session ran.
#[test]
fn up_recalls_the_last_run_from_the_persons_history() {
    let mut app = App::new().expect("terminal");
    ush_shell(&mut app, demo_host());
    app.call("run", serde_json::json!({ "command": "ls | wc -l" }))
        .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 1 && state.requests.pending().next().is_none()
    });

    app.open_here(&[]).expect("a second session");
    assert!(app.state().runs.is_empty());
    ush_shell(&mut app, demo_host());
    focus_grid(&mut app);
    press(&mut app, &[], "Up");
    // The answer comes back through the terminal; Enter is pressed at once
    // and the line waits for it.
    press(&mut app, &[], "Enter");
    settle_until(&mut app, |state| state.runs.len() == 1);
    let state = app.state();
    let (_, run) = state.runs.iter().next().unwrap();
    assert_eq!(run.command.0, "ls | wc -l");
    assert!(matches!(run.output, Bytes::Blob { len, .. } if len == "2\r\n".len() as u64));
}

/// `sleep 0.3 &` runs as a job beside the shell, with a run of its own,
/// and `fg` waits for it: three runs, the job's among them, its command its
/// own.
#[test]
fn a_background_job_has_its_own_run_and_fg_waits_for_it() {
    let mut app = App::new().expect("terminal");
    ush_shell(&mut app, demo_host().with_real_time());
    app.call("run", serde_json::json!({ "command": "sleep 0.3 &" }))
        .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 1 && state.requests.pending().next().is_none()
    });
    app.call("run", serde_json::json!({ "command": "fg" }))
        .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 3 && state.requests.pending().next().is_none()
    });
    let state = app.state();
    let mut commands: Vec<(String, i32)> = state
        .runs
        .values()
        .map(|run| (run.command.0.clone(), run.exit))
        .collect();
    commands.sort();
    assert_eq!(
        commands,
        [
            ("fg".to_owned(), 0),
            ("sleep 0.3".to_owned(), 0),
            ("sleep 0.3 &".to_owned(), 0)
        ]
    );
    // The person's history has the lines they ran, not the job.
    let history = app
        .person::<crate::history::CommandHistory>()
        .expect("the person's history")
        .get();
    let remembered: Vec<&str> = history
        .runs
        .values()
        .map(|run| run.command.0.as_str())
        .collect();
    assert_eq!(remembered, ["sleep 0.3 &", "fg"]);
}

/// An agent's `run` in a ush session gets the compact agent rendering
/// without asking for it; the person's run of the same line gets the text.
#[test]
fn an_agents_run_gets_the_agent_rendering() {
    use ui::tilo_resource::Resource as _;

    let mut app = App::new().expect("terminal");
    let host = demo_host();
    for i in 0..3 {
        host.file(&format!("/src/demo/p/f{i}.txt"), "");
    }
    ush_shell(&mut app, host);
    app.call("run", serde_json::json!({ "command": "ls -R p" }))
        .expect("runs");
    settle_until(&mut app, |state| {
        state.runs.len() == 1 && state.requests.pending().next().is_none()
    });
    let agent = ActionInvoker::actor("agent:helper")
        .with_kind(ActorKind::Agent)
        .acting_for("person:ada");
    app.call_by(
        agent,
        Terminal::KIND,
        "run",
        serde_json::json!({ "command": "ls -R p" }),
    )
    .expect("the agent's run");
    settle_until(&mut app, |state| {
        state.runs.len() == 2 && state.requests.pending().next().is_none()
    });
    let state = app.state();
    let output = |actor: &str| {
        let run = state
            .runs
            .values()
            .find(|run| (run.actor.0 == "agent:helper") == (actor == "agent"))
            .unwrap();
        let Bytes::Blob { hash, .. } = &run.output else {
            panic!("printed nothing: {run:?}");
        };
        let hex = hash.iter().fold(String::new(), |mut hex, b| {
            let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{b:02x}"));
            hex
        });
        String::from_utf8(app.content().read(&hex).expect("the output")).unwrap()
    };
    assert!(
        output("person").contains("p:\r\nf0.txt\r\nf1.txt\r\nf2.txt"),
        "{}",
        output("person")
    );
    assert!(
        output("agent").contains("p/: f0.txt f1.txt f2.txt"),
        "{}",
        output("agent")
    );
}
