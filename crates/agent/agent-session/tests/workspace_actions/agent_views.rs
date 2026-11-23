//! What a person's window shows, read by their agent on a computer of theirs (`tilo login`, then
//! `tilo mcp`): the window is the person's page's, on the workspace's layout, and the agent
//! reaches it only through its computer's session with the workspace's server. The agent lists
//! it among what is open, reads its lines, and is shown its picture as an image it can look at,
//! not an address it would have to download and decode first.
use super::agent_mcp::tool;
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;

/// Sheets, the app whose window the person's page holds.
const SHEETS: &str = "so.tilo.sheets";
/// What the note in it says, which the agent must read off the window.
const TITLE: &str = "Launch plan for Friday";
/// The longest a read of a window may take, the first one included. Debug builds on a shared
/// test machine draw several times slower than a release, so the bound allows for that.
const READ_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_lists_and_reads_the_window_a_persons_page_holds() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let page_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = start(&root, &home, state.path()).await;

    // The person's page opens Sheets.
    let mut page = Browser::open(&served, &page_device, &home).await;
    page.put_layout();
    let notes = page.create(SHEETS);
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();

    // Their agent, on a computer of theirs, served over that computer's own session.
    let agent = Browser::open(&served, &computer, &home).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let actions = SessionActions::new(&agent.peer, agent.workspace, home.cert.device_public_key);
    let mcp = McpServer::for_agent(actions, WORKSPACE.into(), "claude-code", &person)
        .with_agent_session("views");

    // A sheet is made in the person's Sheets, the one their page shows.
    let (made, failed) = tool(
        &mcp,
        "tilo_do",
        json!({"path": "sheets#new-sheet", "args": {"name": TITLE}}),
    )
    .await;
    assert!(!failed, "{made}");

    // The person works in that window: their page says so on the workspace's presence, as
    // every page does.
    let mut shell = tilo_session_client::presence::view(
        1,
        tilo_clock::unix_now_ms(),
        "Ada",
        [1, 2, 3],
        None,
        Some(&tilo_server_protocol::live::LiveResource {
            app: SHEETS.into(),
            record: notes.document.clone(),
        }),
        None,
    );
    shell.agent = false;
    let said = collab_types::presence::Presence::desktop(
        notes.cluster_id.clone(),
        collab_types::presence::Payload::Shell(Box::new(shell)),
    );

    // The window is among what is open, with the person working in it.
    let deadline = tilo_test_env::hang_deadline();
    let apps = loop {
        page.peer
            .send(tilo_session_client::Cmd::Presence(
                page.workspace,
                vec![said.clone()],
            ))
            .unwrap();
        let (apps, failed) = tool(&mcp, "tilo_ls", json!({})).await;
        assert!(!failed, "{apps}");
        if apps.contains("Ada works in it") || std::time::Instant::now() > deadline {
            break apps;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert!(
        apps.lines()
            .any(|line| line.starts_with("sheets ") && line.ends_with("running, Ada works in it")),
        "the person's Sheets is listed as running, with them working in it: {apps}"
    );

    // Its lines say what it shows: the sheet the person made, named as they named it.
    let first = std::time::Instant::now();
    let (lines, failed) = tool(&mcp, "tilo_view", json!({"path": "sheets"})).await;
    let first = first.elapsed();
    assert!(!failed, "{lines}");
    assert!(
        lines.contains(TITLE),
        "the view shows the person's sheet: {lines}"
    );
    // A read of a window answers about as fast as the person sees it: the first one included,
    // and every one after it.
    let again = std::time::Instant::now();
    let (_, failed) = tool(&mcp, "tilo_view", json!({"path": "sheets"})).await;
    let again = again.elapsed();
    assert!(!failed);
    eprintln!("tilo_view sheets: first {first:?}, again {again:?}");
    assert!(
        first < READ_BOUND && again < READ_BOUND,
        "a view read answers within {READ_BOUND:?}: first {first:?}, again {again:?}"
    );

    // Its picture comes as an image, a PNG the agent looks at, beside the same lines.
    let answer = mcp
        .handle(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": {"name": "tilo_view",
                                  "arguments": {"path": "sheets", "picture": true}}}))
        .await
        .expect("the view is answered");
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
    let png = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().unwrap_or_default())
            .expect("the image is base64")
    };
    assert!(
        png.starts_with(b"\x89PNG\r\n\x1a\n") && png.len() > 1_000,
        "a picture of the window, {} bytes",
        png.len()
    );
    assert!(
        blocks.iter().any(|block| block["text"]
            .as_str()
            .is_some_and(|text| text.contains(TITLE))),
        "the lines come with the picture: {answer}"
    );

    // Chat, opened on the page too: its conversations are read once each, and an empty one
    // says what the person sees under it.
    let _chat = page.create("so.tilo.chat");
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();
    let (lines, failed) = tool(&mcp, "tilo_view", json!({"path": "chat"})).await;
    assert!(!failed, "{lines}");
    let items: Vec<&str> = lines
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("tree_item "))
        .collect();
    assert!(!items.is_empty(), "the conversations are listed: {lines}");
    for item in &items {
        let name = item.split('"').nth(1).unwrap_or_default();
        assert_eq!(
            lines.matches(&format!("\"{name}\"")).count(),
            1,
            "{name} is read once: {lines}"
        );
    }
    assert!(
        lines.contains("\"No messages yet\"") && lines.contains("sees it."),
        "the empty conversation's title and the line under it are read: {lines}"
    );
}
