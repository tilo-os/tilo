//! The session's agent door on a live headless compositor: a client that
//! says `initialize` is served nothing until the person answers through the
//! `permission-prompts` decision; refused, it reads a JSON-RPC error; admitted,
//! it is an agent member of the workspace and lists the session's live apps
//! over the broker as that agent.
//!
//! `#[ignore]`d by default (spawns a real compositor child). Opt in with:
//! ```text
//! cargo test -p shell --test it agent_door_e2e:: -- --ignored --test-threads 1
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Session {
    child: Child,
    runtime_dir: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

impl Session {
    fn control(&self) -> PathBuf {
        self.runtime_dir.join("ctl")
    }
    fn door(&self) -> PathBuf {
        self.runtime_dir.join("agent.sock")
    }
}

fn spawn() -> Session {
    tilo_test_env::require_isolated_desktop();
    // Short, so every socket path stays below SUN_LEN.
    let runtime_dir = PathBuf::from(format!(
        "/run/user/{}/tilo-door-{}",
        // SAFETY: `getuid` has no failure mode.
        unsafe { libc::getuid() },
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_shell"))
        .args(["--mode", "native", "--backend", "headless"])
        .env("TILO_TEST_CONTROL", runtime_dir.join("ctl"))
        .env("TILO_AGENT_DOOR", runtime_dir.join("agent.sock"))
        .env("TILO_HEADLESS_SIZE", "1280x800")
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HOME", runtime_dir.join("home"))
        .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
        .env("XDG_DATA_HOME", runtime_dir.join("data"))
        .env("XDG_STATE_HOME", runtime_dir.join("state"))
        .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
        .env("TILO_IDENTITY_ROOT", runtime_dir.join("identity"))
        .env("TILO_STATE_PATH", runtime_dir.join("state.json"))
        .env("TILO_WORKSPACE_ROOTS", "")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn compositor");
    let mut session = Session { child, runtime_dir };
    let deadline = tilo_test_env::hang_deadline();
    while UnixStream::connect(session.control()).is_err() || !session.door().exists() {
        assert!(
            session.child.try_wait().unwrap().is_none(),
            "the compositor exited before serving its door"
        );
        assert!(Instant::now() < deadline, "the door was never served");
        std::thread::sleep(Duration::from_millis(100));
    }
    session
}

fn control(socket: &Path, request: &serde_json::Value) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

/// The waiting agent admission's prompt id, as the decision rows name it.
fn waiting_prompt(socket: &Path) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let listed = control(
            socket,
            &serde_json::json!({"op":"actions_enumerate","query":"permission-prompts","limit":2000}),
        );
        let found = listed["answer"]["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["provider"] == "permission-prompts")
            .find_map(|row| row["id"].as_str()?.strip_prefix("deny@").map(str::to_owned));
        if let Some(found) = found {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "the person was never asked: {listed}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn decide(socket: &Path, decision: &str, prompt: &str) {
    let answer = control(
        socket,
        &serde_json::json!({"op":"actions_invoke","provider":"permission-prompts",
            "id":format!("{decision}@{prompt}"),"args":{"confirm":true}}),
    );
    assert!(answer.get("err").is_none(), "{answer}");
}

struct Client {
    stream: UnixStream,
    replies: BufReader<UnixStream>,
}

impl Client {
    fn connect(door: &Path) -> Self {
        let stream = UnixStream::connect(door).unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let replies = BufReader::new(stream.try_clone().unwrap());
        Self { stream, replies }
    }
    fn send(&mut self, request: &serde_json::Value) {
        writeln!(self.stream, "{request}").unwrap();
    }
    fn reply(&mut self) -> Option<serde_json::Value> {
        let mut line = String::new();
        (self.replies.read_line(&mut line).unwrap() > 0)
            .then(|| serde_json::from_str(&line).unwrap())
    }
}

fn initialize() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"Claude Code","version":"2"}}})
}

#[test]
#[ignore = "spawns a real headless compositor; opt in with --ignored"]
fn a_terminal_agent_is_admitted_or_refused_by_the_person_and_lists_live_apps() {
    let session = spawn();

    // Refused: a JSON-RPC error on its initialize, then the door closes.
    let mut refused = Client::connect(&session.door());
    refused.send(&initialize());
    let prompt = waiting_prompt(&session.control());
    decide(&session.control(), "deny", &prompt);
    let error = refused.reply().expect("a refusal");
    assert_eq!(error["id"], 1, "{error}");
    assert!(error["error"]["message"].is_string(), "{error}");
    assert!(refused.reply().is_none(), "the door closed");

    // Admitted once: served as the agent, with the session's live apps.
    let mut agent = Client::connect(&session.door());
    agent.send(&initialize());
    let prompt = waiting_prompt(&session.control());
    decide(&session.control(), "allow-once", &prompt);
    let initialized = agent.reply().expect("initialize answered");
    assert!(
        initialized["result"]["serverInfo"].is_object(),
        "{initialized}"
    );
    agent.send(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = agent.reply().unwrap();
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"tilo_list_apps"), "{names:?}");
    assert!(
        !names.iter().any(|name| name.starts_with("computer")),
        "{names:?}"
    );
    agent.send(
        &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"tilo_list_apps","arguments":{}}}),
    );
    let listed = agent.reply().unwrap();
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    assert!(
        listed["result"]["structuredContent"]["apps"].is_array(),
        "{listed}"
    );

    // The admitted agent is an ordinary, revocable member of the workspace.
    let dump = control(&session.control(), &serde_json::json!({"op":"state_dump"}));
    assert!(
        dump.to_string().contains("agent-member:claude-code"),
        "the agent is not a member: {dump}"
    );
}
