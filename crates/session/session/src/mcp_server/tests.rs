use super::*;

/// An agent's `open` and `place` naming nothing to stand beside stand beside the conversation
/// of its turn; one that names its own anchor, and every other action, keep their arguments.
#[test]
fn a_turns_open_and_place_stand_beside_its_conversation() {
    let conversation = "chat.workspace/channels/c1";
    let address = |action: &str| {
        tilo_actions::ActionAddress::new(tilo_session_core::arrangement::RESOURCE, action)
    };
    let resource = serde_json::json!({"resource": "tl:so.tilo.lang.tasks:root-t"});
    let resource = resource.as_object().expect("an object");
    for action in ["open", "place"] {
        let filled = with_turn_beside(&address(action), resource, conversation);
        assert_eq!(filled["beside"], conversation, "{action}");
    }
    let own = serde_json::json!({"resource": "r", "beside": "shell:place:3"});
    let own = own.as_object().expect("an object");
    assert_eq!(
        with_turn_beside(&address("place"), own, conversation)["beside"],
        "shell:place:3"
    );
    assert!(
        !with_turn_beside(&address("place-found"), resource, conversation).contains_key("beside")
    );
}

/// What the person remembers is the program, and every connection it admits is an instance of
/// its own: revoking the one takes back the answer of the other, and no other program's.
#[test]
fn an_answer_remembered_for_a_program_covers_each_instance_of_it() {
    assert!(same_program("claude-code", "claude-code@host#1"));
    assert!(same_program("claude-code@host", "claude-code@host#2"));
    assert!(same_program("claude-code", "claude-code@laptop-3f2a#1"));
    assert!(same_program("claude-code@host#1", "claude-code@host#1"));
    assert!(!same_program("claude-code", "codex@host#2"));
    assert!(
        !same_program("claude-code", "claude-codex@host#1"),
        "an id that only begins with the same letters is another program"
    );
}

#[test]
fn the_door_is_the_seats_own_unless_a_developer_named_one() {
    let seat = path_in(None, Some("/run/user/1000".into()));
    assert_eq!(
        seat,
        Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock")),
        "the seat's door"
    );
    let named = path_in(
        Some("/run/an-mcp-server".into()),
        Some("/run/user/1000".into()),
    );
    assert_eq!(
        named,
        Some(PathBuf::from("/run/an-mcp-server")),
        "a developer's own door"
    );
    assert_eq!(
        path_in(Some("relative".into()), Some("/run/user/1000".into())),
        seat,
        "only an absolute path is a developer's"
    );
    assert_eq!(path_in(None, None), None, "nowhere to listen");
}

#[test]
fn a_client_is_proposed_as_a_canonical_agent_id() {
    assert_eq!(proposed_agent_id("Claude Code"), "claude-code");
    assert_eq!(proposed_agent_id("  Claude   Code 2.1 "), "claude-code-2-1");
    assert_eq!(proposed_agent_id("!!!"), "agent");
    assert_eq!(proposed_agent_id("MCP client"), "mcp-client");
}

