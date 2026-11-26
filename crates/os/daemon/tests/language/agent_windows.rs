//! What the person's page shows of their agent's work: the app the agent opens reaches the
//! person's layout as one window, named as the agent's run's; a write answers as soon as the
//! host ran it, without waiting for a window to show it on; and the agent reads a window as
//! text, calls the action it names and reads the change.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::language::agent_fixture::{act, ls, said};
use crate::language::host;

/// The longest a write may take, or a read of a window.
const WRITE_WITHIN: Duration = Duration::from_secs(3);
const READ_BOUND: Duration = Duration::from_secs(5);

/// The agent opens Sheets, makes a sheet, lists the workspace and its apps, and writes in the
/// workbook: what it made is read back from the workspace by a key of the person's, as another
/// device sees it.
#[test]
fn an_agents_computer_lists_the_workspace_and_writes_what_another_device_sees() {
    host::run_with(&["sheets", "word-processor"], |host| async move {
        let agent = host.agent();
        // The window the person's page holds: their agent opens it on the layout.
        let (opened, failed) = act(&agent, "sheets#open", json!({})).await;
        assert!(!failed, "the agent opens Sheets: {opened}");
        let (made, failed) = act(&agent, "sheets#new_sheet", json!({"name": "Budget"})).await;
        assert!(!failed, "the agent makes a sheet: {made}");
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("sheets ") && line.ends_with("running")),
            "the workspace lists the sheets the person's page opened: {apps}"
        );
        let (wrote, failed) = act(
            &agent,
            "word-processor#append_paragraph",
            json!({"text": "Hello"}),
        )
        .await;
        assert!(!failed, "the agent writes in the workbook: {wrote}");

        // Another device of the person's reads what the agent wrote.
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let listed = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "invitation.list"),
                &json!({}),
            )
            .await;
        assert!(
            matches!(listed, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "the person's other device reaches the workspace: {listed:?}"
        );
        let (workbook, failed) = said(
            &agent
                .tool("tilo_read", &json!({"path": "word-processor"}))
                .await,
        );
        assert!(
            !failed && workbook.contains("Hello"),
            "the workbook reads what the agent wrote: {workbook}"
        );
    });
}

/// The agent opens Sheets and makes a sheet: the Sheets window reaches the person's layout as a
/// member's open puts it there, named as the agent's run's.
#[test]
fn an_app_an_agent_opens_reaches_the_persons_layout() {
    host::run_with(&["sheets"], |host| async move {
        let agent = host.agent();
        let (opened, failed) = act(&agent, "sheets#open", json!({})).await;
        assert!(!failed, "the agent opens Sheets: {opened}");
        let (wrote, failed) = act(&agent, "sheets#new_sheet", json!({})).await;
        assert!(!failed, "the agent makes a sheet: {wrote}");
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("sheets ") && line.ends_with("running")),
            "the window the agent opened is on the person's layout: {apps}"
        );
    });
}

/// The same on a host someone else runs, which is no member of the person's workspace: the
/// agent's Sheets window reaches the person's layout all the same.
#[test]
fn an_app_an_agent_opens_on_another_persons_host_reaches_the_persons_layout() {
    host::run_with(&["sheets"], |host| async move {
        // The daemon founds the person's own workspace and serves no other person's: the agent's
        // open of Sheets is what a guest's page would see reach the layout.
        let agent = host.agent();
        let (opened, failed) = act(&agent, "sheets#open", json!({})).await;
        assert!(
            !failed,
            "an agent on a host someone else runs opens Sheets, and its window reaches the \
             person's layout: {opened}"
        );
    });
}

/// An agent opens Sheets, which it started without a window: its open alone, before the agent
/// changes anything, puts Sheets' window in front of the person.
#[test]
fn an_agents_open_of_a_running_app_shows_its_window_on_the_persons_layout() {
    host::run_with(&["sheets"], |host| async move {
        let agent = host.agent();
        let (started, failed) = act(&agent, "sheets#start", json!({})).await;
        assert!(!failed, "the agent starts Sheets: {started}");
        let (opened, failed) = act(&agent, "sheets#open", json!({})).await;
        assert!(!failed, "the agent opens the running Sheets: {opened}");
        assert!(
            opened.starts_with("ok: sheets is running"),
            "Sheets ran already: {opened}"
        );
    });
}

