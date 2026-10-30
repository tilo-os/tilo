//! `tilolang repl` against a session's agent door.
//!
//! The first test needs nothing running: a door that serves the agent tools but no `tilo` (a
//! session from before programs on the door), which the REPL refuses cleanly. The second drives
//! a running nested session with Notes, Todo, Chat and Board (`TILOLANG_REPL_SESSION`: its
//! --ready file), doing real work there: queries, a bulk change shown as a preview and applied
//! on confirm, and undo of it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Answer one door connection as a session that serves the agent tools but runs no programs.
fn old_door(listener: &UnixListener) {
    let (stream, _) = listener.accept().unwrap();
    let mut writer = stream.try_clone().unwrap();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let request: Value = serde_json::from_str(&line).unwrap();
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let result = match request["method"].as_str() {
            Some("initialize") => json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "serverInfo": {"name": "tilo-runtime", "version": "0"}}),
            Some("tools/list") => {
                let tools: Vec<Value> = [
                    "tilo_ls",
                    "tilo_read",
                    "tilo_do",
                    "tilo_find",
                    "tilo_status",
                ]
                .map(|name| json!({"name": name}))
                .into();
                json!({ "tools": tools })
            }
            _ => json!({"content": [{"type": "text", "text": "apps"}]}),
        };
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
        writeln!(writer, "{reply}").unwrap();
    }
}

fn repl(args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .arg("repl")
        .args(args)
        .args(["--color", "never"])
        .env(
            "XDG_STATE_HOME",
            std::env::temp_dir().join("tilolang-repl-test"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_session_whose_door_runs_no_programs_is_refused_with_why() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let door = std::thread::spawn(move || old_door(&listener));
    let output = repl(&["--door", socket.to_str().unwrap()], "notes.notes\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("has no `tilo` tool") && stderr.contains("tilo_ls"),
        "says what the door lacks and what it serves: {stderr}"
    );
    assert!(output.stdout.is_empty(), "nothing was evaluated");
    door.join().unwrap();
}

/// The lines of `out` that a run printed after the entry `entry` and before the next prompt.
fn after<'a>(out: &'a str, entry: &str) -> Vec<&'a str> {
    out.lines()
        .skip_while(|line| !line.ends_with(entry))
        .skip(1)
        .take_while(|line| !line.contains("> "))
        .collect()
}

#[test]
#[ignore = "drives a running nested session: TILOLANG_REPL_SESSION names its --ready file"]
fn the_repl_queries_changes_in_bulk_with_a_preview_and_undoes_on_a_nested_session() {
    let session = std::env::var("TILOLANG_REPL_SESSION").expect("TILOLANG_REPL_SESSION");
    let script = "\
todo.tasks.items.len()
for t in [\"Take the bins out\", \"Buy oat milk and eggs\", \"Water the plants\", \"Return the library books\", \"Book a haircut\"] { todo.add(todo.tasks, t) }
confirm
todo.tasks.items.len()
todo.tasks.items.filter(t -> not t.done).len()
undo
todo.tasks.items.len()
history task
";
    let output = repl(&["--session", &session], script);
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{out}\n{err}");
    let before: usize = after(&out, "todo.tasks.items.len()")[0]
        .trim()
        .parse()
        .unwrap();
    let previewed = after(&out, "todo.add(todo.tasks, t) }").join("\n");
    assert!(previewed.contains("preview: 5 actions"), "{out}");
    assert!(previewed.contains("add ×5"), "{out}");
    let applied = after(&out, "confirm").join("\n");
    assert!(applied.contains("step 1: 5 changes in todo"), "{out}");
    let counts: Vec<usize> = out
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    assert_eq!(counts.first(), Some(&before), "{out}");
    assert!(counts.contains(&(before + 5)), "the five were added: {out}");
    assert!(
        after(&out, "undo")
            .join("\n")
            .contains("undone: step 1 in todo"),
        "{out}"
    );
    assert_eq!(
        counts.last(),
        Some(&before),
        "undo took all five back: {out}"
    );
    assert!(
        after(&out, "history task").join("\n").contains("(undone)"),
        "{out}"
    );
}

/// Answer a nested session's control socket as the person there: one agent admission waiting,
/// every request recorded in `seen`, until `stop` is set.
fn person_at_control(
    listener: &UnixListener,
    seen: &std::sync::Mutex<Vec<Value>>,
    stop: &std::sync::atomic::AtomicBool,
) {
    listener.set_nonblocking(true).unwrap();
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            Err(error) => panic!("{error}"),
        };
        stream.set_nonblocking(false).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let reply = match request["op"].as_str() {
            Some("actions_enumerate") => json!({"answer": {"actions": [
                {"provider": "permission-prompts", "id": "deny@p1"}
            ]}}),
            _ => json!({"answer": {}}),
        };
        seen.lock().unwrap().push(request);
        writeln!(writer, "{reply}").unwrap();
    }
}

/// The REPL is admitted to a nested session as the person's agent, and writes none of the
/// answers the person gives that agent: a `deny` or `ask` they set stays theirs.
#[test]
fn the_repl_is_admitted_as_the_persons_agent_and_never_writes_their_answers() {
    let runtime = tempfile::tempdir().unwrap();
    std::fs::create_dir(runtime.path().join("tilo-session")).unwrap();
    let door = UnixListener::bind(runtime.path().join("tilo-session/agent.sock")).unwrap();
    let control = UnixListener::bind(runtime.path().join("control.sock")).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let person = std::thread::spawn({
        let (seen, stop) = (seen.clone(), stop.clone());
        move || person_at_control(&control, &seen, &stop)
    });
    let door = std::thread::spawn(move || old_door(&door));
    let output = repl(&["--session", runtime.path().to_str().unwrap()], "");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    person.join().unwrap();
    door.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no `tilo` tool"),
        "it got past admission: {stderr}"
    );
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let admitted = seen
        .iter()
        .find(|request| request["id"] == "allow-once@p1")
        .unwrap_or_else(|| panic!("the person admits it: {seen:?}"));
    assert_eq!(admitted["args"]["agent"], "tilolang-repl");
    assert!(
        admitted["args"].get("preset").is_none(),
        "the door takes no preset: the connection works in its task run: {admitted}"
    );
    assert!(
        seen.iter()
            .all(|request| request["op"] != "allow" && request["op"] != "disallow"),
        "the REPL gives itself no answers: {seen:?}"
    );
}
