//! Adding a directory bot's program saves it as the person's shortcut, through the same
//! workspace action a person's own shortcut save uses.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::Path;

use serde_json::{Value, json};
use tilolang_tilo::engine::Provider as _;

/// A bot program as a registry lists it: a schedule that posts in a chat, in the canonical
/// spelling a program file has.
fn program() -> String {
    let shortcut: tilo_automation::Shortcut = serde_json::from_value(json!({
        "id": "echo",
        "name": "Echo",
        "steps": [{
            "id": "post",
            "step": "invoke",
            "action": {"app": "so.tilo.chat", "kind": "chat.workspace", "action": "post"},
            "resource": {"first_of_kind": "chat.workspace"},
            "args": {"text": {"literal": "welcome!"}}
        }],
        "triggers": [{"trigger": "schedule", "schedule": {"every": "interval", "seconds": 60}}]
    }))
    .expect("the fixture is a shortcut");
    String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).expect("it encodes"))
        .expect("a program is UTF-8")
}

/// Serve the daemon's native endpoint: answer each MCP call with the next of `answers`, keeping
/// what was asked.
fn daemon(socket: &Path, answers: Vec<Value>) -> std::thread::JoinHandle<Vec<Value>> {
    let listener = UnixListener::bind(socket).expect("the daemon's socket is free");
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the daemon is reached");
        let mut reader = BufReader::new(stream.try_clone().expect("the connection reads"));
        let mut writer = stream;
        let mut asked = Vec::new();
        for answer in answers {
            let mut line = String::new();
            reader.read_line(&mut line).expect("a request comes");
            let request: Value = serde_json::from_str(&line).expect("a request is MCP");
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            asked.push(request.get("params").cloned().unwrap_or(Value::Null));
            writeln!(
                writer,
                "{}",
                json!({"jsonrpc": "2.0", "id": id, "result": answer})
            )
            .expect("the answer goes");
        }
        asked
    })
}

/// Both tests point the daemon's native endpoint at a scratch runtime, so they take turns.
static RUNTIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Add `program` to the shortcut library the stand-in daemon keeps, answering what was asked.
fn add(program: String, saved: Value) -> Vec<Value> {
    let _runtime = RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let scratch = tempfile::tempdir().expect("a scratch directory");
    // SAFETY: this test's own process (nextest runs each test in one), before the daemon is
    // reached.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", scratch.path());
    }
    let sockets = scratch.path().join("tilo");
    std::fs::create_dir_all(&sockets).expect("the daemon's socket directory");
    let text = saved.to_string();
    let served = daemon(
        &sockets.join("native.sock"),
        vec![
            json!({"content": [{"type": "text", "text": text}], "structuredContent": saved}),
            json!({"structuredContent": {"id": "echo"}}),
        ],
    );
    let store = tilo_store_engine::Store;
    let answer = store
        .call("add-bot", &json!({"program": program}).to_string())
        .expect("the bot is added");
    assert!(answer.contains("Echo"), "{answer}");
    served.join().expect("the daemon's thread ends")
}

#[test]
fn a_directory_bots_program_is_saved_as_the_persons_shortcut() {
    let asked = add(program(), json!([]));
    assert_eq!(asked.len(), 2, "the shortcut is looked up, then saved");
    assert_eq!(asked[0]["name"], "tilo_do");
    assert_eq!(asked[0]["arguments"]["path"], "workspace/shortcuts#list");
    assert_eq!(asked[1]["arguments"]["path"], "workspace/shortcuts#create");
    let shortcut = &asked[1]["arguments"]["args"]["shortcut"];
    assert_eq!(shortcut["id"], "echo");
    assert_eq!(shortcut["name"], "Echo");
    assert_eq!(shortcut["triggers"][0]["trigger"], "schedule");
    assert!(
        shortcut.get("saved_by").is_none(),
        "the workspace sets whose shortcut it is"
    );
}

/// A newer program of a bot the workspace already runs replaces what runs: its id names the
/// shortcut, so the save is an update and no second copy stands beside it.
#[test]
fn a_program_the_person_already_saved_is_updated_in_place() {
    let asked = add(program(), json!([{"id": "echo", "name": "Echo"}]));
    assert_eq!(asked[1]["arguments"]["path"], "workspace/shortcuts#update");
    assert_eq!(asked[1]["arguments"]["args"]["shortcut"]["id"], "echo");
}
