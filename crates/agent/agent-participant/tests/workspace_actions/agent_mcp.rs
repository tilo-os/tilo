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
            // What the host says for the agent's run is the host's word
            // (`route_parity`); this is what the agent's own device says.
            && event.on_behalf.is_none()
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
            && frame.on_behalf.is_none()
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
    browser.put_desk();
    let writer = browser.create(WRITER);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    // The agent's computer enters the workspace as a device of the person's,
    // as `tilo login` does, and serves the projection over its session.
    let agent = Browser::open(&hosted, &computer, &host).await;
    // Its session runs the agent, as `tilo login` makes it say.
    let _ = tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code").await;
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
    // The workbook it changed opens on the desk.
    let changed = server.take_changed();
    assert_eq!(
        changed,
        std::slice::from_ref(&acted),
        "the call changed the workbook"
    );
    presence
        .called(&new_sheet, false, Some(acted), changed)
        .await;
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
                        .any(|run| run.id.starts_with("agent-run:") && run.label == "Claude Code"),
                    "the change names the agent's run: {runs:?}"
                );
                edited = true;
            }
        }
        // The workbook it works in is open on the workspace's desk, as a
        // window of the person's, for every page to draw.
        shown = shown || desk_windows(&mut browser, &sheets.record) > 0;
        let said = view
            .as_ref()
            .is_some_and(|view| view.activity.as_deref() == Some("Adding Sheets"));
        let committed = phases.contains(&tilo_actions::InteractionPhase::Committed);
        if edited && said && shown && committed && selected.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent's sheet ({edited}), its window ({shown}), what it says it is doing \
             ({said}), its narrated action ({phases:?}) or where it left off ({selected:?}) \
             never all reached the browser\nbrowser channels: {}\nagent channels: {}",
            browser.peer.status().channels,
            agent.peer.status().channels,
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
    assert_eq!(view.activity.as_deref(), Some("Adding Sheets"));
    let edited_record = tilo_relay_protocol::live::LiveRecord {
        app: SHEETS.into(),
        record: sheets.record.clone(),
    };
    assert_eq!(view.focus(), Some((SHEETS, edited_record.short_digest())));

    // Another change in the same workbook opens no second window of it.
    let again = tilo_runtime::mcp::ActedRecord {
        app: SHEETS.into(),
        record: sheets.record.clone(),
    };
    presence
        .called(&new_sheet, false, server.acted(), vec![again])
        .await;
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    assert_eq!(
        desk_windows(&mut browser, &sheets.record),
        1,
        "one window shows the workbook"
    );

    // An agent that idles between calls restates itself, so a member that came in after its last
    // call still draws it within one restatement. What it says again is the word it already said,
    // so the cadence is its board's schedule, and what a reader proves is that the idle loop keeps
    // that schedule armed and says each word it owes. Timing the arrivals here measured this
    // machine instead: a loaded suite starved the reader past a keepalive and handed it the frames
    // in one burst, and an agent that had said every one of them read as silent.
    let _ = heard(&mut browser);
    let keepalive = Duration::from_secs_f64(tilo_live::presence::KEEPALIVE_MS / 1_000.0);
    let mut restated = 0;
    while restated < 2 {
        restated += heard(&mut browser)
            .0
            .iter()
            .filter(|frame| matches!(frame.payload, collab_types::PeerPresencePayload::Shell(_)))
            .count();
        let due = presence.restates_in();
        assert!(
            due.is_some_and(|due| due <= keepalive + Duration::from_secs(1)),
            "an idle agent's next word is due in {due:?}, past a keepalive: it said itself again \
             {restated} times"
        );
        assert!(
            Instant::now() < deadline,
            "an idle agent said itself again only {restated} times"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

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
    // Its session runs the agent, as `tilo login` makes it say.
    let _ = tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code").await;
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
                    .any(|run| run.id.starts_with("agent-run:") && run.label == "Claude Code"),
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
    // Its session runs the agent, as `tilo login` makes it say.
    let _ = tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code").await;
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

/// How many windows of the workspace's desk show `record`, as the page has
/// received it: each says the agent's run opened it.
fn desk_windows(browser: &mut Browser, record: &str) -> usize {
    browser.desk().map_or(0, |desktop| {
        desktop
            .records
            .iter()
            .filter(|(tab, shown)| shown == record && desktop.tree.tab(*tab).is_some())
            .inspect(|(tab, _)| {
                let run = desktop.opened_by.get(tab);
                assert!(
                    run.is_some_and(
                        |run| run.label == "Claude Code" && run.run.starts_with("agent-run:")
                    ),
                    "the window names the agent's run: {run:?}"
                );
            })
            .count()
    })
}

/// The person's page's board of who is here: what a page draws its roster from, fed the presence
/// frames the page's session hears and its own clock, as the page's runtime feeds it.
struct Roster {
    board: tilo_session_client::Presence,
}

impl Roster {
    fn of(device: &support::Device) -> Self {
        let board = tilo_session_client::Presence::over(
            None,
            &tilo_session_host::saying::device(&device.cert).expect("a device"),
            tilo_session_client::presence::OwnPresence {
                label: "Ada".into(),
                user: Some(hex::encode(device.cert.user_public_key)),
                ..tilo_session_client::presence::OwnPresence::default()
            },
        );
        let workspace = tilo_desktop_model::WorkspaceId::parse(WORKSPACE).expect("a workspace");
        let topic = tilo_session_client::topic_of(workspace);
        board.taking_part(&[(topic, WORKSPACE.to_owned())], Some(topic));
        Self { board }
    }

    /// Take what `page` heard, and whether the agent `agent` (its device key) is listed after.
    fn listed(&self, page: &mut Browser, agent: [u8; 32]) -> bool {
        for event in page.events.drain() {
            if let tilo_session_host::PeerEvent::LivePlane {
                workspace_id,
                from,
                track: tilo_live::live::Track::Presence,
                body,
                ..
            } = &event
                && *workspace_id == page.workspace
                && let Ok(from) = iroh::EndpointId::from_bytes(from)
            {
                self.board.heard_body(WORKSPACE, from, body);
            }
            let _ = page.take(event);
        }
        self.board.due_in(WORKSPACE);
        self.board
            .members(WORKSPACE)
            .iter()
            .any(|member| member.agent && member.publisher == hex::encode(agent))
    }
}

/// How an agent's session ends.
#[derive(Clone, Copy, Debug)]
enum Ending {
    /// `tilo mcp`'s input closed: the agent says it left, and its session closes.
    Leaves,
    /// Its session closes without a word, as a process that is killed leaves it.
    Closes,
    /// The person stops it from its card, through their agent door.
    Stopped,
}

/// The longest the person's page may still list an agent after its session ended: well inside
/// the silence after which a page forgets a member that says nothing
/// (eight seconds).
const LEAVES_WITHIN: Duration = Duration::from_secs(4);

/// An agent that says it left, as `tilo mcp` does once its input closes, leaves who is here on
/// the person's page within [`LEAVES_WITHIN`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_that_leaves_leaves_who_is_here() {
    Box::pin(leaves_who_is_here(Ending::Leaves)).await;
}

/// An agent whose session closes without a word, as a killed process leaves it, leaves who is
/// here as soon as its session is gone, rather than once the page gave up waiting for its word.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_session_closes_leaves_who_is_here() {
    Box::pin(leaves_who_is_here(Ending::Closes)).await;
}

