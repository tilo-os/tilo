//! An agent on a computer of the person's (`tilo login`, then `tilo mcp`):
//! the runtime's MCP projection of the workspace, served on that computer,
//! its actions asked of the workspace's host through the computer's own
//! session as one of the person's devices.
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;
use tilo_session_client::Outbox;

/// The app the agent writes in.
const WRITER: &str = "so.tilo.word-processor";

/// What the projection answers `request`.
async fn ask(server: &McpServer<SessionActions>, request: Value) -> Value {
    server.handle(request).await.expect("a request is answered")
}

/// The text a tool call answers, and whether it failed.
pub(super) async fn tool(
    server: &McpServer<SessionActions>,
    name: &str,
    arguments: Value,
) -> (String, bool) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_computer_lists_the_workspace_and_writes_what_another_device_sees() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let sheets = browser.create(SHEETS);
    browser.put_layout();
    let writer = browser.create(WRITER);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    // The agent's computer enters the workspace as a device of the person's,
    // as `tilo login` does, and serves the projection over its session.
    let agent = Browser::open(&hosted, &computer, &host).await;
    // Its session is the agent's, as `tilo login` makes it say.
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_agent_session("session-1", "Claude Code");

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
    let ran = ask(&server, new_sheet).await;
    assert_eq!(ran["result"]["isError"], false, "{ran}");
    // The call names the record it worked in, which it opened as its window.
    let acted = server
        .acted()
        .expect("the call names the record it worked in");
    assert_eq!(acted.app, SHEETS);
    assert_eq!(acted.record, sheets.document);
    // And it writes in the document the other device made.
    let (wrote, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "word-processor#append-paragraph", "args": {"text": "Hello"}}),
    )
    .await;
    assert!(!failed, "{wrote}");

    // The other device sees the agent present — once, as an agent the host
    // speaks for, by the name the host knows, saying what it does and in
    // which record — narrated as it went, and sees its change, made by the
    // host as the workspace's own, whose History says it was the agent's run.
    // The agent's own computer says nothing of itself.
    let spoken_by = hex::encode(host.cert.device_public_key);
    let deadline = tilo_test_env::hang_deadline();
    let mut edited = false;
    let mut shown = false;
    let (view, phases) =
        loop {
            browser.pump();
            let here: Vec<collab_types::presence::Shown> = browser
                .here()
                .into_iter()
                .filter(|member| member.agent)
                .collect();
            let run = here
                .iter()
                .find(|member| member.label == "Claude Code")
                .cloned();
            let phases: Vec<tilo_actions::InteractionPhase> = browser
                .narrated
                .iter()
                .filter(|(_, work)| {
                    run.as_ref()
                        .is_some_and(|run| work.actor.agent_id() == Some(run.publisher.as_str()))
                })
                .filter(|(record, work)| {
                    work.action.id == "new-sheet"
                        && (record.app.as_str(), record.record.as_str())
                            == (SHEETS, sheets.document.as_str())
                })
                .map(|(_, work)| work.phase.clone())
                .collect();
            if !edited {
                let app = browser.mount(&sheets);
                if sheet_names(&app).iter().any(|name| name == "Budget") {
                    let groups: Vec<tilo_actions::WorkGroup> = app
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
                        groups.iter().any(|run| run.id.starts_with("agent-session:")
                            && run.label == "Claude Code"),
                        "the change names the agent's session: {groups:?}"
                    );
                    edited = true;
                }
            }
            // The workbook it works in is open on the workspace's layout, as a
            // window of the person's, for every page to draw.
            shown = shown || layout_windows(&mut browser, &sheets.document) > 0;
            // What it does reads as the catalogue's own line for such a step, in
            // the app as a person reads its name, not as an English verb glued to
            // an id.
            let working = ui::step::lines::step(&tilo_actions::Step::new(
                tilo_actions::StepKind::Work,
                Some("Word processor"),
            ));
            let said = run.as_ref().is_some_and(|run| {
                run.spoken_by.as_deref() == Some(spoken_by.as_str())
                    && run.activity.as_deref() == Some(working.as_str())
                    && run.selection.is_some()
            });
            let committed = phases.contains(&tilo_actions::InteractionPhase::Committed);
            if edited && said && shown && committed {
                assert_eq!(here.len(), 1, "the agent is seen once: {here:?}");
                break (run.unwrap(), phases);
            }
            assert!(
                Instant::now() < deadline,
                "the agent's sheet ({edited}), its window ({shown}), the agent the host speaks for \
             ({run:?}), or its narrated action ({phases:?}) never all reached the browser; \
             here: {here:?}",
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

    // It was seen applying the action and done, in that order.
    assert_eq!(
        phases.first(),
        Some(&tilo_actions::InteractionPhase::Applying),
        "{phases:?}"
    );
    assert!(
        phases
            .windows(2)
            .all(|pair| pair[0] != tilo_actions::InteractionPhase::Committed),
        "nothing after done: {phases:?}"
    );
    // What it wrote last is its selection in the document, which every
    // window showing the document highlights in its colour, and it is in
    // the record it wrote in: following it brings the person there.
    let selected = view.selection.clone().expect("where the agent left off");
    let (key, _, _) = tilo_server_protocol::live::read_presence_anchors(
        &tilo_server_protocol::live::LiveAnchor {
            cursor: hex::decode(&selected.anchor).unwrap(),
        },
        &tilo_server_protocol::live::LiveAnchor {
            cursor: hex::decode(&selected.head).unwrap(),
        },
    )
    .expect("the selection resolves");
    assert!(
        matches!(&key, collab_types::presence::PresenceKey::Resource { app, .. } if app == WRITER)
            || key.scope_digest().is_none(),
        "{key:?}"
    );
    let wrote_in = tilo_server_protocol::live::LiveResource {
        app: WRITER.into(),
        record: writer.document.clone(),
    };
    assert_eq!(
        view.focus
            .as_ref()
            .map(|focus| (focus.app.as_str(), focus.document.clone())),
        Some((WRITER, hex::encode(wrote_in.short_digest())))
    );

    // Another change in the same workbook opens no second window of it.
    let again = ask(
        &server,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "tilo_do",
                          "arguments": {"path": "sheets#new-sheet",
                                        "args": {"name": "Spending"}}}}),
    )
    .await;
    assert_eq!(again["result"]["isError"], false, "{again}");
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    assert_eq!(
        layout_windows(&mut browser, &sheets.document),
        1,
        "one window shows the workbook"
    );

    // An agent that idles between calls is restated by the host, so a member
    // that came in after its last call still draws it within one restatement.
    tokio::time::sleep(Duration::from_secs(10)).await;
    browser.pump();
    browser.board =
        tilo_live::presence::Board::new(WORKSPACE, browser_device.cert.device_public_key);
    browser.board.set_host(Some(host.cert.device_public_key));
    let came_in = Instant::now();
    loop {
        browser.pump();
        if browser
            .here()
            .iter()
            .any(|member| member.label == "Claude Code")
        {
            break;
        }
        assert!(
            came_in.elapsed() <= Duration::from_secs(4),
            "an idle agent went unsaid for {:?}",
            came_in.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // And it leaves with the connection, which `tilo mcp` closes as it ends,
    // rather than staying there.
    drop(server);
    agent.peer.close();
    loop {
        browser.pump();
        if !browser
            .here()
            .iter()
            .any(|member| member.label == "Claude Code")
        {
            break;
        }
        assert!(Instant::now() < deadline, "the agent never left");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// An agent on the person's computer opens Notes and makes a note: the Notes window reaches the
/// person's layout as a member's open puts it there, named as the agent's run's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_an_agent_opens_reaches_the_persons_layout() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    browser.put_layout();
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_agent_session("session-1", "Claude Code");

    agent_opens_notes(&server, &mut browser).await;
}

/// The same on a host someone else runs (a Lounge's), which is no member of the person's
/// workspace, with Notes running already: the agent's Notes window reaches the person's layout
/// all the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_an_agent_opens_on_another_persons_host_reaches_the_persons_layout() {
    let mut guest = Box::pin(Guest::new(true, true)).await;
    agent_opens_notes(&guest.server, &mut guest.browser).await;
}

/// An agent opens Notes, which it started without a window: its open alone, before the agent
/// changes anything, puts Notes' window in front of the person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_open_of_a_running_app_shows_its_window_on_the_persons_layout() {
    let mut guest = Box::pin(Guest::new(false, true)).await;
    let (started, failed) = tool(&guest.server, "tilo_do", json!({"path": "notes#start"})).await;
    assert!(!failed, "{started}");
    assert_eq!(
        layout_windows(&mut guest.browser, ""),
        0,
        "a started app has no window"
    );
    let (opened, failed) = tool(&guest.server, "tilo_do", json!({"path": "notes#open"})).await;
    assert!(!failed, "{opened}");
    assert!(
        opened.starts_with("ok: notes is running"),
        "Notes ran already: {opened}"
    );
    let deadline = tilo_test_env::hang_deadline();
    while layout_windows(&mut guest.browser, "") == 0 {
        assert!(
            Instant::now() < deadline,
            "the open answered ({opened}) but no Notes window reached the person's layout"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        layout_windows(&mut guest.browser, ""),
        1,
        "one Notes window"
    );
}

/// An agent opens Notes in a workspace whose layout the serving device does not hold: the open
/// answers at once, nothing waits for a window, and the view is asked for in the layout's document,
/// where the person's page finds it and puts it on their layout when it holds one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_open_with_no_layout_asks_the_persons_page() {
    let mut guest = Box::pin(Guest::new(false, false)).await;
    let asked = Instant::now();
    let answer = ask(
        &guest.server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments": {"path": "notes#open"},
                "_meta": {"so.tilo.mcp": {"structured": true}}}}),
    )
    .await;
    let result = &answer["result"];
    assert_eq!(result["isError"], false, "Notes is open: {answer}");
    assert!(
        result["structuredContent"].get("not_shown").is_none(),
        "the view is asked for, which is no failure: {answer}"
    );
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "the open waited for a window: {:?}",
        asked.elapsed()
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let views = guest.browser.asked_views();
        if let Some(view) = views.iter().find(|view| view.app_id == "so.tilo.notes") {
            assert_eq!(
                view.opened_by
                    .as_ref()
                    .map(|session| session.label.as_str()),
                Some("Claude Code"),
                "named as the agent session's: {view:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the person's page never received the ask: {views:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A person on a host someone else runs (a Lounge's), which is no member of their workspace:
/// their page holds Notes' document when `notes` and has written the workspace's layout when
/// `layout` (with no window on it), and an agent on their computer is served over the computer's
/// own session.
struct Guest {
    browser: Browser,
    server: McpServer<SessionActions>,
    _agent: Browser,
    _hosted: Hosted,
    _state: tempfile::TempDir,
}

impl Guest {
    async fn new(notes: bool, layout: bool) -> Self {
        let (operator, _) = tilo_user_identity::UserIdentity::generate("Operator", 1);
        let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
        let lounge = enrol(&operator, "lounge", DeviceCapabilities::member());
        let page_device = enrol(&guest, "browser", DeviceCapabilities::member());
        let computer = enrol(
            &guest,
            "Claude Code on laptop",
            DeviceCapabilities::member(),
        );
        let state = crate::support::scratch_dir();
        let hosted = start(&operator, &lounge, state.path()).await;
        let workspace = hosted
            .server
            .workspaces()
            .unwrap()
            .create("Grace's workspace")
            .await
            .unwrap();
        let cluster = workspace.workspace.clone();
        for device in [&page_device, &computer] {
            super::resource_support::admit_person(
                &hosted,
                &lounge,
                &cluster,
                device,
                tilo_peer_state::join::Role::Editor,
            )
            .await;
        }
        let mut browser =
            super::resource_support::open_admitted_peer(&hosted, &page_device, &lounge, &cluster)
                .await;
        if notes {
            browser.create("so.tilo.notes");
        }
        if layout {
            browser.put_layout();
        }
        browser.peer.barrier().unwrap().recv().unwrap().unwrap();
        let agent =
            super::resource_support::open_admitted_peer(&hosted, &computer, &lounge, &cluster)
                .await;
        let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
        let person = hex::encode(guest.verifying_key().to_bytes());
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, lounge.cert.device_public_key);
        let server = McpServer::new(actions, cluster, person)
            .with_agent_session("session-guest", "Claude Code");
        Self {
            browser,
            server,
            _agent: agent,
            _hosted: hosted,
            _state: state,
        }
    }
}

/// The agent behind `server` opens Notes and makes a note; `browser`'s layout gets one Notes
/// window, named as the agent's run's.
pub(super) async fn agent_opens_notes(server: &McpServer<SessionActions>, browser: &mut Browser) {
    let (opened, failed) = tool(server, "tilo_do", json!({"path": "notes#open"})).await;
    assert!(!failed, "{opened}");
    let (wrote, failed) = tool(
        server,
        "tilo_do",
        json!({"path": "notes#new-note", "args": {"title": "Hello from the agent"}}),
    )
    .await;
    assert!(!failed, "{wrote}");
    let notes = server
        .acted()
        .expect("the call names the record it wrote in");
    assert_eq!(notes.app, "so.tilo.notes");

    let deadline = tilo_test_env::hang_deadline();
    while layout_windows(browser, &notes.record) == 0 {
        assert!(
            Instant::now() < deadline,
            "the Notes window the agent opened never reached the person's layout ({opened}; {wrote})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        layout_windows(browser, &notes.record),
        1,
        "one Notes window"
    );
}

/// The runs the browser holds of the workspace's Terminal sessions.
fn terminal_commands(browser: &mut Browser) -> Vec<terminal::resource::Command> {
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
                        .commands
                        .iter()
                        .map(|(_, run)| run.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// A server's options whose workspaces' Terminal sessions keep their shells
/// in `shells`, on the server itself.
fn shelled(shells: &std::path::Path) -> Options {
    Options {
        shell: Some(tilo_server::agents::host_shells(shells.to_path_buf()).unwrap()),
        ..Options::offering(crate::common::catalogue, ModelChoice::Scripted)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_runs_a_command_in_its_workspace_in_one_step() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    // A workspace's shell works in a directory an environment may be
    // granted: not one below /dev, where the scratch state is.
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = crate::harness::hosted::start(&root, &host, state.path(), shelled(&shells)).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    // Its session is the agent's, as `tilo login` makes it say.
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_agent_session("session-7", "Claude Code");

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

    // The browser sees them in the session: the commands, where
    // they ran and how they ended, recorded as the agent's run.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let commands = terminal_commands(&mut browser);
        if commands.len() >= 2 {
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
                    .any(|run| run.id.starts_with("agent-session:") && run.label == "Claude Code"),
                "History keeps the runs as the agent's: {groups:?}"
            );
            assert!(commands.iter().any(
                |run| run.command.0.starts_with("echo hello from the host")
                    && run.exit == 0
                    && run.cwd.0 == files.to_string_lossy()
            ));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the runs never reached the browser: {commands:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A window of another device opened the workspace's Terminal session once,
/// which set the session up on that device and claimed its pane there, and
/// closed again: the session still names that device. An agent's
/// command still goes to the workspace's shell, on the server, and lands in
/// that session's commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_command_lands_after_another_devices_terminal_closed() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = crate::harness::hosted::start(&root, &host, state.path(), shelled(&shells)).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    // The device's Terminal window set its session up there and claimed a
    // pane, as a window does when it opens; then it closed.
    let page_device = hex::encode(browser_device.cert.device_public_key);
    let ada = MemberId::person(hex::encode(root.verifying_key().to_bytes()));
    let mut page = crate::common::open_in(
        &browser.cluster_id,
        "so.tilo.terminal",
        &ada,
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let session = DocumentRoute {
        app_id: "so.tilo.terminal".into(),
        cluster_id: browser.cluster_id.clone(),
        document: page.wire_document().to_owned(),
    };
    for (action, arguments) in [
        (
            "record-target",
            json!({"target": terminal::target::SessionTarget::Host.encode(), "device": page_device}),
        ),
        (
            "claim-pane",
            json!({"pane": "6f1c2b8e-0000-4000-8000-000000000001", "device": page_device}),
        ),
    ] {
        let resource = page
            .survey()
            .into_iter()
            .find(|descriptor| descriptor.address.action == action)
            .unwrap_or_else(|| panic!("the session offers {action}"))
            .address
            .resource;
        page.invoke(
            &tilo_actions::api::ActionAddress::new(&resource, action),
            arguments.as_object().unwrap(),
            true,
        )
        .unwrap();
    }
    let claimed = page.take_mutation().unwrap().unwrap();
    browser.peer.write(session.clone(), &claimed).unwrap();
    let bytes = match claimed {
        collab_types::DocumentMutation::Snapshot(bytes)
        | collab_types::DocumentMutation::Incremental(bytes) => bytes,
    };
    browser
        .documents
        .entry(session.clone())
        .or_default()
        .push(([0; 32], bytes));
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_agent_session("session-8", "Claude Code");

    let (ran, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#run", "args": {"command": "echo ran on the server"}}),
    )
    .await;
    assert!(!failed, "{ran}");
    assert!(
        ran.contains("ran on the server") && ran.contains("\"exit\":0"),
        "{ran}"
    );

    let deadline = tilo_test_env::hang_deadline();
    loop {
        if terminal_commands(&mut browser)
            .iter()
            .any(|run| run.command.0 == "echo ran on the server" && run.exit == 0)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the command never reached the Terminal session"
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
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = crate::harness::hosted::start(&root, &host, state.path(), shelled(&shells)).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    // Its session is the agent's, as `tilo login` makes it say.
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_agent_session("session-9", "Claude Code");

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
        let commands = terminal_commands(&mut browser);
        if let Some(stopped) = commands
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
            "the stopped run never reached the browser: {commands:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How many windows of the workspace's layout show `record`, as the page has
/// received it: each says the agent's run opened it.
fn layout_windows(browser: &mut Browser, record: &str) -> usize {
    browser.layout().map_or(0, |desktop| {
        desktop
            .records
            .iter()
            .filter(|(tab, shown)| shown == record && desktop.tree.tab(*tab).is_some())
            .inspect(|(tab, _)| {
                let run = desktop.opened_by.get(tab);
                assert!(
                    run.is_some_and(
                        |run| run.label == "Claude Code" && run.run.starts_with("agent-session:")
                    ),
                    "the window names the agent's run: {run:?}"
                );
            })
            .count()
    })
}

/// The agent runs the host speaks for that `browser`'s board lists now, by the
/// host's device: each run is a member of its own, named as the host knows it.
fn runs_listed(
    browser: &mut Browser,
    host: &crate::support::Device,
) -> Vec<collab_types::presence::Shown> {
    let spoken_by = hex::encode(host.cert.device_public_key);
    browser.pump();
    browser
        .here()
        .into_iter()
        .filter(|member| {
            member.agent
                && member.label == "Claude Code"
                && member.spoken_by.as_deref() == Some(spoken_by.as_str())
        })
        .collect()
}

/// How an agent's session ends.
#[derive(Clone, Copy, Debug)]
enum Ending {
    /// Its session closes: `tilo mcp`'s input closed, or its process was killed.
    Closes,
    /// The person stops it from its card, through their MCP server.
    Stopped,
}

/// The longest the person's page may still list an agent after its session
/// ended: well inside the silence after which a page forgets a member that
/// says nothing (eight seconds).
const LEAVES_WITHIN: Duration = Duration::from_secs(4);

/// An agent whose session closes leaves who is here as soon as its session is
/// gone, rather than once the page gave up waiting for its word.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_session_closes_leaves_who_is_here() {
    Box::pin(leaves_who_is_here(Ending::Closes)).await;
}

/// The person stops their agent from its card: its session ends for good, and
/// it leaves who is here with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_agent_leaves_who_is_here() {
    Box::pin(leaves_who_is_here(Ending::Stopped)).await;
}

/// An agent whose session ends as `ending` says leaves who is here on the
/// person's page: the host that spoke for its run says it left within
/// [`LEAVES_WITHIN`], so the page can say the agent finished; a stopped
/// agent's session connects no more.
async fn leaves_who_is_here(ending: Ending) {
    use tilo_actions::remote::{ActionProvider as _, Caller, InvokeRequest, mcp_server};
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let device = computer.cert.device_public_key;
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());
    let agent = Browser::open(&hosted, &computer, &host).await;
    tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code")
        .await
        .expect("the computer's session is the agent's run");
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_agent_session("session-1", "Claude Code");
    let (_, failed) = tool(&server, "tilo_ls", json!({})).await;
    assert!(!failed, "the agent lists the workspace");
    let deadline = tilo_test_env::hang_deadline();
    while runs_listed(&mut browser, &host).is_empty() {
        assert!(
            Instant::now() < deadline,
            "{ending:?}: the agent never arrived"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let ended = Instant::now();
    match ending {
        Ending::Closes => agent.peer.close(),
        Ending::Stopped => {
            let api = SessionActions::new(
                &browser.peer,
                browser.workspace,
                host.cert.device_public_key,
            );
            let request = InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: mcp_server::target(mcp_server::STOP),
                argument: mcp_server::argument(&mcp_server::AgentSessionRef {
                    grant: hex::encode(device),
                    session: "Claude Code".into(),
                }),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            };
            let receipt = api
                .invoke(&Caller::local_person("ada"), request)
                .await
                .expect("the action API answers")
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
    while !runs_listed(&mut browser, &host).is_empty() {
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
            runs_listed(&mut browser, &host).is_empty(),
            "a stopped agent came back on its own"
        );
        assert_eq!(
            agent.peer.status().phase,
            tilo_session_client::TransportPhase::Closed,
            "a stopped agent's session connects no more"
        );
    }
    drop(server);
}

/// The longest an open page may take to list the agent of a new `tilo mcp`
/// run on the same computer: a run that restarts (after `tilo login`, or as an
/// agent's client restarts its MCP server) is the same agent coming back to
/// the person.
const COMES_BACK_WITHIN: Duration = Duration::from_secs(6);

/// One computer runs `tilo mcp` again and again: the person's page, open the
/// whole time, lists each run's agent once, as the host speaks for it, and
/// drops it as its session ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_that_comes_back_is_heard_by_an_open_page_every_time() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());
    let mut seen = std::collections::BTreeSet::new();
    for run in 0..4 {
        let agent = Browser::open(&hosted, &computer, &host).await;
        tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the computer's session is the agent's run");
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
        let server = McpServer::new(actions, WORKSPACE.into(), person.clone())
            .with_agent_session(format!("session-{run}"), "Claude Code");
        let opened = Instant::now();
        let (_, failed) = tool(&server, "tilo_ls", json!({})).await;
        assert!(!failed, "run {run}: the agent lists the workspace");
        let listed = loop {
            let listed = runs_listed(&mut browser, &host);
            if let [only] = listed.as_slice()
                && !seen.contains(&only.publisher)
                && only.activity.is_some()
            {
                break only.clone();
            }
            assert!(
                opened.elapsed() < COMES_BACK_WITHIN,
                "run {run}: the open page never heard the agent come back, once and saying \
                 what it does ({:?} since its session opened): {listed:?}",
                opened.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        seen.insert(listed.publisher);
        let ended = Instant::now();
        agent.peer.close();
        while !runs_listed(&mut browser, &host).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "run {run}: the agent is still listed {:?} after its session ended",
                ended.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(server);
        drop(agent);
    }
}

/// How long the agent in [`a_quiet_agent_stays_while_its_session_lasts`]
/// says nothing: longer than an agent's model takes to think or its person
/// to answer it, and longer than the host ever kept a run that took no action.
const QUIET: Duration = Duration::from_secs(35);

/// An agent stays in who is here for as long as its session lasts, however
/// long it goes without acting: one whose `tilo mcp` was killed leaves, and
/// the one started right after it, which took one action and then waits on
/// its person, is still listed, once, until its own session ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_agent_stays_while_its_session_lasts() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());
    // One `tilo mcp` of the computer's, listed once it lists the workspace.
    let mut runs = Vec::new();
    for run in 0..2 {
        let agent = Browser::open(&hosted, &computer, &host).await;
        tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the computer's session is the agent's run");
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
        let server = McpServer::new(actions, WORKSPACE.into(), person.clone())
            .with_agent_session(format!("session-{run}"), "Claude Code");
        let (_, failed) = tool(&server, "tilo_ls", json!({})).await;
        assert!(!failed, "run {run}: the agent lists the workspace");
        let opened = Instant::now();
        while runs_listed(&mut browser, &host).len() != 1 {
            assert!(
                opened.elapsed() < COMES_BACK_WITHIN,
                "run {run}: the page never listed the agent once: {:?}",
                runs_listed(&mut browser, &host)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if run == 0 {
            // Killed: its session ends without a word, and it leaves.
            agent.peer.close();
            let ended = Instant::now();
            while !runs_listed(&mut browser, &host).is_empty() {
                assert!(
                    ended.elapsed() < LEAVES_WITHIN,
                    "the killed agent is still listed {:?} after its session ended",
                    ended.elapsed()
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        runs.push((agent, server));
    }
    let quiet = Instant::now();
    while quiet.elapsed() < QUIET {
        let listed = runs_listed(&mut browser, &host);
        assert_eq!(
            listed.len(),
            1,
            "the waiting agent is listed once {:?} after its last action, with its session open: \
             {listed:?}",
            quiet.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let (agent, _server) = runs.pop().expect("the second run");
    agent.peer.close();
    let ended = Instant::now();
    while !runs_listed(&mut browser, &host).is_empty() {
        assert!(
            ended.elapsed() < LEAVES_WITHIN,
            "the agent is still listed {:?} after its session ended",
            ended.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// An agent is present from the moment its `tilo mcp` connects, before it
/// asks anything: the host says its run waiting, and the run's first request
/// moves that same entry on to what it does, rather than a second one
/// appearing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_is_present_waiting_as_soon_as_it_connects() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());

    let agent = Browser::open(&hosted, &computer, &host).await;
    tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code")
        .await
        .expect("the computer's session is the agent's run");
    let connected = Instant::now();
    loop {
        let listed = runs_listed(&mut browser, &host);
        if let [run] = listed.as_slice() {
            assert_eq!(
                run.activity.as_deref(),
                Some("Waiting"),
                "an agent that has asked nothing is waiting: {run:?}"
            );
            break;
        }
        assert!(
            connected.elapsed() < COMES_BACK_WITHIN,
            "the page never listed the connected agent before its first request: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_agent_session("session-0", "Claude Code");
    let (_, failed) = tool(&server, "tilo_ls", json!({})).await;
    assert!(!failed, "the agent lists the workspace");
    let asked = Instant::now();
    loop {
        let listed = runs_listed(&mut browser, &host);
        assert!(
            listed.len() <= 1,
            "the run's first request moves its entry on: {listed:?}"
        );
        if listed
            .first()
            .is_some_and(|run| run.activity.as_deref() != Some("Waiting"))
        {
            break;
        }
        assert!(
            asked.elapsed() < COMES_BACK_WITHIN,
            "the agent still reads waiting after its first request: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    agent.peer.close();
}

/// A run that ended, its face gone from the page, and the next `tilo mcp` of
/// the same computer: the page that stayed open lists the agent as soon as the
/// new run starts, with nothing left behind by the one that ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_run_after_one_that_ended_is_on_the_page_at_once() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());
    for run in 0..3 {
        let agent = Browser::open(&hosted, &computer, &host).await;
        tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the computer's session is the agent's run");
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
        let server = McpServer::new(actions, WORKSPACE.into(), person.clone())
            .with_agent_session(format!("session-{run}"), "Claude Code");
        let opened = Instant::now();
        let (_, failed) = tool(&server, "tilo_ls", json!({})).await;
        assert!(!failed, "run {run}: the agent lists the workspace");
        while runs_listed(&mut browser, &host).is_empty() {
            assert!(
                opened.elapsed() < COMES_BACK_WITHIN,
                "run {run}: the page never listed the agent {:?} after its session opened",
                opened.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        agent.peer.close();
        let ended = Instant::now();
        while !runs_listed(&mut browser, &host).is_empty() {
            assert!(
                ended.elapsed() < LEAVES_WITHIN,
                "run {run}: the agent is still listed {:?} after its session ended",
                ended.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The page sits with the agent gone while its client restarts the
        // server, as it does between two `tilo mcp` runs.
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(server);
        drop(agent);
    }
}

/// What a write may take at most: a write answers in well under a second, and one that
/// waited out the host's ten-second wait for what it reads takes several times this.
const WRITE_WITHIN: Duration = Duration::from_secs(3);

/// An agent's writes on a logged-in computer answer as soon as the host ran them, in a
/// workspace no page wrote a window layout in: an action waits for the data it reads and
/// writes, never for a layout or a window to show it on, whether it makes a record,
/// writes in one, or opens one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_writes_answer_without_waiting_out_a_timeout() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    // The person's page is in, and has written no layout.
    let browser = Browser::open(&hosted, &browser_device, &host).await;
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_agent_session("session-1", "Claude Code");
    // Reads warm the agent's session up; what is timed below is the writes.
    let (apps, failed) = tool(&server, "tilo_ls", json!({})).await;
    assert!(!failed, "{apps}");

    let mut slow = Vec::new();
    for (path, args) in [
        ("tasks#create", json!({"title": "First"})),
        ("tasks#create", json!({"title": "Second"})),
        ("tasks#create-area", json!({"name": "Launch"})),
        ("tasks#open", json!({})),
        ("notes#new-note", json!({"title": "Plan"})),
        (
            "notes#append-to-note",
            json!({"title": "Plan", "markdown": "Ship."}),
        ),
    ] {
        let asked = Instant::now();
        let (answer, failed) = tool(&server, "tilo_do", json!({"path": path, "args": args})).await;
        let took = asked.elapsed();
        assert!(!failed, "{path}: {answer}");
        eprintln!("{path}: {took:?}");
        if took > WRITE_WITHIN {
            slow.push((path, took));
        }
    }
    assert!(
        slow.is_empty(),
        "writes waited out a timeout rather than running once the host was ready: {slow:?}"
    );
}
