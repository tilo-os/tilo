//! The session's agent door on a live headless compositor: a client that
//! says `initialize` is served nothing until the person answers through the
//! `permission-prompts` decision; refused, it reads a JSON-RPC error; admitted,
//! it is an agent member of the workspace and lists the session's live and
//! installed apps over the broker as that agent.
//!
//! Spawns a real headless compositor child, like the other headless shell
//! tests, and runs in the normal `it` target:
//! ```text
//! cargo test -p shell --test it agent_door_e2e::
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The restored Notes desktop (the one the idle-wakeup tests measure),
/// serving the agent door where a client in the login looks for it.
struct Session {
    desktop: crate::idle_wakeups::Desktop,
}

impl Session {
    fn control(&self) -> PathBuf {
        self.desktop.control_path().to_path_buf()
    }
    fn door(&self) -> PathBuf {
        self.desktop.runtime_dir().join("tilo-session/agent.sock")
    }
    fn log(&self) -> crate::idle_wakeups::Log {
        self.desktop.log_handle()
    }
    fn runtime_dir(&self) -> &Path {
        self.desktop.runtime_dir()
    }
    fn restart(&mut self) {
        self.desktop.restart();
        self.wait_for_door();
    }
    fn wait_for_door(&self) {
        self.desktop
            .wait_for("the restored Notes notebook", |targets| {
                targets.iter().any(|name| name == "notes.first_note")
            });
        let deadline = tilo_test_env::hang_deadline();
        while !self.door().exists() {
            assert!(Instant::now() < deadline, "the door was never served");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn spawn() -> Session {
    let desktop =
        crate::idle_wakeups::Desktop::start_with_door(crate::idle_wakeups::notes_desktop(), true);
    let session = Session { desktop };
    session.wait_for_door();
    session
}

/// Install a Viewer app in the session's own app store, as an install does:
/// its manifest, then the registry naming it, written whole.
fn install_viewer(session: &Session) {
    let store = session.runtime_dir().join("data/tilo-apps");
    let package = store.join("e2e/org.example.viewer");
    std::fs::create_dir_all(package.join("bin")).unwrap();
    let manifest = package.join("manifest.toml");
    std::fs::write(
        &manifest,
        "[package]\nid = \"org.example.viewer\"\nname = \"Viewer\"\nversion = \"1.0.0\"\n\
         kind = \"app\"\n[publisher]\nidentity = \"\"\nsignature = \"\"\n[sandbox]\n\
         namespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n[entrypoint]\nbin = \"bin/viewer\"\ntype = \"gui\"\n",
    )
    .unwrap();
    let registry = package_store::Layout::new(&store).registry();
    let staged = registry.with_extension("toml.tmp");
    std::fs::write(
        &staged,
        format!(
            "[[entry]]\nid = \"org.example.viewer\"\npayload_root = {:?}\nmanifest_path = {:?}\n",
            package.display().to_string(),
            manifest.display().to_string(),
        ),
    )
    .unwrap();
    std::fs::rename(&staged, &registry).unwrap();
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
    /// One tool call as request `id`: the text the agent reads, and whether
    /// it is an error.
    fn tool(&mut self, id: u64, name: &str, arguments: &serde_json::Value) -> (String, bool) {
        self.send(
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}),
        );
        let reply = self.reply().expect("the door answers");
        assert_eq!(reply["id"], id, "{reply}");
        (
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("no text in {reply}"))
                .to_owned(),
            reply["result"]["isError"] == true,
        )
    }
}

fn initialize() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"Claude Code","version":"2"}}})
}

