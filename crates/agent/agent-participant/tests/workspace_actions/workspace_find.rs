//! An agent finding what a workspace holds by its words, as `tilo mcp` serves
//! `tilo_find` on the person's computer: the host ranks every resource its
//! records' apps tell search about, so a note is found by its title and by
//! what is written in it, and the answer names the note itself, by a path
//! `tilo_read` takes, not the notebook it is in.
use super::own_agent::listed_resources;
use super::*;
use serde_json::{Value, json};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_core::remote::SessionActions;

const NOTES: &str = "so.tilo.notes";

/// The text a tool call answers; a failed call fails the test.
async fn call(server: &McpServer<SessionActions>, name: &str, arguments: Value) -> String {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": name, "arguments": arguments}});
    let answer = server.handle(request).await.expect("answered");
    let text = answer["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let failed = answer["result"]["isError"] == true || answer.get("error").is_some();
    assert!(!failed, "{name} {arguments}: {text} {answer}");
    text
}

/// The path of the note titled `title` in `notebook`, as the notebook lists it.
async fn note_path(server: &McpServer<SessionActions>, notebook: &str, title: &str) -> String {
    let pages = call(
        server,
        "tilo_ls",
        json!({"path": format!("notes/{notebook}")}),
    )
    .await;
    let page = listed_resources(&pages)
        .into_iter()
        .find(|page| {
            pages
                .lines()
                .any(|line| line.contains(page.as_str()) && line.contains(title))
        })
        .unwrap_or_else(|| panic!("the notebook lists {title}: {pages}"));
    format!("notes/{notebook}/{page}")
}

/// Write `text` into the body of the note at `note`.
async fn write_body(server: &McpServer<SessionActions>, note: &str, text: &str) {
    let parts = call(server, "tilo_ls", json!({"path": note})).await;
    let body = listed_resources(&parts)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the note lists its body: {parts}"));
    call(
        server,
        "tilo_do",
        json!({"path": format!("{note}/{body}#append-paragraph"), "args": {"text": text}}),
    )
    .await;
}

/// `tilo_find`'s lines for `query`, and how long it took to answer.
async fn find(server: &McpServer<SessionActions>, query: &str) -> (Vec<String>, Duration) {
    let asked = Instant::now();
    let text = call(server, "tilo_find", json!({"query": query})).await;
    (text.lines().map(str::to_owned).collect(), asked.elapsed())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_finds_a_note_by_its_title_and_by_its_words_and_reads_that_note() {
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
    browser.create(NOTES);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::run_as_agent(&agent.peer, "Claude Code").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let server = McpServer::new(actions, WORKSPACE.into(), person)
        .with_surface(Surface::Agent)
        .with_agent_run("run-find", "Claude Code");

    // Two notes, as the walk-through left them: one the agent made and the
    // person wrote in, and a file that landed as a note named by its path.
    for title in ["Hello from Claude Code", "files/notes/handbook.md"] {
        call(
            &server,
            "tilo_do",
            json!({"path": "notes#new-note", "args": {"title": title}}),
        )
        .await;
    }
    let listing = call(&server, "tilo_ls", json!({"path": "notes"})).await;
    let notebook = listed_resources(&listing)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("notes lists a notebook: {listing}"));
    let hello = note_path(&server, &notebook, "Hello from Claude Code").await;
    write_body(&server, &hello, "Payment terms: net 30.").await;
    let handbook = note_path(&server, &notebook, "handbook").await;
    write_body(&server, &handbook, "Welcome to the team.").await;

    // By what is written in it: the note holding the words, named by the
    // path the notebook lists it at, with its title, ahead of the notebook.
    let (lines, took) = find(&server, "net 30").await;
    let first = lines.first().cloned().unwrap_or_default();
    assert!(
        first.starts_with(&format!("{hello}  ")),
        "the note holding net 30 leads: {lines:?}"
    );
    eprintln!("tilo_find \"net 30\" answered in {took:?}: {lines:?}");
    let path = first.split("  ").next().unwrap_or_default().to_owned();
    let read = call(&server, "tilo_read", json!({"path": path})).await;
    assert!(
        read.contains("net 30"),
        "the found path reads the note: {read}"
    );

    // By its title, which holds slashes: named plainly, its title shown
    // whole beside the path, and nothing escaped.
    let (lines, took) = find(&server, "handbook").await;
    eprintln!("tilo_find \"handbook\" answered in {took:?}: {lines:?}");
    let line = lines
        .iter()
        .find(|line| line.ends_with("files/notes/handbook.md"))
        .unwrap_or_else(|| panic!("the handbook note is found by its title: {lines:?}"));
    assert!(!line.contains('\\'), "nothing is escaped: {line}");
    let path = line.split("  ").next().unwrap_or_default();
    assert!(
        path.starts_with(&format!("notes/{notebook}/")),
        "the note itself is named, not its notebook: {line}"
    );
    let read = call(&server, "tilo_read", json!({"path": path})).await;
    assert!(read.contains("Welcome to the team."), "{read}");

    // A word no note holds finds none of them.
    let (lines, _) = find(&server, "quarterly forecast").await;
    assert!(
        !lines
            .iter()
            .any(|line| line.contains(&hello) || line.contains("handbook")),
        "{lines:?}"
    );
}