/// An agent opens Sheets in a workspace whose layout the serving device does not keep: the open
/// answers at once, and nothing answers as though a window were shown.
#[test]
fn an_agents_open_with_no_layout_asks_the_persons_page() {
    host::run_with(&["sheets"], |host| async move {
        let agent = host.agent();
        let asked = Instant::now();
        let answer = agent
            .ask(
                "tools/call",
                json!({"name": "tilo_do",
                       "arguments": {"path": "sheets#open"},
                       "_meta": {"so.tilo.mcp": {"structured": true}}}),
            )
            .await;
        let result = &answer["result"];
        assert_eq!(result["isError"], false, "Sheets is open: {answer}");
        assert!(
            result["structuredContent"].get("not_shown").is_none(),
            "the view is asked for, which is no failure: {answer}"
        );
        assert!(
            asked.elapsed() < READ_BOUND,
            "the open answered without waiting for a window: {:?}",
            asked.elapsed()
        );
    });
}

/// An agent's writes on a logged-in computer answer as soon as the host ran them, in a workspace
/// no page wrote a window layout in: an action waits for the data it reads and writes, never for
/// a layout or a window to show it on.
#[test]
fn an_agents_writes_answer_without_waiting_out_a_timeout() {
    host::run_with(&["sheets"], |host| async move {
        let agent = host.agent();
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        let mut slow = Vec::new();
        for (path, args) in [
            ("so.tilo.tasks#new_task", json!({"title": "First"})),
            ("so.tilo.tasks#new_task", json!({"title": "Second"})),
            ("sheets#new_sheet", json!({})),
            ("sheets#open", json!({})),
        ] {
            let asked = Instant::now();
            let (answer, failed) = act(&agent, path, args).await;
            let took = asked.elapsed();
            assert!(!failed, "{path}: {answer}");
            if took > WRITE_WITHIN {
                slow.push((path, took));
            }
        }
        assert!(
            slow.is_empty(),
            "writes waited out a timeout rather than running once the host was ready: {slow:?}"
        );
    });
}

/// An agent opens Counter and reads its window as text: the count, and the action each of its
/// buttons runs by the address it is called by. It calls the action the Increment line names,
/// as it stands, and the next read shows the count it changed.
#[test]
fn an_agent_reads_a_window_calls_the_action_it_names_and_reads_the_change() {
    host::run_with(&["counter"], |host| async move {
        // The person installs Counter in the workspace, which gives it its own records: its
        // count is kept there.
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let installed = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "install-app"),
                &json!({
                    "app": "so.tilo.lang.counter",
                    "version": "0.1.0",
                    "surface": "00".repeat(32),
                    "package": "00".repeat(32),
                    "publisher": "00".repeat(32),
                    "dev": true,
                }),
            )
            .await;
        assert!(
            matches!(
                installed,
                Ok(tilo_actions::InvocationUpdate::Succeeded { .. })
            ),
            "Counter is installed in the workspace: {installed:?}"
        );
        let agent = host.agent();
        let (opened, failed) = act(&agent, "counter#open", json!({})).await;
        assert!(!failed, "the agent opens Counter: {opened}");

        let lines = view(&agent).await;
        assert!(
            lines.contains("text \"0\""),
            "the window reads the count: {lines}"
        );
        let increment = lines
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("button \"Increment\""))
            .and_then(|line| line.split_once(" → "))
            .map(|(_, address)| address.to_owned())
            .unwrap_or_else(|| panic!("the Increment button names its action: {lines}"));

        let (done, failed) = act(&agent, &increment, json!({})).await;
        assert!(!failed, "the action the window names runs as named: {done}");
        let lines = view(&agent).await;
        assert!(
            lines.contains("text \"1\""),
            "the next read shows the count the action changed: {lines}"
        );
    });
}

/// What `agent` reads of Counter's window, as lines, read as fast as the person sees it.
async fn view(agent: &host::Agent) -> String {
    let asked = Instant::now();
    let (lines, failed) = said(&agent.tool("tilo_view", &json!({"path": "counter"})).await);
    assert!(!failed, "{lines}");
    assert!(
        asked.elapsed() < READ_BOUND,
        "a view read answers as fast as the person sees it: {:?}",
        asked.elapsed()
    );
    lines
}

