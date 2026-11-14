//! A folder of files opened in a hosted workspace, each in its own item of
//! the app that opens it: `<app>#open content=… name=…`, the one way the API,
//! `tilo files put`, an agent and a person's page open a file there.
//!
//! PDF Viewer and Writer keep one document per record, so two PDFs are two
//! records, each reading as its own PDF, where the second used to be refused
//! by the first one's record. Notes keeps every note in the notebook it
//! already has. A file the app does not open is refused before any record is
//! made for it.

#![allow(
    clippy::string_slice,
    reason = "an answer's ASCII prefix; a slip is the failure it reports"
)]

use super::hosted_documents::{content_in, form_pdf, put, tool, word_docx};
use super::*;
use serde_json::json;
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_core::remote::SessionActions;

/// The record `open` said the file opened in, as `<app>@<provider>`.
fn opened_in(answer: &str) -> String {
    let at = answer
        .find("opened in ")
        .unwrap_or_else(|| panic!("the open names no record: {answer}"));
    answer[at + "opened in ".len()..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Open the file `bytes` named `name` in `app`, and answer the record it is in.
async fn open_file(
    server: &McpServer<SessionActions>,
    app: &str,
    bytes: &[u8],
    name: &str,
) -> String {
    let content = content_in(&put(server, bytes).await);
    let answer = tool(
        server,
        "tilo_do",
        json!({"path": format!("{app}#open"), "args": {"content": content, "name": name}}),
    )
    .await;
    opened_in(&answer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_file_of_a_folder_opens_in_an_item_of_its_own() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let computer = enrol(&root, "uploading agent", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options::offering(crate::common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent);

    // Two PDFs: the second opens in a record of its own, not refused by the
    // first's.
    let visa = open_file(
        &server,
        "pdf-viewer",
        &form_pdf("Application for Schengen Visa"),
        "forms/visa.pdf",
    )
    .await;
    let permit = open_file(
        &server,
        "pdf-viewer",
        &form_pdf("Application for Residence Permit"),
        "forms/permit.pdf",
    )
    .await;
    assert_ne!(visa, permit, "each PDF has a record of its own");
    for (record, title) in [(&visa, "Schengen Visa"), (&permit, "Residence Permit")] {
        let text = tool(
            &server,
            "tilo_do",
            json!({"path": format!("{record}#read-text"), "args": {}}),
        )
        .await;
        assert!(text.contains(title), "{record} reads its own PDF: {text}");
    }

    // Two Word documents: two records of Writer, each holding its own words.
    let statement = open_file(
        &server,
        "word-processor",
        &word_docx("# Bank statement\n\nClosing balance: EUR 12,480.00\n"),
        "statement.docx",
    )
    .await;
    let letter = open_file(
        &server,
        "word-processor",
        &word_docx("# Cover letter\n\nI am applying for a visa.\n"),
        "letter.docx",
    )
    .await;
    assert_ne!(statement, letter, "each document has a record of its own");
    let read = tool(&server, "tilo_read", json!({"path": letter})).await;
    assert!(read.contains("Cover letter"), "{letter} reads: {read}");
    assert!(!read.contains("Bank statement"), "{letter} reads: {read}");

    // Notes keeps every note in its one notebook.
    let plan = open_file(&server, "notes", b"# Plan\n\n- book flights\n", "plan.md").await;
    let todo = open_file(&server, "notes", b"pack the passport\n", "todo.txt").await;
    assert_eq!(plan, todo, "notes go into the notebook the workspace keeps");

    // A PDF given to Writer is refused, naming the app that opens it, and
    // leaves no empty document behind.
    let content = content_in(&put(&server, &form_pdf("Stray form")).await);
    let refused = hosted_documents::ask(
        &server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "tilo_do",
            "arguments": {"path": "word-processor#open", "args": {"content": content, "name": "stray.pdf"}}}}),
    )
    .await
    .to_string();
    assert!(refused.contains("pdf-viewer opens it"), "{refused}");
    let listed = tool(&server, "tilo_ls", json!({"path": "word-processor"})).await;
    assert!(
        !listed.contains("stray"),
        "no record is made for a file the app refuses: {listed}"
    );
}
