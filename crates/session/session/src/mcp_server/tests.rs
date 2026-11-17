use super::*;

/// An agent's `open` and `place` naming nothing to stand beside stand beside the conversation
/// of its turn; one that names its own anchor, and every other action, keep their arguments.
#[test]
fn a_turns_open_and_place_stand_beside_its_conversation() {
    let conversation = "chat.workspace/channels/c1";
    let address = |action: &str| {
        tilo_actions::ActionAddress::new(tilo_session_core::layout::RESOURCE, action)
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
fn the_api_is_the_devices_own_unless_a_developer_named_one() {
    let device = path_in(None, Some("/run/user/1000".into()));
    assert_eq!(
        device,
        Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock")),
        "this device's API"
    );
    let named = path_in(
        Some("/run/an-mcp-server".into()),
        Some("/run/user/1000".into()),
    );
    assert_eq!(
        named,
        Some(PathBuf::from("/run/an-mcp-server")),
        "a developer's own API"
    );
    assert_eq!(
        path_in(Some("relative".into()), Some("/run/user/1000".into())),
        device,
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

/// An agent works in one workspace: the shell's windows of another, and windows the shell no
/// longer knows, are not its; the windows of its own are, and so is anything not a window.
#[test]
fn a_window_of_another_workspace_is_not_an_agents() {
    let services = SessionServices::new(Arc::new(|| {}), "person");
    services
        .tabs
        .lock_recover()
        .extend([(7, "studio".to_owned()), (9, "garden".to_owned())]);
    assert!(
        !services.window_elsewhere("desktop:tab:7", "studio"),
        "its own"
    );
    assert!(
        services.window_elsewhere("desktop:tab:9", "studio"),
        "another's"
    );
    assert!(
        services.window_elsewhere("desktop:tab:11", "studio"),
        "one no longer on the shell"
    );
    assert!(
        !services.window_elsewhere("notes.notebook", "studio"),
        "a resource that is no window"
    );
}

/// A workspace app's record is read without waiting on the session's pass, which the loop
/// answers only between frames: only the apps the session's services keep are asked there.
#[test]
fn a_workspace_apps_record_is_read_without_the_sessions_pass() {
    use tilo_actions::remote::{ResourceAddress, ResourceReads as _};
    // A session that never answers, and no broker: a read asking the session waits it out.
    let records = ServiceRecords {
        state: None,
        session: Arc::new(SessionServices::new(Arc::new(|| {}), "person")),
        agent_session: None,
    };
    let started = std::time::Instant::now();
    let read = records.read(
        &Caller::local_person("person"),
        &ResourceAddress {
            app: "so.tilo.chat".to_owned(),
            record: String::new(),
        },
    );
    assert!(
        matches!(&read, Err(ActionCallError::Unavailable { reason })
            if reason == "this desktop keeps no records of its own"),
        "answered from the broker, which this shell has none of: {read:?}"
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

/// A service publishing the one style the person may switch to, as the device's does.
struct Styles {
    offered: serde_json::Value,
}

impl tilo_service::Service for Styles {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == crate::device_lists::STYLES_RESOURCE).then(|| self.offered.clone())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        Err(tilo_actions::ApiError::Unknown(
            tilo_actions::ActionAddress::new(resource, action),
        ))
    }
}

/// A program's read of a resource the session publishes (the device's styles) is answered from
/// the session's own services on the loop's pass, where they are: one `tilo` call reads
/// `device.styles.offered` with no record and no app, as the shell's own parts read it.
#[test]
fn a_program_reads_the_reading_a_session_service_publishes() {
    let offered = serde_json::json!({
        "offered": [{"style": "so.tilo.shell:strips", "package": "so.tilo.shell",
                     "name": "strips", "line": "The shell's strips", "loads": true, "reason": ""}],
    });
    let mut services = tilo_service::Services::default();
    services.add(Box::new(Styles {
        offered: offered.clone(),
    }));
    let app = tilolang_tilo::platform_apps::DEVICE_APP;
    assert_eq!(
        published_reading(&services, app, crate::device_lists::STYLES_RESOURCE),
        Some(offered),
        "the reading its service publishes"
    );
    // A device resource no service of this device serves is published empty, as the shell's
    // parts are handed it: a program reads its fields as absent and goes on.
    assert_eq!(
        published_reading(&services, app, "power"),
        Some(serde_json::json!({})),
        "a resource no service serves is published empty"
    );
    // The session's own roots (`device.windows`) are published as its own app's.
    assert_eq!(
        published_reading(
            &services,
            tilolang_tilo::platform_apps::DEVICE_APP,
            "session:windows",
        ),
        Some(serde_json::json!({})),
        "a resource of the session's own, read as it stands here"
    );
}

/// A resource the session does not publish itself is no reading of its, whatever app names it:
/// an app's record is read through the record API, never from a service.
#[test]
fn what_the_session_does_not_publish_is_no_reading_of_its() {
    let mut services = tilo_service::Services::default();
    services.add(Box::new(Styles {
        offered: serde_json::json!({"offered": []}),
    }));
    // An app whose records are read, by the id its own record's resource has, is no reading the
    // session publishes: none of `platform_roots` names it.
    assert_eq!(
        published_reading(&services, "so.tilo.notes", "notes.notebook"),
        None,
        "no reading of an app's record"
    );
    // Nor is a session resource asked of an app that does not publish it.
    assert_eq!(
        published_reading(
            &services,
            tilolang_tilo::platform_apps::SETTINGS_APP,
            crate::device_lists::STYLES_RESOURCE,
        ),
        None,
        "the device publishes the styles, not Settings"
    );
}

/// A program's reading of a resource the session publishes is decided for its caller, as every
/// read is: the person reads their own device, and an agent reads it only where the broker lets
/// it read the resource. With no broker to decide it, an agent reads none of it.
#[test]
fn a_reading_is_the_callers_only_where_the_read_is_theirs() {
    use tilo_actions::remote::Readings as _;
    let session = Arc::new(SessionServices::new(Arc::new(|| {}), "person"));
    let readings = SessionReadings {
        session: Arc::clone(&session),
        state: None,
    };
    let offered = serde_json::json!({"offered": []});
    // The loop's pass, answering the reading asked of it with the styles, published under the
    // kind `kind` (`None`: under none).
    let answer_once = |session: &SessionServices, kind: Option<&str>| {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let asked = std::mem::take(&mut *session.asks.lock_recover());
            if let Some(ApiAsk::Reading { done, .. }) = asked.into_iter().next() {
                done.complete(Some(Published {
                    kind: kind.map(str::to_owned),
                    reading: offered.clone(),
                }));
                return;
            }
            assert!(std::time::Instant::now() < deadline, "no reading was asked");
            std::thread::yield_now();
        }
    };
    let app = tilolang_tilo::platform_apps::DEVICE_APP;
    let resource = crate::device_lists::STYLES_RESOURCE;
    let read_as = |caller: &Caller, kind: Option<&str>| {
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| readings.reading(caller, app, resource));
            answer_once(&session, kind);
            reader.join().expect("the reader")
        })
    };
    // A reading published under no kind has no node to decide it at: nobody reads it, the
    // person included, rather than anyone reading it unchecked.
    for caller in [
        Caller::local_person("person"),
        Caller::local_agent("helper").in_workspace("studio"),
    ] {
        assert_eq!(read_as(&caller, None), Ok(None), "no kind, no reading");
    }
    for (caller, theirs) in [
        (Caller::local_person("person"), true),
        (Caller::local_agent("helper").in_workspace("studio"), false),
    ] {
        let read = read_as(&caller, Some("session.styles"));
        if theirs {
            assert_eq!(read, Ok(Some(offered.clone())), "the person's own device");
        } else {
            assert!(
                matches!(
                    read,
                    Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
                ),
                "an agent no broker lets read it reads none of it: {read:?}"
            );
        }
    }
}