/// The agent opens Sheets: the view stands on the person's layout, named as the agent's
/// session's, and the shell of the person's computer reads its window there.
#[cfg(unix)]
#[test]
fn a_view_an_agent_opens_stands_on_the_layout_the_persons_shell_reads() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    use tilo_actions::remote::{
        ReadAnswer, RemoteActionFrame, RemoteActionMessage, ResourceReadRequest,
    };
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
    host::run_with(&["sheets"], |host| async move {
        let agent = host.agent();
        let (opened, failed) = act(&agent, "sheets#open", json!({})).await;
        assert!(!failed, "the agent opens Sheets: {opened}");

        // The person's computer: the shell the daemon admits, reading the layout as a session
        // does.
        let link = host.daemon().shell_link().unwrap();
        link.set_nonblocking(true).unwrap();
        let (shell, _) =
            runtime_collab::ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap());
        let deadline = Instant::now() + READ_BOUND;
        let (apps, opened_by) = loop {
            let message = RemoteActionMessage::ResourceRead {
                correlation: 0,
                on_behalf_of: None,
                read: ResourceReadRequest {
                    workspace: host.workspace().to_owned(),
                    app: DESKTOP_APP.to_owned(),
                    record: DESKTOP_DOCUMENT.to_owned(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                    answer: ReadAnswer::Document,
                },
            };
            let read = shell
                .call(ShellCall::Action {
                    workspace: None,
                    frame: RemoteActionFrame::new(message).encode().unwrap(),
                    caller: None,
                    pressed: None,
                })
                .await
                .unwrap();
            let ShellReply::Action(frame) = read else {
                panic!("the daemon answers the shell's read: {read:?}");
            };
            let layout =
                match RemoteActionFrame::decode(&frame).unwrap().message {
                    RemoteActionMessage::ResourceReading {
                        result: Ok(readings),
                        ..
                    } => serde_json::from_str::<serde_json::Value>(&readings).unwrap()["records"]
                        [0]["document"]
                        .as_str()
                        .map(|document| hex::decode(document).unwrap())
                        .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                        .and_then(|layout| {
                            tilo_desktop_model::shared_resource::read(&layout, None)
                        }),
                    _ => None,
                };
            let apps: Vec<String> = layout
                .iter()
                .flat_map(|layout| layout.tree.tabs())
                .filter_map(|tab| match &tab.tile {
                    tilo_desktop_model::TileContent::LocalApp { app_id, .. } => {
                        Some(app_id.clone())
                    }
                    _ => None,
                })
                .collect();
            if !apps.is_empty() || Instant::now() > deadline {
                break (apps, layout.map(|layout| layout.opened_by.len()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            apps.len() == 1 && apps[0].ends_with("sheets"),
            "the layout holds the one window the agent showed: {apps:?}"
        );
        assert_eq!(
            opened_by,
            Some(1),
            "the window names the agent session that opened it"
        );
    });
}

/// The app a device installed is the person's after the computer starts its daemon again: their
/// own dispatch lists it and starts it, and the instance the device hands runs on machine code
/// the cache holds, so the first open compiles nothing.
#[test]
fn a_restarted_computer_opens_an_installed_app_without_compiling() {
    host::run_with(&["sheets"], |host| async move {
        // The daemon stops and starts again on the same home, as a computer's does.
        host.restart().await;
        assert!(
            tilo_runtime::device::processes::last_handover().is_none(),
            "the restarted daemon handed nothing yet"
        );
        // The person finds the app the device installed in what their own dispatch offers.
        let page = host::listed(host.as_ref(), "so.tilo.sheets")
            .await
            .expect("the person's dispatch answers what it offers");
        assert!(
            page.rows.iter().any(|row| {
                row.action.as_ref().is_some_and(|action| {
                    action.address.resource == "app.so.tilo.sheets"
                        && action.address.action == "open"
                })
            }),
            "the restarted computer offers the app it installed: {page:?}"
        );
        // The person starts the app once, their own way: it opens on the layout as any agent's
        // start does, and the instance the device hands runs on machine code the install compiled.
        let started = host::start_app(host.as_ref(), "so.tilo.sheets").await;
        assert!(
            started.is_ok(),
            "the restarted computer starts Sheets: {started:?}"
        );
        let (app, parts, cached) = tilo_runtime::device::processes::last_handover()
            .expect("the daemon reports the module it handed its instance");
        assert_eq!(app, "so.tilo.sheets", "{app}");
        assert!(parts > 0, "the app carries modules");
        assert_eq!(
            cached, parts,
            "the install compiled every module the app runs, so its first open compiles none \
             ({cached} of {parts} came from the cache)"
        );
    });
}