#[test]
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
    assert!(names.contains(&"tilo_ls"), "{names:?}");
    assert!(
        !names.iter().any(|name| name.starts_with("computer")),
        "{names:?}"
    );
    // Admitted as an observer, with no grant of its own, it is told the
    // Notes that runs and every app installed, each one it may not open
    // saying so, never an empty list.
    install_viewer(&session);
    let deadline = tilo_test_env::hang_deadline();
    let mut id: u64 = 2;
    let listed = loop {
        id = id.checked_add(1).expect("request ids left");
        let (text, failed) = agent.tool(id, "tilo_ls", &serde_json::json!({}));
        assert!(!failed, "{text}");
        assert!(text.starts_with("apps"), "{text}");
        if text
            .lines()
            .any(|line| line.starts_with("notes  ") && line.ends_with("  running"))
            && text
                .lines()
                .any(|line| line.starts_with("viewer  Viewer  "))
        {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the running Notes and the installed Viewer were never listed: {text}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let installed: Vec<&str> = listed
        .lines()
        .filter(|line| line.contains("  installed"))
        .collect();
    assert!(
        installed
            .iter()
            .all(|line| line.ends_with("  installed, not yours to open")),
        "an observer was offered to open an app: {listed}"
    );
    assert!(
        listed.contains("the person decides which apps an agent may open"),
        "{listed}"
    );
    // Opening it is refused with who decides, not with "no such app".
    let (refused, failed) = agent.tool(
        id.checked_add(1).expect("request ids left"),
        "tilo_do",
        &serde_json::json!({"path": "viewer#open"}),
    );
    assert!(
        failed && refused.contains("this agent may not open viewer"),
        "{refused}"
    );

    // The admitted agent is an ordinary, revocable member of the workspace.
    let dump = control(&session.control(), &serde_json::json!({"op":"state_dump"}));
    assert!(
        dump.to_string().contains("agent-member:claude-code"),
        "the agent is not a member: {dump}"
    );
}

/// The real `tilo-runtime-mcp --connect`, started the way Claude Code starts
/// it inside tmux: in the person's login, but without `XDG_CURRENT_DESKTOP`.
struct McpClient {
    child: Child,
    replies: BufReader<std::process::ChildStdout>,
    next_id: u64,
    /// The compositor's log, for what a failed wait says about it.
    log: crate::idle_wakeups::Log,
    /// The compositor's control socket, for what its catalogue holds when a
    /// wait fails.
    control: PathBuf,
}

impl McpClient {
    fn start(session: &Session) -> Self {
        let mut child = Command::new(tilo_test_env::package_binary!("tilo-runtime-mcp"))
            .arg("--connect")
            .env("XDG_RUNTIME_DIR", session.runtime_dir())
            // Every directory the client could write is the session's own:
            // a run never touches the person's real profile.
            .env("HOME", session.runtime_dir().join("home"))
            .env("XDG_CONFIG_HOME", session.runtime_dir().join("config"))
            .env("XDG_DATA_HOME", session.runtime_dir().join("data"))
            .env("XDG_STATE_HOME", session.runtime_dir().join("state"))
            .env("XDG_CACHE_HOME", session.runtime_dir().join("cache"))
            .env_remove("XDG_CURRENT_DESKTOP")
            .env_remove("TILO_RUNTIME_PROFILE")
            .env_remove("TILO_RUNTIME_STANDALONE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn tilo-runtime-mcp");
        let replies = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            replies,
            next_id: 1,
            log: session.log(),
            control: session.control(),
        }
    }

    fn send(&mut self, request: &serde_json::Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
    }

    fn reply(&mut self) -> serde_json::Value {
        let mut line = String::new();
        assert!(
            self.replies.read_line(&mut line).unwrap() > 0,
            "the client closed"
        );
        serde_json::from_str(&line).unwrap()
    }

    /// One tool call: the text the agent reads, and whether it is an error.
    fn tool(&mut self, name: &str, arguments: &serde_json::Value) -> (String, bool) {
        self.next_id = self.next_id.checked_add(1).expect("request ids left");
        let id = self.next_id;
        self.send(
            &serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}),
        );
        let reply = self.reply();
        assert_eq!(reply["id"], id, "{reply}");
        (
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("no text in {reply}"))
                .to_owned(),
            reply["result"]["isError"] == true,
        )
    }

    /// Call `name` until it succeeds and `ready` accepts its text: an app
    /// publishes its directory a moment after it opens.
    fn tool_until(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        ready: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let (text, failed) = self.tool(name, arguments);
            if !failed && ready(&text) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "{name} {arguments} never became ready; last answer: {text}\n\
                 the compositor's providers: {}\ncompositor log:\n{}",
                self.providers(),
                crate::idle_wakeups::matching(
                    &self.log,
                    &[
                        "irectory",
                        "agent door",
                        "ERROR",
                        "overload",
                        "budget",
                        "ollab"
                    ],
                    40
                )
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Every provider the compositor's own catalogue offers, with its app and
    /// room: whether an app the agent cannot see published at all.
    fn providers(&self) -> String {
        let listed = control(
            &self.control,
            &serde_json::json!({"op":"actions_enumerate","query":"","limit":2000}),
        );
        let mut providers: Vec<String> = listed["answer"]["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                format!(
                    "{} app={} room={}",
                    row["provider"], row["app_id"], row["room"]
                )
            })
            .collect();
        providers.sort();
        providers.dedup();
        if providers.is_empty() {
            format!("none in {listed}")
        } else {
            providers.join("; ")
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An agent in a terminal with no `XDG_CURRENT_DESKTOP` is served by the
/// session's door, not a standalone runtime of its own, and reads and edits
/// Notes by the names a person uses, with no id in sight: ls, a new note, the
/// note found by its title under the notebook, a paragraph appended through
/// its body's own action, a read back; then the Notes grant is taken back
/// and the agent may no longer write, and its membership is taken back and
/// it may no longer read.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end agent session: ls, create, append and read back read in order"
)]
fn an_agent_without_the_desktop_name_edits_a_note_by_path() {
    let session = spawn();
    let mut agent = McpClient::start(&session);
    agent.send(&initialize());
    let prompt = waiting_prompt(&session.control());
    decide(&session.control(), "allow-once", &prompt);
    let initialized = agent.reply();
    assert!(
        initialized["result"]["serverInfo"].is_object(),
        "the door did not answer: {initialized}"
    );
    for profile in ["home/.local/state/tilo/runtime", "state/tilo/runtime"] {
        assert!(
            !session.runtime_dir().join(profile).exists(),
            "the client started a standalone runtime inside the login ({profile})"
        );
    }
    // The agent holds no role of its own: the person grants it Notes.
    let granted = control(
        &session.control(),
        &serde_json::json!({"op":"allow","subject":"agent:claude-code","node":"app:so.tilo.notes",
            "kinds":["read","undoable"],"answer":"allow","id":"e2e-agent-notes"}),
    );
    assert!(granted.get("err").is_none(), "{granted}");

    agent.send(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = agent.reply();
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    // The session's door has the room's content store, so it offers the
    // content puts an agent hands its bytes to an action through.
    assert_eq!(
        names,
        [
            "tilo_ls",
            "tilo_read",
            "tilo_do",
            "tilo_find",
            "tilo_status",
            "tilo_put_begin",
            "tilo_put_append",
            "tilo_put_finish"
        ]
    );

    // The restored notebook is an app of the session, by its short name.
    let apps = agent.tool_until("tilo_ls", &serde_json::json!({}), |text| {
        text.lines().any(|line| line.starts_with("notes  "))
    });
    assert!(apps.starts_with("apps\n"), "{apps}");
    let notes = agent.tool_until("tilo_ls", &serde_json::json!({"path": "notes"}), |text| {
        text.contains("#new-note") && text.lines().any(|line| line.starts_with("Notebook/"))
    });
    assert_no_opaque_id(&notes);

    // A new note, by the parameter its action page names.
    let params = agent
        .tool("tilo_ls", &serde_json::json!({"path": "notes#new-note"}))
        .0;
    assert!(
        params
            .lines()
            .any(|line| line.trim_start().starts_with("title")),
        "{params}"
    );
    let (created, failed) = agent.tool(
        "tilo_do",
        &serde_json::json!({"path": "notes#new-note", "args": {"title": "Agent todo"}}),
    );
    assert!(!failed && created.starts_with("ok"), "{created}");

    // Found by its title under the notebook, its body a resource of its own.
    let notebook = agent.tool_until(
        "tilo_ls",
        &serde_json::json!({"path": "notes/Notebook"}),
        |text| text.lines().any(|line| line.starts_with("Agent todo")),
    );
    assert_no_opaque_id(&notebook);
    let page = "notes/Notebook/Agent todo";
    let inside = agent.tool_until("tilo_ls", &serde_json::json!({"path": page}), |text| {
        text.lines()
            .any(|line| line.starts_with("Body") && line.contains("tilo.rich-text"))
    });
    assert_no_opaque_id(&inside);
    let body = format!("{page}/body");
    let actions = agent.tool("tilo_ls", &serde_json::json!({"path": body})).0;
    assert!(actions.contains("#append-paragraph"), "{actions}");
    assert_no_opaque_id(&actions);

    let (appended, failed) = agent.tool(
        "tilo_do",
        &serde_json::json!({"path": format!("{body}#append-paragraph"),
            "args": {"text": "buy milk"}}),
    );
    assert!(!failed && appended.starts_with("ok"), "{appended}");
    let read = agent.tool_until("tilo_read", &serde_json::json!({"path": body}), |text| {
        text.contains("buy milk")
    });
    assert_no_opaque_id(&read);
    let note = agent.tool_until("tilo_read", &serde_json::json!({"path": page}), |text| {
        text.contains("buy milk")
    });
    assert!(note.contains("Agent todo"), "{note}");

    // Taking the Notes grant back ends what it added, at once: the agent may
    // no longer write to the body.
    let revoked = control(
        &session.control(),
        &serde_json::json!({"op":"disallow","id":"e2e-agent-notes"}),
    );
    assert!(revoked.get("err").is_none(), "{revoked}");
    let (refused, failed) = agent.tool(
        "tilo_do",
        &serde_json::json!({"path": format!("{body}#append-paragraph"),
            "args": {"text": "buy eggs"}}),
    );
    assert!(
        failed,
        "an agent without a grant on Notes still wrote a note: {refused}"
    );
    // Reading the room is its membership's (the door admits an agent as an
    // Observer: AgentPreset grants it the room's reads), so the read stands
    // until the agent is removed, and then ends with the next call. Removed
    // the way the Agents tab's Remove does it, through the one action that
    // revokes every grant naming the agent.
    let (still, failed) = agent.tool("tilo_read", &serde_json::json!({"path": page}));
    assert!(
        !failed && still.contains("buy milk"),
        "an admitted Observer reads the room: {still}"
    );
    let removed = control(
        &session.control(),
        &serde_json::json!({"op":"actions_invoke","provider":"actions","id":"agent.remove",
            "args":{"name":"claude-code","confirm":true}}),
    );
    assert!(removed.get("err").is_none(), "{removed}");
    let (text, failed) = agent.tool("tilo_read", &serde_json::json!({"path": page}));
    assert!(
        failed && !text.contains("buy milk"),
        "an agent removed from the workspace still read a note: {text}"
    );

    // The compositor kept its state file's writer the whole time.
    let busy = crate::idle_wakeups::matching(&session.log(), &["file lease is busy"], 3);
    assert!(
        busy.is_empty(),
        "the state file's writer lease was taken:\n{busy}"
    );
}

/// What the agent reads names resources as a person does: never by a bound
/// UI id, a separator-joined handle or the app's own resource id.
fn assert_no_opaque_id(text: &str) {
    assert!(
        !text.contains("tilo-ui-bound")
            && !text.contains('\u{1f}')
            && !text.contains("notes.notebook/"),
        "an opaque id reached the agent: {text}"
    );
}

/// An agent admitted once by the person and granted Notes under `grant`.
fn admitted_agent(session: &Session, grant: &str) -> McpClient {
    let mut agent = McpClient::start(session);
    agent.send(&initialize());
    let prompt = waiting_prompt(&session.control());
    decide(&session.control(), "allow-once", &prompt);
    let initialized = agent.reply();
    assert!(
        initialized["result"]["serverInfo"].is_object(),
        "the door did not answer: {initialized}"
    );
    let granted = control(
        &session.control(),
        &serde_json::json!({"op":"allow","subject":"agent:claude-code","node":"app:so.tilo.notes",
            "kinds":["read","undoable"],"answer":"allow","id":grant}),
    );
    assert!(granted.get("err").is_none(), "{granted}");
    agent
}

impl McpClient {
    /// Run `path` until it succeeds: after a restart the app publishes its
    /// directory a moment after it opens. Its answer.
    fn do_until(&mut self, path: &str, args: &serde_json::Value) -> String {
        self.tool_until(
            "tilo_do",
            &serde_json::json!({"path": path, "args": args}),
            |text| text.starts_with("ok"),
        )
    }
}

/// The note a creating action answered with: its own id, which names it
/// for as long as it exists.
fn created_page(answer: &str) -> String {
    let result: serde_json::Value = answer
        .lines()
        .find(|line| line.starts_with('{'))
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("the creating action answered no result: {answer}"));
    let page = result["page"]
        .as_str()
        .unwrap_or_else(|| panic!("the result names no page: {answer}"));
    uuid::Uuid::parse_str(page).unwrap_or_else(|_| panic!("not a page id: {answer}"));
    page.to_owned()
}