/// The person stops their agent from its card: its session ends for good, and it leaves who is
/// here with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_agent_leaves_who_is_here() {
    Box::pin(leaves_who_is_here(Ending::Stopped)).await;
}

/// An agent whose session ends as `ending` says leaves who is here on the person's page: the page
/// lists it no longer within [`LEAVES_WITHIN`], so it can say the agent finished; a stopped agent's
/// session connects no more.
async fn leaves_who_is_here(ending: Ending) {
    use tilo_actions::remote::{ActionHost as _, Caller, InvokeRequest, agent_door};
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let device = computer.cert.device_public_key;
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let roster = Roster::of(&browser_device);
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let agent = Browser::open(&hosted, &computer, &host).await;
    tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code")
        .await
        .expect("the computer's session is the agent's run");
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let presence = mcp_presence::Presence::new(
        (&agent.peer, &computer.cert),
        actions,
        person.clone(),
        (WORKSPACE.into(), agent.workspace),
        "Claude Code".into(),
    )
    .expect("the agent's certificate names a device");
    presence.arrive();
    let deadline = tilo_test_env::hang_deadline();
    while !roster.listed(&mut browser, device) {
        assert!(
            Instant::now() < deadline,
            "{ending:?}: the agent never arrived"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let ended = Instant::now();
    match ending {
        Ending::Leaves => {
            presence.leave();
            agent.peer.close();
        }
        Ending::Closes => agent.peer.close(),
        Ending::Stopped => {
            let door = SessionActions::new(
                &browser.peer,
                browser.workspace,
                host.cert.device_public_key,
            );
            let request = InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: agent_door::target(agent_door::STOP),
                argument: agent_door::argument(&agent_door::RunRef {
                    grant: hex::encode(device),
                    run: "Claude Code".into(),
                }),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            };
            let receipt = door
                .invoke(&Caller::local_person("ada"), request)
                .await
                .expect("the door answers")
                .expect("the person stops their agent");
            assert!(
                !matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Rejected { .. }
                ),
                "{receipt:?}"
            );
        }
    }
    while roster.listed(&mut browser, device) {
        assert!(
            ended.elapsed() < LEAVES_WITHIN,
            "{ending:?}: the agent is still listed {:?} after its session ended",
            ended.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if matches!(ending, Ending::Stopped) {
        // The stopped session stays closed: the agent is not said again.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !roster.listed(&mut browser, device),
            "a stopped agent came back on its own"
        );
        assert_eq!(
            agent.peer.status().phase,
            tilo_session_host::TransportPhase::Closed,
            "a stopped agent's session connects no more"
        );
    }
    drop(presence);
}

/// The longest an open page may take to list the agent of a new `tilo mcp` run on the same
/// computer, with what it is doing: a run that restarts (after `tilo login`, or as an agent's
/// client restarts its MCP server) is the same agent coming back to the person.
const COMES_BACK_WITHIN: Duration = Duration::from_secs(6);

/// One computer runs `tilo mcp` again and again, each run ending its own way: the person's page,
/// open the whole time, lists each run's agent with what it is doing, and drops it as it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_that_comes_back_is_heard_by_an_open_page_every_time() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let device = computer.cert.device_public_key;
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let roster = Roster::of(&browser_device);
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    for (run, ending) in [
        Ending::Leaves,
        Ending::Closes,
        Ending::Leaves,
        Ending::Closes,
    ]
    .into_iter()
    .enumerate()
    {
        let started = Instant::now();
        let agent = Browser::open(&hosted, &computer, &host).await;
        tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the computer's session is the agent's run");
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
        let presence = mcp_presence::Presence::new(
            (&agent.peer, &computer.cert),
            actions,
            person.clone(),
            (WORKSPACE.into(), agent.workspace),
            "Claude Code".into(),
        )
        .expect("the agent's certificate names a device");
        presence.arrive();
        let path = format!("/{SHEETS}/run-{run}");
        presence
            .called(
                &json!({"method": "tools/call",
                        "params": {"name": "tilo_read", "arguments": {"path": path}}}),
                false,
                None,
                Vec::new(),
            )
            .await;
        let opened = Instant::now();
        let doing = |roster: &Roster, browser: &mut Browser| {
            roster.listed(browser, device);
            roster
                .board
                .members(WORKSPACE)
                .into_iter()
                .find(|member| member.agent && member.publisher == hex::encode(device))
                .and_then(|member| member.activity)
        };
        loop {
            let activity = doing(&roster, &mut browser);
            if activity
                .as_deref()
                .is_some_and(|line| line.contains(&format!("run-{run}")))
            {
                break;
            }
            assert!(
                opened.elapsed() < COMES_BACK_WITHIN,
                "run {run}: the open page never heard the agent come back ({:?} since its \
                 session opened, {:?} in all), it shows {activity:?}",
                opened.elapsed(),
                started.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let ended = Instant::now();
        if matches!(ending, Ending::Leaves) {
            presence.leave();
        }
        agent.peer.close();
        while roster.listed(&mut browser, device) {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "run {run} ({ending:?}): the agent is still listed {:?} after its session ended",
                ended.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(presence);
        drop(agent);
    }
}
