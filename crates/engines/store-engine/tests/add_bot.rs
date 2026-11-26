//! Adding a directory bot's program saves it as the person's shortcut, through the same
//! workspace action a person's own shortcut save uses.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::Path;

use serde_json::{Value, json};
use tilo_test_env::ScopedEnv;
use tilolang_engine::Provider as _;

/// A bot program as a registry lists it: a schedule that posts in a chat, in the canonical
/// spelling a program file has.
fn program() -> Result<String, String> {
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
    .map_err(|error| format!("the fixture is no shortcut: {error}"))?;
    let bytes = tilo_automation::program::canonical_bytes(&shortcut)
        .map_err(|error| format!("the shortcut does not encode: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("a program is no UTF-8: {error}"))
}

/// The daemon's thread: the answers to the requests it was sent, or why it served none.
type Served = std::thread::JoinHandle<Result<Vec<Value>, String>>;

/// Serve the daemon's native endpoint: answer each MCP call with the next of `answers`, keeping
/// what was asked.
fn daemon(socket: &Path, answers: Vec<Value>) -> std::io::Result<Served> {
    let listener = UnixListener::bind(socket)?;
    Ok(std::thread::spawn(move || {
        let (stream, _) = listener
            .accept()
            .map_err(|error| format!("the daemon is not reached: {error}"))?;
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|error| format!("the connection does not read: {error}"))?,
        );
        let mut writer = stream;
        let mut asked = Vec::new();
        for answer in answers {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|error| format!("no request comes: {error}"))?;
            let request: Value = serde_json::from_str(&line)
                .map_err(|error| format!("a request is no MCP: {error}"))?;
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            asked.push(request.get("params").cloned().unwrap_or(Value::Null));
            writeln!(
                writer,
                "{}",
                json!({"jsonrpc": "2.0", "id": id, "result": answer})
            )
            .map_err(|error| format!("the answer does not go: {error}"))?;
        }
        Ok(asked)
    }))
}

/// Both tests point the daemon's native endpoint at a scratch runtime, so they take turns.
static RUNTIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Add `program` to the shortcut library the stand-in daemon keeps, answering what was asked and
/// what the store said of it.
fn add(program: &str, saved: &Value) -> Result<(String, Vec<Value>), String> {
    let _runtime = RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let scratch = tempfile::tempdir().map_err(|error| format!("a scratch directory: {error}"))?;
    let mut env = ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", scratch.path());
    let sockets = scratch.path().join("tilo");
    std::fs::create_dir_all(&sockets)
        .map_err(|error| format!("the daemon's socket directory: {error}"))?;
    let text = saved.to_string();
    let served = daemon(
        &sockets.join("native.sock"),
        vec![
            json!({"content": [{"type": "text", "text": text}], "structuredContent": saved}),
            json!({"structuredContent": {"id": "echo"}}),
        ],
    )
    .map_err(|error| format!("the daemon's socket: {error}"))?;
    let store = tilo_store_engine::Store;
    let answer = store.call("add-bot", &json!({"program": program}).to_string())?;
    let asked = served
        .join()
        .map_err(|_panicked| "the daemon's thread ends".to_owned())??;
    Ok((answer, asked))
}

#[test]
fn a_directory_bots_program_is_saved_as_the_persons_shortcut() {
    let program = program().unwrap();
    let (answer, asked) = add(&program, &json!([])).unwrap();
    assert!(answer.contains("Echo"), "{answer}");
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
    let program = program().unwrap();
    let (_, asked) = add(&program, &json!([{"id": "echo", "name": "Echo"}])).unwrap();
    assert_eq!(asked[1]["arguments"]["path"], "workspace/shortcuts#update");
    assert_eq!(asked[1]["arguments"]["args"]["shortcut"]["id"], "echo");
}