/// An agent creates a note and is answered with the note's id; that id
/// keeps naming the note, and the actions on it keep working, after Notes
/// is closed and reopened and after the whole session restarts.
#[test]
fn the_id_a_creating_action_answers_keeps_working_across_app_and_shell_restarts() {
    let mut session = spawn();
    let mut agent = admitted_agent(&session, "e2e-agent-notes");
    agent.tool_until("tilo_ls", &serde_json::json!({"path": "notes"}), |text| {
        text.contains("#new-note")
    });
    let (created, failed) = agent.tool(
        "tilo_do",
        &serde_json::json!({"path": "notes#new-note", "args": {"title": "Kept note"}}),
    );
    assert!(!failed && created.starts_with("ok"), "{created}");
    let page = created_page(&created);
    let append = format!("notes/Notebook/{page}/body#append-paragraph");
    agent.do_until(&append, &serde_json::json!({"text": "before any restart"}));

    for close in ["tab.close", "tab.reopen"] {
        let answer = control(
            &session.control(),
            &serde_json::json!({"op":"actions_invoke","provider":"actions","id":close}),
        );
        assert!(answer.get("err").is_none(), "{close}: {answer}");
    }
    agent.do_until(
        &append,
        &serde_json::json!({"text": "after Notes reopened"}),
    );

    drop(agent);
    session.restart();
    let mut agent = admitted_agent(&session, "e2e-agent-notes-again");
    agent.do_until(
        &append,
        &serde_json::json!({"text": "after the session restarted"}),
    );
    let body = agent.tool_until(
        "tilo_read",
        &serde_json::json!({"path": format!("notes/Notebook/{page}/body")}),
        |text| text.contains("after the session restarted"),
    );
    for line in ["before any restart", "after Notes reopened"] {
        assert!(body.contains(line), "{line} is missing: {body}");
    }
}

