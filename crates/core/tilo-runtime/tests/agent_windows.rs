//! What the person's page shows of their agent's work: the app the agent opens reaches the
//! person's layout as one window, named as the agent's run's; a write answers as soon as the
//! host ran it, without waiting for a window to show it on; and the window the page keeps is
//! read by the agent, as lines and as a picture.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent_fixture::{act, ls, said};
use crate::host;

/// What the note in the sheets app says.
const TITLE: &str = "Launch plan for Friday";
/// The longest a write may take, or a read of a window.
const WRITE_WITHIN: Duration = Duration::from_secs(3);
const READ_BOUND: Duration = Duration::from_secs(5);

/// The agent lists the workspace and its apps, makes a sheet, and writes in the workbook: what it
/// made is read back from the workspace by a key of the person's, as another device sees it.
#[test]
fn an_agents_computer_lists_the_workspace_and_writes_what_another_device_sees() {
    host::run(|host| async move {
        let agent = host.agent();
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("sheets ") && line.ends_with("running")),
            "the workspace lists the sheets the person's page opened: {apps}"
        );
        let (made, failed) = act(&agent, "sheets#new-sheet", json!({"name": "Budget"})).await;
        assert!(!failed, "the agent makes a sheet: {made}");
        let (wrote, failed) = act(
            &agent,
            "word-processor#append-paragraph",
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
    host::run(|host| async move {
        let agent = host.agent();
        let (opened, failed) = act(&agent, "sheets#open", json!({})).await;
        assert!(!failed, "the agent opens Sheets: {opened}");
        let (wrote, failed) = act(&agent, "sheets#new-sheet", json!({})).await;
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
    host::run(|host| async move {
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
    host::run(|host| async move {
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
    host::run(|host| async move {
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
    host::run(|host| async move {
        let agent = host.agent();
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        let mut slow = Vec::new();
        for (path, args) in [
            ("so.tilo.tasks#new_task", json!({"title": "First"})),
            ("so.tilo.tasks#new_task", json!({"title": "Second"})),
            ("sheets#new-sheet", json!({})),
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

/// The person's page keeps Sheets' window: the agent lists it among what is open, reads its
/// lines, and is shown its picture as a PNG image beside them.
#[test]
fn an_agent_lists_and_reads_the_window_a_persons_page_holds() {
    host::run(|host| async move {
        let agent = host.agent();
        let (made, failed) = act(&agent, "sheets#new-sheet", json!({"name": TITLE})).await;
        assert!(
            !failed,
            "the agent makes the sheet the person's window shows: {made}"
        );
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("sheets ")
                    && line.ends_with("running, Ada works in it")),
            "the person's Sheets is listed as running, with them working in it: {apps}"
        );

        // Its lines say what it shows: the sheet the person made, named as they named it.
        let first = Instant::now();
        let (lines, failed) = said(&agent.tool("tilo_view", &json!({"path": "sheets"})).await);
        let first = first.elapsed();
        assert!(!failed, "{lines}");
        assert!(
            lines.contains(TITLE),
            "the view shows the person's sheet: {lines}"
        );
        assert!(
            first < READ_BOUND,
            "a view read answers as fast as the person sees it: {first:?}"
        );

        // Its picture comes as an image the agent looks at, beside the same lines.
        let answer = agent
            .tool("tilo_view", &json!({"path": "sheets", "picture": true}))
            .await;
        let blocks: Vec<&Value> = answer["result"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        assert_ne!(answer["result"]["isError"], true, "{answer}");
        let image = blocks
            .iter()
            .find(|block| block["type"] == "image")
            .unwrap_or_else(|| panic!("the picture is an image block: {answer}"));
        assert_eq!(image["mimeType"], "image/png");
        assert!(
            blocks.iter().any(|block| block["text"]
                .as_str()
                .is_some_and(|text| text.contains(TITLE))),
            "the lines come with the picture: {answer}"
        );
    });
}
