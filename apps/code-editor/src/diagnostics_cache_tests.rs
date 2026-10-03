use super::*;
use automerge::ReadDoc;
use automerge::transaction::Transactable;
use tilo_automerge::ApplyIncoming;

fn fixture() -> (CodeApp, AutoCommit, String) {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
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
#[test]
fn diagnostics_cache_reuses_only_identical_committed_inputs() {
    // Each phase builds a fresh sample fixture; read counts must not leak between phases.
    unchanged_diagnostics_reuse_local_lint();
    pending_edits_never_cache_or_commit();
    heads_language_buffer_and_lsp_scope_invalidate_but_lsp_payload_does_not();
    equal_pending_counts_with_different_text_never_alias();
}

fn unchanged_diagnostics_reuse_local_lint() {
    let (mut app, doc, _) = fixture();
    app.refresh_diagnostics(&doc);
    let expected = app.panel.problems().to_vec();
    for _ in 0..10 {
        app.refresh_diagnostics(&doc);
    }
    assert_eq!(app.diagnostics_buffer_reads, 1);
    assert_eq!(app.panel.problems(), expected);
    // The uncached path preserves the prior read/lint behavior and provides
    // a deterministic work-count comparison on identical authoritative text.
    let (mut baseline, baseline_doc, _) = fixture();
    for _ in 0..11 {
        baseline.refresh_diagnostics_at(&baseline_doc, None);
    }
    assert_eq!(baseline.diagnostics_buffer_reads, 11);
    assert_eq!(baseline.panel.problems(), expected);
}
fn pending_edits_never_cache_or_commit() {
    let (mut app, mut doc, key) = fixture();
    app.refresh_diagnostics(&doc);
    for text in ["TODO first", "TODO other"] {
        replace(&mut doc, &key, text);
        let pending = doc.pending_ops();
        assert!(pending > 0);
        app.refresh_diagnostics(&doc);
        app.refresh_diagnostics(&doc);
        assert_eq!(doc.pending_ops(), pending);
        assert!(app.diagnostics_cache.is_none());
    }
    assert_eq!(app.diagnostics_buffer_reads, 5);
    doc.commit();
    app.refresh_diagnostics(&doc);
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 6);
}
fn heads_language_buffer_and_lsp_scope_invalidate_but_lsp_payload_does_not() {
    let (mut app, mut doc, key) = fixture();
    app.refresh_diagnostics(&doc);
    assert!(
        app.panel
            .problems()
            .iter()
            .any(|d| d.message.contains("Unclosed"))
    );
    let uri = "file:///cache.rs".to_string();
    app.lsp_uri = Some(uri.clone());
    app.lsp_diagnostics.insert(uri.clone(), vec![]);
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 2);
    assert!(
        !app.panel
            .problems()
            .iter()
            .any(|d| d.message.contains("Unclosed"))
    );
    let message = serde_json::json!({"params": {"uri": uri, "diagnostics": [{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"message":"Fresh server result","severity":1}]}});
    let (_, diagnostics) = crate::lsp::parse_publish_diagnostics(&message).unwrap();
    app.lsp_diagnostics.insert(uri.clone(), diagnostics);
    app.shots_indexing();
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 2);
    assert!(
        app.panel
            .problems()
            .iter()
            .any(|d| d.message == "Fresh server result")
    );
    assert!(app.panel.analysis().is_some());
    app.lsp_status = LspStatus::None;
    app.lsp_diagnostics.insert(uri, vec![]);
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 2);
    assert!(app.panel.analysis().is_none());
    assert!(
        !app.panel
            .problems()
            .iter()
            .any(|d| d.message == "Fresh server result")
    );
    let active = app.session.active.unwrap();
    app.session.open[active].language = Language::Python;
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 3);
    let other = app
        .session
        .project
        .nodes
        .values()
        .find(|n| matches!(n.kind, NodeKind::File { .. }) && n.id != app.session.open[active].node)
        .unwrap()
        .id;
    app.session.open_file(other);
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 4);
    app.session.active = Some(active);
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 5);
    let historical = doc.get_heads();
    let mut remote = doc.fork();
    replace(&mut remote, &key, "TODO remote\n");
    remote.commit();
    doc.merge_incoming(&mut remote).unwrap();
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 6);
    let expected = lint::lint(
        &EditorDoc::read_buffer_key(&doc, &key).unwrap(),
        Language::Python,
        lint::Scope::TextualOnly,
    );
    assert_eq!(app.panel.problems(), expected);
    // A restored historical checkout has different heads even in this same widget.
    doc = doc.fork_at(&historical).unwrap();
    app.refresh_diagnostics(&doc);
    assert_eq!(app.diagnostics_buffer_reads, 7);
}