/// An agent writes a thousand paragraphs into a note as fast as the door
/// takes them: every edit lands, collaboration stays enabled, and the next
/// action on the note still runs.
#[test]
fn a_burst_of_a_thousand_agent_edits_lands_and_keeps_the_action_channel() {
    const EDITS: usize = 1_024;
    const STEPS: usize = 32;
    let session = spawn();
    let mut agent = admitted_agent(&session, "e2e-agent-notes");
    agent.tool_until("tilo_ls", &serde_json::json!({"path": "notes"}), |text| {
        text.contains("#new-note")
    });
    let (created, failed) = agent.tool(
        "tilo_do",
        &serde_json::json!({"path": "notes#new-note", "args": {"title": "Burst"}}),
    );
    assert!(!failed && created.starts_with("ok"), "{created}");
    let page = created_page(&created);
    let body = format!("notes/Notebook/{page}/body");
    let append = format!("{body}#append-paragraph");
    agent.do_until(&append, &serde_json::json!({"text": "first"}));

    // Every request goes out before the first answer is read.
    let first_id = agent.next_id.checked_add(1).expect("request ids left");
    for batch in 0..EDITS / STEPS {
        agent.next_id = agent.next_id.checked_add(1).expect("request ids left");
        let calls: Vec<_> = (0..STEPS)
            .map(|step| {
                serde_json::json!({"path": append,
                    "args": {"text": format!("line {}", batch * STEPS + step)}})
            })
            .collect();
        agent.send(&serde_json::json!({"jsonrpc":"2.0","id":agent.next_id,
            "method":"tools/call","params":{"name":"tilo_do","arguments":{"calls":calls}}}));
    }
    for id in first_id..=agent.next_id {
        let reply = agent.reply();
        assert_eq!(reply["id"], id, "{reply}");
        assert!(
            reply["result"]["isError"] != true,
            "a step of the burst failed: {reply}"
        );
    }

    agent.do_until(&append, &serde_json::json!({"text": "after the burst"}));
    let read = agent.tool_until(
        "tilo_read",
        &serde_json::json!({"path": body, "max_chars": 100_000}),
        |text| text.contains("after the burst"),
    );
    for line in 0..EDITS {
        assert!(
            read.contains(&format!("line {line} ")) || read.contains(&format!("line {line}\n")),
            "line {line} is missing"
        );
    }
    let disabled = crate::idle_wakeups::matching(
        &session.log(),
        &["disabling collaboration", "invocation stream closed"],
        5,
    );
    assert!(
        disabled.is_empty(),
        "the burst ended collaboration:\n{disabled}"
    );
}