/// The login's shell runtime is the image's executable in its own unit of the session's
/// service manager; the same executable run anywhere else, or another program in that unit,
/// is not it.
#[test]
fn the_shell_runtime_is_its_executable_in_its_unit_of_this_login() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = dir.path().join("tilo-ush");
    std::fs::write(&runtime, b"runtime").unwrap();
    let other = dir.path().join("other");
    std::fs::write(&other, b"other").unwrap();
    let exe = runtime.to_str().unwrap();
    // As a booted box has them: the session in its login's scope, the runtime in its unit.
    let unit = "0::/user.slice/user-1001.slice/user@1001.service/app.slice/tilo-ush.service\n";
    assert!(shell_runtime_at(&runtime, exe, unit, 1001));
    let below =
        "0::/user.slice/user-1001.slice/user@1001.service/app.slice/tilo-ush.service/run-1\n";
    assert!(
        shell_runtime_at(&runtime, exe, below, 1001),
        "a group of the unit's own"
    );
    assert!(
        !shell_runtime_at(&runtime, other.to_str().unwrap(), unit, 1001),
        "another program in the unit"
    );
    let app =
        "0::/user.slice/user-1001.slice/user@1001.service/app.slice/app-so.tilo.terminal.scope\n";
    assert!(
        !shell_runtime_at(&runtime, exe, app, 1001),
        "the executable run by an app"
    );
    let session = "0::/user.slice/user-1001.slice/session-81.scope\n";
    assert!(
        !shell_runtime_at(&runtime, exe, session, 1001),
        "the executable run in the login's scope"
    );
    assert!(
        !shell_runtime_at(&runtime, exe, unit, 1002),
        "another account's runtime"
    );
    let named = "0::/user.slice/user-1001.slice/session-81.scope/tilo-ush.service\n";
    assert!(
        !shell_runtime_at(&runtime, exe, named, 1001),
        "a group named like the unit elsewhere"
    );
    assert!(
        !shell_runtime_at(&runtime, "/nonexistent", unit, 1001),
        "a process that is gone"
    );
    assert!(
        !shell_runtime_at(&runtime, exe, "", 1001),
        "no cgroup to read"
    );
}

/// An agent works in one workspace: the desk's windows of another, and windows the desk no
/// longer knows, are not its; the windows of its own are, and so is anything not a window.
#[test]
fn a_window_of_another_workspace_is_not_an_agents() {
    let doors = SessionDoors::new(Arc::new(|| {}), "person");
    doors
        .tabs
        .lock_recover()
        .extend([(7, "studio".to_owned()), (9, "garden".to_owned())]);
    assert!(
        !doors.window_elsewhere("desktop:tab:7", "studio"),
        "its own"
    );
    assert!(
        doors.window_elsewhere("desktop:tab:9", "studio"),
        "another's"
    );
    assert!(
        doors.window_elsewhere("desktop:tab:11", "studio"),
        "one no longer on the desk"
    );
    assert!(
        !doors.window_elsewhere("notes.notebook", "studio"),
        "a resource that is no window"
    );
}

/// A workspace app's record is read without waiting on the session's pass, which the loop
/// answers only between frames: only the apps the session's services keep are asked there.
#[test]
fn a_workspace_apps_record_is_read_without_the_sessions_pass() {
    use tilo_actions::remote::{RecordAddress, RecordReads as _};
    // A session that never answers, and no broker: a read asking the session waits it out.
    let records = ServiceRecords {
        state: None,
        session: Arc::new(SessionDoors::new(Arc::new(|| {}), "person")),
        run: None,
        agent_session: None,
    };
    let started = std::time::Instant::now();
    let read = records.read(
        &Caller::local_person("person"),
        &RecordAddress {
            app: "so.tilo.chat".to_owned(),
            record: String::new(),
        },
    );
    assert!(
        matches!(&read, Err(ActionHostError::Unavailable { reason })
            if reason == "this desktop keeps no records of its own"),
        "answered from the broker, which this desk has none of: {read:?}"
    );
    assert!(started.elapsed() < WAIT, "nothing waited on the session");
    assert!(records.session.asks.lock_recover().is_empty());
}

#[test]
fn the_peer_must_be_this_accounts_own_process_in_its_namespaces() {
    let own = rustix::process::geteuid().as_raw();
    let shares = |uid, namespaces| {
        peer_decision(&Peer {
            uid,
            shares_namespaces: namespaces,
        })
    };
    assert_eq!(shares(own, true), Ok(()));
    assert_eq!(
        shares(own.saturating_add(1), true),
        Err(PeerRefusal::OtherAccount),
        "another account"
    );
    assert_eq!(
        shares(own, false),
        Err(PeerRefusal::Sandboxed),
        "a program inside a sandbox"
    );
}
