//! An agent on a computer of the person's (`tilo login`, then `tilo mcp`):
//! the runtime's MCP projection of the workspace, served on that computer,
//! its actions asked of the workspace's host through the computer's own
//! session as one of the person's devices.
use super::*;
use serde_json::{Value, json};
use tilo_browser_peer::session_actions::SessionActions;
use tilo_runtime::mcp::{McpServer, Surface};

/// The presence `tilo mcp` says of its agent, as the CLI itself says it: the
/// same source, so what this test's other device hears is what a person's
/// desktop hears.
#[path = "../../../../system/tilo-cli/src/mcp_presence.rs"]
mod mcp_presence;

/// What the projection answers `request`.
async fn ask(server: &McpServer<SessionActions>, request: Value) -> Value {
    server.handle(request).await.expect("a request is answered")
}

/// The text a tool call answers, and whether it failed.
async fn tool(server: &McpServer<SessionActions>, name: &str, arguments: Value) -> (String, bool) {
    let answer = ask(
        server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}}),
    )
    .await;
    let result = &answer["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (
        if text.is_empty() {
            answer.to_string()
        } else {
            text
        },
        result["isError"] == true || answer.get("error").is_some(),
    )
}

/// The presence frames the page heard since it last looked, in order,
/// taking everything else it received as its own pump does.
fn heard(browser: &mut Browser) -> Vec<collab_types::presence::Presence> {
    let mut frames = Vec::new();
    for event in browser.events.drain() {
        if let tilo_browser_peer::PeerEvent::LivePlane {
            workspace_id,
            track: tilo_browser_peer::Track::Presence,
            body,
            ..
        } = &event
            && *workspace_id == browser.workspace
            && let Some(frame) = collab_types::presence::Presence::decode(body)
        {
            frames.push(frame);
        }
        let _ = browser.take(event);
    }
    frames
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_computer_lists_the_workspace_and_writes_what_another_device_sees() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    // The agent's computer enters the workspace as a device of the person's,
    // as `tilo login` does, and serves the projection over its session.
    let agent = Browser::open(&hosted, &computer, &host).await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent)
    .with_agent_run("run-1", "Claude Code");

    // The agent shows itself present in the workspace from the moment it
    // enters it, as `tilo mcp` does.
    let presence = mcp_presence::Presence::new(
        &agent.peer,
        WORKSPACE.into(),
        agent.workspace,
        "Claude Code".into(),
    );
    presence.arrive();

    let listed = ask(
        &server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for wanted in ["tilo_ls", "tilo_read", "tilo_do", "tilo_find"] {
        assert!(names.contains(&wanted), "{names:?}");
    }

    // The workspace's apps, a record the other device made among them as a
    // running app, and what one of its actions takes.
    let (apps, failed) = tool(&server, "tilo_ls", json!({})).await;
    assert!(!failed, "{apps}");
    assert!(
        apps.lines()
            .any(|line| line.starts_with("sheets ") && line.ends_with("running")),
        "{apps}"
    );
    let (app, failed) = tool(&server, "tilo_ls", json!({"path": "sheets"})).await;
    assert!(!failed && app.contains("Workbook"), "{app}");
    let (action, failed) = tool(&server, "tilo_ls", json!({"path": "sheets#new-sheet"})).await;
    assert!(!failed && action.contains("name?"), "{action}");

    let new_sheet = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                           "params": {"name": "tilo_do",
                                      "arguments": {"path": "sheets#new-sheet",
                                                    "args": {"name": "Budget"}}}});
    let ran = ask(&server, new_sheet.clone()).await;
    assert_eq!(ran["result"]["isError"], false, "{ran}");
    // The call names the record it worked in; the agent says that is where
    // it acts.
    let acted = server
        .acted()
        .expect("the call names the record it worked in");
    assert_eq!(acted.app, SHEETS);
    assert_eq!(acted.record, sheets.record);
    presence.called(&new_sheet, false, Some(acted));

    // The other device hears the agent present — as an agent, saying what it
    // does and in which record (the frame a desktop turns into a member of
    // kind agent, `tilo_live::presence::shown`) — and sees its change, made
    // by the host as the workspace's own, whose History says it was the
    // agent's run.
    let deadline = tilo_test_env::hang_deadline();
    let mut view: Option<collab_types::presence::ShellPresence> = None;
    let mut edited = false;
    loop {
        for frame in heard(&mut browser) {
            if let collab_types::PeerPresencePayload::Shell(shell) = frame.payload
                && shell.agent
            {
                view = Some(*shell);
            }
        }
        if !edited {
            let app = browser.mount(&sheets);
            if sheet_names(&app).iter().any(|name| name == "Budget") {
                let runs: Vec<tilo_actions::WorkGroup> = app
                    .read(|doc| {
                        doc.clone()
                            .get_changes(&[])
                            .iter()
                            .filter_map(|change| change.message())
                            .filter_map(|message| {
                                tilo_actions::ActionContext::from_change_message(message)
                            })
                            .filter_map(|context| context.group)
                            .collect()
                    })
                    .unwrap();
                assert!(
                    runs.iter()
                        .any(|run| run.id == "run-1" && run.label == "Claude Code"),
                    "the change names the agent's run: {runs:?}"
                );
                edited = true;
            }
        }
        let said = view
            .as_ref()
            .is_some_and(|view| view.activity.as_deref() == Some("Adding sheets"));
        if edited && said {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent's sheet and what it says it is doing never reached the browser"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The agent is a member of the workspace as an agent, by its own name,
    // doing that in the workbook it edited: following it brings the person
    // to that record.
    let view = view.expect("another device hears the agent");
    assert!(view.agent);
    assert_eq!(view.label, "Claude Code");
    assert_eq!(view.activity.as_deref(), Some("Adding sheets"));
    let edited_record = tilo_relay_protocol::live::LiveRecord {
        app: SHEETS.into(),
        record: sheets.record.clone(),
    };
    assert_eq!(view.focus(), Some((SHEETS, edited_record.short_digest())));

    // And it leaves with the connection, rather than standing there.
    presence.leave();
    loop {
        let left = heard(&mut browser)
            .into_iter()
            .any(|frame| frame.payload == collab_types::PeerPresencePayload::Left);
        if left {
            break;
        }
        assert!(Instant::now() < deadline, "the agent never left");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
