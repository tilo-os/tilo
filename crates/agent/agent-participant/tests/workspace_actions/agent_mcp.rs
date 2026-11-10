//! An agent on a computer of the person's (`tilo login`, then `tilo mcp`):
//! the runtime's MCP projection of the workspace, served on that computer,
//! its actions asked of the workspace's host through the computer's own
//! session as one of the person's devices.
use super::*;
use serde_json::{Value, json};
use tilo_relay_protocol::live::{LiveFrame, LiveRecord, OpsFrame};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_runtime::mcp_presence;
use tilo_session_host::session_actions::SessionActions;

/// The app the agent writes in.
const WRITER: &str = "so.tilo.word-processor";

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

/// Semantic work a member narrated on a record's live stream: who published
/// it, the record, and what was done.
type Narrated = ([u8; 32], LiveRecord, tilo_actions::SemanticInteraction);

/// What the page heard since it last looked, in order: the presence frames,
/// and the semantic work members narrated on records' live streams, with
/// who published it. Everything else it received it takes as its own pump
/// does.
fn heard(browser: &mut Browser) -> (Vec<collab_types::presence::Presence>, Vec<Narrated>) {
    let mut frames = Vec::new();
    let mut narrated = Vec::new();
    for event in browser.events.drain() {
        if let tilo_session_host::PeerEvent::Live { from, frame, .. } = &event
            && let Ok(LiveFrame::Ops(OpsFrame::Semantic { record, event })) =
                LiveFrame::decode(frame)
            && let Ok(interaction) = serde_json::from_str(&event.interaction_json)
        {
            narrated.push((*from, record, interaction));
        }
        if let tilo_session_host::PeerEvent::LivePlane {
            workspace_id,
            track: tilo_live::live::Track::Presence,
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
    (frames, narrated)
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
    let desk = put_desk(&mut browser);
    let writer = browser.create(WRITER);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    // The agent's computer enters the workspace as a device of the person's,
    // as `tilo login` does, and serves the projection over its session.
    let agent = Browser::open(&hosted, &computer, &host).await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    // The agent shows itself present in the workspace from the moment it
    // enters it, as `tilo mcp` does, and each action it takes as it goes.
    let presence = mcp_presence::Presence::new(
        (&agent.peer, &computer.cert),
        actions.clone(),
        person.clone(),
        (WORKSPACE.into(), agent.workspace),
        "Claude Code".into(),
    )
    .expect("the agent's certificate names a device");
    presence.arrive();
    let narrating = std::sync::Arc::clone(&presence);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_surface(Surface::Agent)
        .with_agent_run("run-1", "Claude Code")
        .with_narration(std::sync::Arc::new(move |acting| {
            narrating.acting(&acting);
        }));

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
    presence.called(&new_sheet, false, Some(acted)).await;
    // And it writes in the document the other device made.
    let (wrote, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "word-processor#append-paragraph", "args": {"text": "Hello"}}),
    )
    .await;
    assert!(!failed, "{wrote}");

    // The other device hears the agent present — as an agent, saying what it
    // does and in which record (the frame a desktop turns into a member of
    // kind agent, `tilo_live::presence::shown`) — and sees its change, made
    // by the host as the workspace's own, whose History says it was the
    // agent's run.
    let deadline = tilo_test_env::hang_deadline();
    let mut view: Option<collab_types::presence::ShellPresence> = None;
    let mut selected: Option<collab_types::presence::ShellSelection> = None;
    let mut phases = Vec::new();
    let mut edited = false;
    let mut shown = false;
    loop {
        let (frames, narrated) = heard(&mut browser);
        for frame in frames {
            if let collab_types::PeerPresencePayload::Shell(shell) = frame.payload
                && shell.agent
            {
                if shell.selection.is_some() {
                    selected.clone_from(&shell.selection);
                }
                view = Some(*shell);
            }
        }
        // The agent's action, narrated on the workbook's stream by the
        // agent's own device as it went.
        for (from, record, interaction) in narrated {
            assert_eq!(from, computer.cert.device_public_key);
            interaction.validate().unwrap();
            let on = (record.app.as_str(), record.record.as_str());
            match interaction.action.id.as_str() {
                "new-sheet" => {
                    assert_eq!(on, (SHEETS, sheets.record.as_str()));
                    phases.push(interaction.phase);
                }
                "append-paragraph" => assert_eq!(on, (WRITER, writer.record.as_str())),
                other => panic!("narrated an action it did not take: {other}"),
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
        // The workbook it works in is open on the workspace's desk, as a
        // window of the person's, for every page to draw.
        shown = shown || desk_windows(&mut browser, &desk, &sheets.record) > 0;
        let said = view
            .as_ref()
            .is_some_and(|view| view.activity.as_deref() == Some("Adding sheets"));
        let committed = phases.contains(&tilo_actions::InteractionPhase::Committed);
        if edited && said && shown && committed && selected.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent's sheet ({edited}), its window ({shown}), what it says it is doing \
             ({said}), its narrated action ({phases:?}) or where it left off ({selected:?}) \
             never all reached the browser"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // It was seen finding the action, applying it and done, in that order.
    assert_eq!(
        phases.first(),
        Some(&tilo_actions::InteractionPhase::Locating),
        "{phases:?}"
    );
    assert!(
        phases
            .windows(2)
            .all(|pair| pair[0] != tilo_actions::InteractionPhase::Committed),
        "nothing after done: {phases:?}"
    );
    // What it wrote is its selection in the document, which every window
    // showing the document highlights in its colour.
    let selected = selected.expect("where the agent left off");
    let (key, _, _) = tilo_relay_protocol::live::read_presence_anchors(
        &tilo_relay_protocol::live::LiveAnchor {
            cursor: hex::decode(&selected.anchor).unwrap(),
        },
        &tilo_relay_protocol::live::LiveAnchor {
            cursor: hex::decode(&selected.head).unwrap(),
        },
    )
    .expect("the selection resolves");
    assert!(
        matches!(&key, collab_types::presence::PresenceKey::Resource { app, .. } if app == WRITER)
            || key.scope_digest().is_none(),
        "{key:?}"
    );

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

    // Another call in the same workbook opens no second window of it.
    presence.called(&new_sheet, false, server.acted()).await;
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    assert_eq!(
        desk_windows(&mut browser, &desk, &sheets.record),
        1,
        "one window shows the workbook"
    );

    // An agent that idles between calls restates itself, so a member that came in after its last
    // call still draws it within one restatement.
    let _ = heard(&mut browser);
    let (idle, mut last, mut longest) = (Instant::now(), Instant::now(), Duration::ZERO);
    while idle.elapsed() < Duration::from_secs(10) {
        let (frames, _) = heard(&mut browser);
        if frames
            .iter()
            .any(|frame| matches!(frame.payload, collab_types::PeerPresencePayload::Shell(_)))
        {
            longest = longest.max(last.elapsed());
            last = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    longest = longest.max(last.elapsed());
    assert!(
        longest <= Duration::from_secs(4),
        "an idle agent went {longest:?} without a word"
    );

    // And it leaves with the connection, rather than standing there.
    presence.leave();
    loop {
        let left = heard(&mut browser)
            .0
            .into_iter()
            .any(|frame| frame.payload == collab_types::PeerPresencePayload::Left);
        if left {
            break;
        }
        assert!(Instant::now() < deadline, "the agent never left");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The runs the browser holds of the workspace's Terminal sessions.
fn terminal_runs(browser: &mut Browser) -> Vec<terminal::resource::Run> {
    browser.pump();
    let sessions: Vec<DocumentRoute> = browser
        .documents
        .keys()
        .filter(|route| route.app_id == "so.tilo.terminal")
        .cloned()
        .collect();
    // Read as the record's document, not mounted: a live Terminal claims
    // the session for its own process as it opens.
    sessions
        .iter()
        .flat_map(|route| {
            let mut doc = automerge::AutoCommit::new();
            for (_, bytes) in browser.documents.get(route).into_iter().flatten() {
                let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
            }
            <terminal::resource::Terminal as ui::tilo_resource::Resource>::read(&doc)
                .map(|session| {
                    session
                        .runs
                        .iter()
                        .map(|(_, run)| run.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_runs_a_command_in_its_workspace_in_one_step() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = support::scratch_dir();
    // A workspace's shell works in a directory an environment may be
    // granted: not one below /dev, where the scratch state is.
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options {
            shell: Some(tilo_relay::agents::host_shells(shells.clone()).unwrap()),
            ..Options::offering(common::catalogue, ModelChoice::Scripted)
        },
    )
    .await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
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
    .with_agent_run("run-7", "Claude Code");

    let (listed, _) = tool(&server, "tilo_ls", json!({"path": "terminal"})).await;
    eprintln!("tilo_ls terminal:\n{listed}");
    assert!(listed.contains("terminal"), "{listed}");

    // No session yet: the agent names the command, and the host opens its
    // session, runs it in the workspace's shell and answers with the run.
    let (ran, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#run", "args": {"command": "echo hello from the host && mkdir src && echo 'fn main() {}' > src/main.rs && ls src"}}),
    )
    .await;
    eprintln!("tilo_do terminal#run:\n{ran}");
    assert!(!failed, "{ran}");
    assert!(
        ran.contains("hello from the host")
            && ran.contains("main.rs")
            && ran.contains("\"exit\":0"),
        "{ran}"
    );
    let files = shells.join(WORKSPACE).join("files");
    assert!(files.join("src/main.rs").is_file(), "{}", files.display());

    // The same session keeps its place for the next command.
    let (again, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#run", "args": {"command": "cd src && pwd"}}),
    )
    .await;
    assert!(!failed && again.contains("/src"), "{again}");

    // The browser sees the runs on the session record: the commands, where
    // they ran and how they ended, recorded as the agent's run.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let runs = terminal_runs(&mut browser);
        if runs.len() >= 2 {
            let groups: Vec<tilo_actions::WorkGroup> = browser
                .documents
                .iter()
                .filter(|(route, _)| route.app_id == "so.tilo.terminal")
                .flat_map(|(_, changes)| {
                    let mut doc = automerge::AutoCommit::new();
                    for (_, bytes) in changes {
                        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
                    }
                    doc.get_changes(&[])
                        .iter()
                        .filter_map(|change| change.message())
                        .filter_map(|message| {
                            tilo_actions::ActionContext::from_change_message(message)
                        })
                        .filter_map(|context| context.group)
                        .collect::<Vec<_>>()
                })
                .collect();
            assert!(
                groups
                    .iter()
                    .any(|run| run.id == "run-7" && run.label == "Claude Code"),
                "History keeps the runs as the agent's: {groups:?}"
            );
            assert!(
                runs.iter()
                    .any(|run| run.command.0.starts_with("echo hello from the host")
                        && run.exit == 0
                        && run.cwd.0 == files.to_string_lossy())
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the runs never reached the browser: {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A command that takes minutes is not silent while it runs: what it printed
/// so far comes back with the `pending` answer and with `tilo_status`, and
/// the agent's cancel ends it in the workspace's shell, recording the run it
/// stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_long_run_shows_its_output_and_its_cancel_ends_it() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = support::scratch_dir();
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options {
            shell: Some(tilo_relay::agents::host_shells(shells.clone()).unwrap()),
            ..Options::offering(common::catalogue, ModelChoice::Scripted)
        },
    )
    .await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
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
    .with_agent_run("run-9", "Claude Code");

    // A build that prints, then takes a long time: the call answers pending
    // with the id this agent chose for it, and what it printed until then.
    let invocation = "3f6b8c0e-0000-4000-8000-000000000009".to_owned();
    let (ran, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#run",
               "args": {"command": "echo building && sleep 300 && echo never"},
               "invocation_id": invocation,
               "wait_ms": 2000}),
    )
    .await;
    assert!(failed, "a run still going is no success: {ran}");
    assert!(
        ran.contains("pending"),
        "the call is answered pending: {ran}"
    );
    assert!(
        ran.contains("building"),
        "the pending answer shows what the run printed: {ran}"
    );
    assert!(!ran.contains("never"), "{ran}");

    // Asking how it went shows the newest output as it stands.
    let (status, failed) = tool(&server, "tilo_status", json!({"invocation_id": invocation})).await;
    assert!(failed, "a run still going is no success: {status}");
    assert!(
        status.contains("building") && status.contains("still running"),
        "the pending run says what it printed: {status}"
    );

    // The agent cancels it, and the workspace's shell ends the command.
    let asked_at = Instant::now();
    let _ = tool(
        &server,
        "tilo_status",
        json!({"invocation_id": invocation, "cancel": true}),
    )
    .await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (status, _) = tool(&server, "tilo_status", json!({"invocation_id": invocation})).await;
        if status.trim() == "cancelled" {
            break;
        }
        assert!(
            Instant::now() < deadline && asked_at.elapsed() < Duration::from_secs(30),
            "the run was never cancelled: {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The run it stopped is recorded like any other end: the command, where
    // it ran, and the status a stopped command has.
    loop {
        let runs = terminal_runs(&mut browser);
        if let Some(stopped) = runs
            .iter()
            .find(|run| run.command.0.starts_with("echo building"))
        {
            assert_eq!(stopped.exit, 130, "{stopped:?}");
            assert!(
                stopped.ended_ms >= stopped.started_ms,
                "the run says when it ended: {stopped:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the stopped run never reached the browser: {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The workspace's shared desk as a page first writes it: one strip and no
/// window yet.
fn put_desk(browser: &mut Browser) -> DocumentRoute {
    use tilo_desktop_model::{DesktopTree, Strip, StripId, Workspace, WorkspaceSlot};
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Workspace");
    workspace.strips = vec![Strip {
        id: StripId(10),
        name: "Main".into(),
        window_groups: Vec::new(),
    }];
    let desktop = tilo_desktop_model::ownership::SharedDesktop {
        tree: DesktopTree {
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        ..tilo_desktop_model::ownership::SharedDesktop::default()
    };
    let mut doc = automerge::AutoCommit::new();
    tilo_desktop_model::shared_record::write(&mut doc, &desktop).unwrap();
    doc.commit();
    let route = DocumentRoute {
        app_id: tilo_desktop_model::ownership::DESKTOP_APP.into(),
        cluster_id: browser.cluster_id.clone(),
        record: tilo_desktop_model::ownership::DESKTOP_RECORD.into(),
    };
    let bytes = doc.save();
    browser
        .peer
        .publish(
            route.clone(),
            &collab_types::DocumentMutation::Snapshot(bytes.clone()),
        )
        .unwrap();
    // The host never echoes a device's own change back to it.
    browser
        .documents
        .entry(route.clone())
        .or_default()
        .push(([0; 32], bytes));
    route
}

/// How many windows of the desk at `desk` show `record`, as the page has
/// received it.
fn desk_windows(browser: &mut Browser, desk: &DocumentRoute, record: &str) -> usize {
    use tilo_automerge::ApplyIncoming as _;
    browser.pump();
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in browser.documents.get(desk).into_iter().flatten() {
        doc.load_incoming(bytes).unwrap();
    }
    tilo_desktop_model::shared_record::read(&doc, None).map_or(0, |desktop| {
        desktop
            .records
            .iter()
            .filter(|(tab, shown)| shown == record && desktop.tree.tab(*tab).is_some())
            .count()
    })
}
