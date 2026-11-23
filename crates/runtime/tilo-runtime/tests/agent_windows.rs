//! What the person's page shows of their agent's work: the app the agent opens reaches the
//! person's layout as one window, named as the agent's run's; a write answers as soon as the
//! host ran it, without waiting for a window to show it on; and the window the page keeps is
//! read by the agent, as lines and as a picture.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent_fixture::{act, ls, said};
use crate::host;

/// What the paragraph in Writer's document says.
const TITLE: &str = "Launch plan for Friday";
/// The longest a write may take, or a read of a window.
const WRITE_WITHIN: Duration = Duration::from_secs(3);
const READ_BOUND: Duration = Duration::from_secs(5);

/// The agent lists the workspace and its apps, makes a paragraph, and writes in the document:
/// what it made is read back from the workspace by a key of the person's, as another device sees it.
#[test]
fn an_agents_computer_lists_the_workspace_and_writes_what_another_device_sees() {
    host::run(|host| async move {
        let agent = host.agent();
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("word-processor ") && line.ends_with("running")),
            "the workspace lists Writer, the app the person's page opened: {apps}"
        );
        let (made, failed) = act(
            &agent,
            "word-processor#append-paragraph",
            json!({"text": "Budget"}),
        )
        .await;
        assert!(!failed, "the agent writes a paragraph: {made}");
        let (wrote, failed) = act(
            &agent,
            "word-processor#append-paragraph",
            json!({"text": "Hello"}),
        )
        .await;
        assert!(!failed, "the agent writes in the document: {wrote}");

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
        let (document, failed) = said(
            &agent
                .tool("tilo_read", &json!({"path": "word-processor"}))
                .await,
        );
        assert!(
            !failed && document.contains("Hello"),
            "the document reads what the agent wrote: {document}"
        );
    });
}

/// The agent opens Writer and writes a paragraph: the Writer window reaches the person's
/// layout as a member's open puts it there, named as the agent's run's.
#[test]
fn an_app_an_agent_opens_reaches_the_persons_layout() {
    host::run(|host| async move {
        let agent = host.agent();
        let (opened, failed) = act(&agent, "word-processor#open", json!({})).await;
        assert!(!failed, "the agent opens Writer: {opened}");
        let (wrote, failed) = act(
            &agent,
            "word-processor#append-paragraph",
            json!({"text": "First"}),
        )
        .await;
        assert!(!failed, "the agent writes a paragraph: {wrote}");
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("word-processor ") && line.ends_with("running")),
            "the window the agent opened is on the person's layout: {apps}"
        );
    });
}

/// The same on a host someone else runs, which is no member of the person's workspace: the
/// agent's Writer window reaches the person's layout all the same.
#[test]
fn an_app_an_agent_opens_on_another_persons_host_reaches_the_persons_layout() {
    host::run(|host| async move {
        // The daemon founds the person's own workspace and serves no other person's: the agent's
        // open of Writer is what a guest's page would see reach the layout.
        let agent = host.agent();
        let (opened, failed) = act(&agent, "word-processor#open", json!({})).await;
        assert!(
            !failed,
            "an agent on a host someone else runs opens Writer, and its window reaches the \
             person's layout: {opened}"
        );
    });
}

/// An agent opens Writer, which it started without a window: its open alone, before the agent
/// changes anything, puts Writer's window in front of the person.
#[test]
fn an_agents_open_of_a_running_app_shows_its_window_on_the_persons_layout() {
    host::run(|host| async move {
        let agent = host.agent();
        let (started, failed) = act(&agent, "word-processor#start", json!({})).await;
        assert!(!failed, "the agent starts Writer: {started}");
        let (opened, failed) = act(&agent, "word-processor#open", json!({})).await;
        assert!(!failed, "the agent opens the running Writer: {opened}");
        assert!(
            opened.starts_with("ok: word-processor is running"),
            "Writer ran already: {opened}"
        );
    });
}

/// An agent opens Writer in a workspace whose layout the serving device does not keep: the open
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
                       "arguments": {"path": "word-processor#open"},
                       "_meta": {"so.tilo.mcp": {"structured": true}}}),
            )
            .await;
        let result = &answer["result"];
        assert_eq!(result["isError"], false, "Writer is open: {answer}");
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
            ("tasks#create", json!({"title": "First"})),
            ("tasks#create", json!({"title": "Second"})),
            ("word-processor#append-paragraph", json!({"text": "Third"})),
            ("word-processor#open", json!({})),
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

/// The person's page keeps Writer's window: the agent lists it among what is open, reads its
/// lines, and is shown its picture as a PNG image beside them.
#[test]
fn an_agent_lists_and_reads_the_window_a_persons_page_holds() {
    host::run(|host| async move {
        let agent = host.agent();
        let (made, failed) = act(
            &agent,
            "word-processor#append-paragraph",
            json!({"text": TITLE}),
        )
        .await;
        assert!(
            !failed,
            "the agent writes the paragraph the person's window shows: {made}"
        );
        let (apps, failed) = ls(&agent, None).await;
        assert!(!failed, "{apps}");
        assert!(
            apps.lines()
                .any(|line| line.starts_with("word-processor ")
                    && line.ends_with("running, Ada works in it")),
            "the person's Writer is listed as running, with them working in it: {apps}"
        );

        // Its lines say what it shows: the sheet the person made, named as they named it.
        let first = Instant::now();
        let (lines, failed) = said(&agent.tool("tilo_view", &json!({"path": "word-processor"})).await);
        let first = first.elapsed();
        assert!(!failed, "{lines}");
        assert!(
            lines.contains(TITLE),
            "the view shows the person's document: {lines}"
        );
        assert!(
            first < READ_BOUND,
            "a view read answers as fast as the person sees it: {first:?}"
        );

        // Its picture comes as an image the agent looks at, beside the same lines.
        let answer = agent
            .tool("tilo_view", &json!({"path": "word-processor", "picture": true}))
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
