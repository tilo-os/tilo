//! The problems the editor shows, as a person reads them in the panel: the
//! lints of the file on screen beside whatever the language server said, per
//! file, and a refresh that changes no document.

use super::*;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use tilo_automerge::ApplyIncoming;

fn fixture() -> (CodeApp, AutoCommit, String) {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&doc);
    let key = app.buffer_key(app.session.active_buffer().unwrap().node);
    replace(&mut doc, &key, "fn main( {\n");
    doc.commit();
    (app, doc, key)
}
fn replace(doc: &mut AutoCommit, key: &str, text: &str) {
    let obj = EditorDoc::ensure_buffer_key(doc, key, "").unwrap();
    let len = doc.text(&obj).unwrap().chars().count();
    EditorDoc::splice(doc, &obj, 0, len, text);
}

/// The problems the panel shows, as the person reads them: the line each
/// stands on and what it says.
fn rows(app: &CodeApp) -> Vec<(usize, String)> {
    app.panel
        .problems()
        .iter()
        .map(|problem| (problem.line, problem.message.clone()))
        .collect()
}

fn says(rows: &[(usize, String)], words: &str) -> bool {
    rows.iter().any(|(_, message)| message.contains(words))
}

#[test]
fn a_files_problems_are_its_own_and_a_remote_edit_shows_in_them() {
    let (mut app, mut doc, _first) = fixture();
    let first = app.session.active_buffer().unwrap().node;
    app.refresh_diagnostics(&doc);
    let first_rows = rows(&app);
    assert!(
        says(&first_rows, "Unclosed"),
        "the open file's unclosed brace is shown: {first_rows:?}"
    );

    // Another file is clean, and the panel shows that file's problems, not the
    // one that was on screen before.
    let other = app
        .session
        .project
        .nodes
        .values()
        .find(|node| {
            matches!(node.kind, NodeKind::File { .. })
                && node.id != app.session.active_buffer().unwrap().node
        })
        .unwrap()
        .id;
    let other_key = app.buffer_key(other);
    replace(&mut doc, &other_key, "fn other() {}\n");
    doc.commit();
    app.session.open_file(other);
    app.refresh_diagnostics(&doc);
    assert!(
        !says(&rows(&app), "Unclosed"),
        "the clean file reports nothing left behind by the last one"
    );

    // Coming back shows the first file's problems again.
    app.session.open_file(first);
    app.refresh_diagnostics(&doc);
    assert_eq!(rows(&app), first_rows, "the first file's problems again");

    // A change that arrives from another device shows up too.
    let historical = doc.get_heads();
    let mut remote = doc.fork();
    replace(&mut remote, &other_key, "fn other( {\n");
    remote.commit();
    doc.merge_incoming(&mut remote).unwrap();
    app.session.open_file(other);
    app.refresh_diagnostics(&doc);
    assert!(
        says(&rows(&app), "Unclosed"),
        "the edited file's new problem is shown: {:?}",
        rows(&app)
    );

    // A file taken back to what it was reports what it reported then.
    doc = doc.fork_at(&historical).unwrap();
    app.refresh_diagnostics(&doc);
    assert!(
        !says(&rows(&app), "Unclosed"),
        "the rewound file is clean again"
    );
    assert_eq!(
        app.panel.problems(),
        lint::lint("fn other() {}\n", Language::Rust, lint::Scope::Full),
        "and says exactly what its own text says"
    );
}

#[test]
fn the_servers_findings_join_the_local_lints_and_leave_with_the_server() {
    let (mut app, doc, _) = fixture();
    app.refresh_diagnostics(&doc);
    assert!(says(&rows(&app), "Unclosed"), "{:?}", rows(&app));
    assert!(
        app.panel.analysis().is_none(),
        "no server has said anything"
    );

    // Once the server answers for this file, its findings stand beside the
    // local lints and the structural scan steps aside.
    let uri = "file:///cache.rs".to_string();
    app.lsp_uri = Some(uri.clone());
    app.lsp_diagnostics.insert(uri.clone(), vec![]);
    app.refresh_diagnostics(&doc);
    assert!(
        !says(&rows(&app), "Unclosed"),
        "the server's answer replaces the local structural note"
    );

    let message = serde_json::json!({"params": {"uri": uri, "diagnostics": [{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"message":"Fresh server result","severity":1}]}});
    let (_, diagnostics) = crate::lsp::parse_publish_diagnostics(&message).unwrap();
    app.lsp_diagnostics.insert(uri.clone(), diagnostics);
    app.shots_indexing();
    app.refresh_diagnostics(&doc);
    assert!(
        says(&rows(&app), "Fresh server result"),
        "the server's problem is shown: {:?}",
        rows(&app)
    );
    assert!(
        app.panel.analysis().is_some(),
        "the panel says what it is doing"
    );

    // With the server gone, its rows leave with it.
    app.lsp_status = LspStatus::None;
    app.lsp_diagnostics.insert(uri, vec![]);
    app.refresh_diagnostics(&doc);
    assert!(app.panel.analysis().is_none());
    assert!(
        !says(&rows(&app), "Fresh server result"),
        "no row outlives the server that said it"
    );
}

#[test]
fn refreshing_the_problems_changes_no_document() {
    use automerge::ActorId;

    let (mut app, mut doc, key) = fixture();
    let before = doc.get_heads();
    app.refresh_diagnostics(&doc);
    assert_eq!(
        doc.get_heads(),
        before,
        "showing problems writes nothing into the project"
    );
    assert_eq!(doc.pending_ops(), 0, "and leaves no transaction open");

    // The file still edits normally afterwards, and the panel follows it.
    doc.set_actor(ActorId::from(vec![0x20]));
    replace(&mut doc, &key, "fn main() {}\n");
    doc.commit();
    app.refresh_diagnostics(&doc);
    assert!(!says(&rows(&app), "Unclosed"), "the fixed file is clean");
    assert_eq!(
        EditorDoc::read_buffer_key(&doc, &key).as_deref(),
        Some("fn main() {}\n")
    );
    assert_eq!(doc.pending_ops(), 0);
}
