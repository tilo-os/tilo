//! An agent on a computer of the person's (`tilo login`, then `tilo mcp`):
//! the runtime's MCP projection of the workspace, served on that computer,
//! its actions asked of the workspace's host through the computer's own
//! session as one of the person's devices.
use super::*;
use serde_json::{Value, json};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_runtime::mcp_windows;
use tilo_session_core::remote::SessionActions;

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
    // The records it works in open as windows on the workspace's desk, as
    // `tilo mcp` opens them; the host that executes its run is what shows it
    // there, doing what it does.
    let windows = mcp_windows::AgentWindows::new(actions.clone(), person.clone(), WORKSPACE.into());
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_surface(Surface::Agent)
        .with_agent_run("run-1", "Claude Code");

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
    // The call names the record it worked in, which opens as its window.
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
    windows.changed(changed, server.run_of(&new_sheet)).await;
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
    let (view, phases) = loop {
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
                    .is_some_and(|run| work.actor.id == run.publisher)
            })
            .filter(|(record, work)| {
                work.action.id == "new-sheet"
                    && (record.app.as_str(), record.record.as_str())
                        == (SHEETS, sheets.record.as_str())
            })
            .map(|(_, work)| work.phase.clone())
            .collect();
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
        let said = run.as_ref().is_some_and(|run| {
            run.spoken_by.as_deref() == Some(spoken_by.as_str())
                && run
                    .activity
                    .as_deref()
                    .is_some_and(|line| line.starts_with("Editing"))
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
    let wrote_in = tilo_relay_protocol::live::LiveRecord {
        app: WRITER.into(),
        record: writer.record.clone(),
    };
    assert_eq!(
        view.focus
            .as_ref()
            .map(|focus| (focus.app.as_str(), focus.record.clone())),
        Some((WRITER, hex::encode(wrote_in.short_digest())))
    );

    // Another change in the same workbook opens no second window of it.
    windows
        .changed(
            vec![tilo_runtime::mcp::ActedRecord {
                app: SHEETS.into(),
                record: sheets.record.clone(),
            }],
            server.run_of(&new_sheet),
        )
        .await;
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    assert_eq!(
        desk_windows(&mut browser, &sheets.record),
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
    // rather than standing there.
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

/// The agent runs the host speaks for that `browser`'s board lists now, by the
/// host's device: each run is a member of its own, named as the host knows it.
fn runs_listed(
    browser: &mut Browser,
    host: &support::Device,
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
    /// The person stops it from its card, through their agent door.
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
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_surface(Surface::Agent)
        .with_agent_run("run-1", "Claude Code");
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
            tilo_session_host::TransportPhase::Closed,
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
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let mut seen = std::collections::BTreeSet::new();
    for run in 0..4 {
        let agent = Browser::open(&hosted, &computer, &host).await;
        tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the computer's session is the agent's run");
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
        let server = McpServer::new(actions, WORKSPACE.into(), person.clone())
            .with_surface(Surface::Agent)
            .with_agent_run(format!("run-{run}"), "Claude Code");
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
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    // One `tilo mcp` of the computer's, listed once it lists the workspace.
    let mut runs = Vec::new();
    for run in 0..2 {
        let agent = Browser::open(&hosted, &computer, &host).await;
        tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the computer's session is the agent's run");
        let actions =
            SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
        let server = McpServer::new(actions, WORKSPACE.into(), person.clone())
            .with_surface(Surface::Agent)
            .with_agent_run(format!("run-{run}"), "Claude Code");
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
    let _sheets = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };

    let agent = Browser::open(&hosted, &computer, &host).await;
    tilo_session_host::device::run_as_agent(&agent.peer, "Claude Code")
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
        .with_surface(Surface::Agent)
        .with_agent_run("run-0", "Claude Code");
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
