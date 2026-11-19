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

/// Notes, the app whose window the person's page holds.
const NOTES: &str = "so.tilo.notes";
/// What the note in it says, which the agent must read off the window.
const TITLE: &str = "Launch plan for Friday";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_lists_and_reads_the_window_a_persons_page_holds() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let home = enrol(&root, "home-server", DeviceCapabilities::member());
    let page_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = start(&root, &home, state.path()).await;

    // The person's page opens Notes.
    let mut page = Browser::open(&served, &page_device, &home).await;
    page.put_layout();
    let _notes = page.create(NOTES);
    page.peer.barrier().unwrap().recv().unwrap().unwrap();

    // Their agent, on a computer of theirs, served over that computer's own session.
    let agent = Browser::open(&served, &computer, &home).await;
    let _ = tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let actions = SessionActions::new(&agent.peer, agent.workspace, home.cert.device_public_key);
    let mcp = McpServer::new(actions, WORKSPACE.into(), person).with_agent_session("views");

    // A note is written in the person's Notes, the one their page shows.
    let (made, failed) = tool(
        &mcp,
        "tilo_do",
        json!({"path": "notes#new-note", "args": {"title": TITLE}}),
    )
    .await;
    assert!(!failed, "{made}");

    // The window is among what is open.
    let (apps, failed) = tool(&mcp, "tilo_ls", json!({})).await;
    assert!(!failed, "{apps}");
    assert!(
        apps.lines()
            .any(|line| line.starts_with("notes ") && line.ends_with("running")),
        "the person's Notes is listed as running: {apps}"
    );

    // Its lines say what it shows: the note the person wrote, as a control the agent can name.
    let (lines, failed) = tool(&mcp, "tilo_view", json!({"path": "notes"})).await;
    assert!(!failed, "{lines}");
    assert!(
        lines.contains(TITLE),
        "the view shows the person's note: {lines}"
    );

    // Its picture comes as an image, a PNG the agent looks at, beside the same lines.
    let answer = mcp
        .handle(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": {"name": "tilo_view",
                                  "arguments": {"path": "notes", "picture": true}}}))
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
}