fn equal_pending_counts_with_different_text_never_alias() {
    let (mut app, mut doc, key) = fixture();
    app.refresh_diagnostics(&doc);
    let mut first = doc.fork();
    let mut second = doc.fork();
    replace(&mut first, &key, "TODO one");
    replace(&mut second, &key, "okay two");
    assert_eq!(first.pending_ops(), second.pending_ops());
    let count = first.pending_ops();
    app.refresh_diagnostics(&first);
    let first_rows = app.panel.problems().to_vec();
    app.refresh_diagnostics(&second);
    assert_ne!(first_rows, app.panel.problems());
    assert_eq!(first.pending_ops(), count);
    assert_eq!(second.pending_ops(), count);
    assert_eq!(app.diagnostics_buffer_reads, 3);
}

#[test]
fn diagnostics_leave_transactions_history_and_patch_cursor_untouched() {
    // Each phase builds its own document and actor setup.
    diagnostics_leave_zero_operation_transactions_and_patch_history_untouched();
    diagnostics_preserve_active_patch_cursor();
}

fn diagnostics_leave_zero_operation_transactions_and_patch_history_untouched() {
    use automerge::{ActorId, ROOT};
    let mut app = CodeApp::new();
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(vec![0x80]));
    doc.put(ROOT, "body", "seed").unwrap();
    doc.commit();
    let before = doc.get_heads();
    doc.set_actor(ActorId::from(vec![0x10]));
    doc.delete(ROOT, "absent").unwrap();
    assert_eq!(doc.pending_ops(), 0);
    let untouched = format!("{doc:?}");
    app.refresh_diagnostics(&doc);
    assert_eq!(
        format!("{doc:?}"),
        untouched,
        "diagnostics must not close the empty transaction or alter patch/history state"
    );
    assert!(doc.diff_cursor().is_empty());
    // The transaction's owner discards its empty transaction. Diagnostics
    // must not have committed it first and poisoned the actor table.
    assert_eq!(doc.rollback(), 0);
    // Match the owner's safe empty-transaction cleanup in tilo_resource::history.
    doc.reset_diff_cursor();
    doc.set_actor(ActorId::from(vec![0x20]));
    doc.put(ROOT, "body", "edited").unwrap();
    doc.commit();
    let after = doc.get_heads();
    let patches = doc.diff(&before, &after);
    assert!(matches!(patches.as_slice(), [automerge::Patch {
        action: automerge::PatchAction::PutMap { key, value, .. }, ..
    }] if key == "body" && value.0.as_str() == Some("edited")));
}

#[test]
#[ignore = "manual cost comparison, not a timing correctness assertion"]
fn diagnostics_clone_witness_cost() {
    let (mut app, mut doc, key) = fixture();
    let source = (0..2000).fold(String::new(), |mut source, i| {
        use std::fmt::Write as _;
        writeln!(source, "fn function_{i}() {{ let value = {i}; }}").unwrap();
        source
    });
    replace(&mut doc, &key, &source);
    doc.commit();
    let object = EditorDoc::buffer_text_key(&doc, &key).unwrap();
    let mut previous = 0;
    for history in [0, 100, 1000] {
        for i in previous..history {
            if i % 2 == 0 {
                EditorDoc::splice(&mut doc, &object, 0, 0, "x");
            } else {
                EditorDoc::splice(&mut doc, &object, 0, 1, "");
            }
            doc.commit();
        }
        previous = history;
        app.refresh_diagnostics(&doc);
        let expected = app.panel.problems().to_vec();
        let start = ui::time::now();
        for _ in 0..50 {
            app.refresh_diagnostics(&doc);
        }
        let cached = start.elapsed();
        assert_eq!(app.panel.problems(), expected);
        let start = ui::time::now();
        for _ in 0..50 {
            app.refresh_diagnostics_at(&doc, None);
        }
        let uncached = start.elapsed();
        assert_eq!(app.panel.problems(), expected);
        println!(
            "diagnostics-cost history={history} iterations=50 cached_us={} uncached_us={}",
            cached.as_micros(),
            uncached.as_micros()
        );
    }
}

fn diagnostics_preserve_active_patch_cursor() {
    use automerge::{ROOT, transaction::Transactable};
    let (mut app, mut doc, _) = fixture();
    doc.update_diff_cursor();
    let before = doc.diff_cursor();
    doc.delete(ROOT, "absent").unwrap();
    app.refresh_diagnostics(&doc);
    assert_eq!(doc.diff_cursor(), before);
    assert_eq!(doc.pending_ops(), 0);
    // Keep the transaction's actor: actor migration after rollback is an
    // independent upstream patch-log issue, outside diagnostic observation.
    doc.put(ROOT, "body", "edited").unwrap();
    doc.commit();
    let patches = doc.diff_incremental();
    assert!(matches!(patches.as_slice(), [automerge::Patch {
        action: automerge::PatchAction::PutMap { key, value, .. }, ..
    }] if key == "body" && value.0.as_str() == Some("edited")));
}
