use super::*;
use automerge::ReadDoc;

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

#[test]
fn a_standalone_project_import_is_submitted_through_the_action() {
    let mut app = CodeApp::with_sample();
    assert!(!app.actions.is_hosted());
    let mut doc = AutoCommit::new();
    app.import_project_file_records(&mut doc);
    let recorded = app
        .actions
        .last_dispatched()
        .expect("imported files are submitted through the action sink");
    assert_eq!(recorded.type_id.as_str(), actions::CODE_EDITOR_ACTION_TYPE);
    assert!(
        !EditorDoc::files(&doc).is_empty(),
        "the sample project is in the document"
    );
}

#[test]
fn a_standalone_project_is_created_by_the_action() {
    let mut app = CodeApp::new();
    assert!(!app.actions.is_hosted());
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    assert_eq!(
        EditorDoc::project_id(&doc).as_deref(),
        Some(app.project_id.as_str())
    );
    let recorded = app
        .actions
        .last_dispatched()
        .expect("the project is submitted through the action sink");
    assert_eq!(recorded.type_id.as_str(), actions::CODE_EDITOR_ACTION_TYPE);
}

#[test]
fn a_standalone_terminal_panel_is_created_by_the_action() {
    let mut app = CodeApp::new();
    assert!(!app.actions.is_hosted());
    let mut doc = AutoCommit::new();
    let id = app.terminal_document_root(&mut doc);
    assert_eq!(EditorDoc::terminal_panel(&doc), Some(id));
    let recorded = app
        .actions
        .last_dispatched()
        .expect("the terminal panel is submitted through the action sink");
    assert_eq!(recorded.type_id.as_str(), actions::CODE_EDITOR_ACTION_TYPE);
}

#[test]
fn hosted_terminal_panel_is_created_by_the_action() {
    let mut app = CodeApp::new();
    app.mark_hosted_for_test();
    let mut doc = AutoCommit::new();
    assert!(EditorDoc::terminal_panel(&doc).is_none());
    let id = app.terminal_document_root(&mut doc);
    assert_eq!(EditorDoc::terminal_panel(&doc), Some(id.clone()));
    assert_eq!(app.terminal_document_root(&mut doc), id);
}

#[test]
fn resource_operations_accept_reject_wrong_kinds_and_resolve_late_files_lazily() {
    // Each phase builds a fresh app and document.
    direct_open_is_accepted_then_reports_the_exact_failure();
    resource_operations_reject_wrong_kinds_without_changing_state();
    large_resource_snapshot_is_bounded_but_late_file_activates_lazily();
}

fn direct_open_is_accepted_then_reports_the_exact_failure() {
    let mut app = CodeApp::new();
    let mut doc = AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    let payload = serde_json::to_vec(&crate::actions::CodeEditorOperation::OpenFile {
        path: Some("/definitely/missing/tilo-code-file".into()),
    })
    .unwrap();
    let started = Widget::start_operation(
        &mut app,
        &ui::widget::ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::CODE_EDITOR_OPERATION_TYPE,
            &payload,
        ),
        &mut doc,
        &automerge::ROOT,
    )
    .unwrap()
    .unwrap();
    assert_eq!(started, ui::widget::ResourceOperationStart::Accepted);
    app.sync(&mut doc, &automerge::ROOT);
    assert!(matches!(
        app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
        Some(tilo_actions::InvocationUpdate::Failed { .. })
    ));
}

fn resource_operations_reject_wrong_kinds_without_changing_state() {
    let (mut app, mut doc) = synced_sample();
    let active = app.active_file_node();
    let other = app
        .session
        .project
        .nodes
        .values()
        .find(|node| Some(node.id) != active && matches!(node.kind, NodeKind::File { .. }))
        .expect("sample has another file")
        .id;
    let open = serde_json::to_vec(&crate::actions::CodeEditorOperation::Open {
        file_id: app.session.project.get(other).unwrap().resource_id.clone(),
    })
    .unwrap();
    assert!(
        Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                crate::actions::CODE_EDITOR_OPERATION_TYPE,
                &open
            ),
            &mut doc,
            &automerge::ROOT
        )
        .is_err()
    );
    assert_eq!(app.active_file_node(), active);

    let open_file = serde_json::to_vec(&crate::actions::CodeEditorOperation::OpenFile {
        path: Some("/tmp/wrong-kind.rs".into()),
    })
    .unwrap();
    assert!(
        Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                crate::actions::CODE_EDITOR_OPERATION_TYPE,
                &open_file
            ),
            &mut doc,
            &automerge::ROOT
        )
        .is_err()
    );
    assert!(app.pending_resource_file.is_none());
    assert!(app.resource_invocation.is_none());
}

#[test]
fn sample_app_opens_main_rs_with_accessible_context_and_a_collapsed_dock() {
    // Each phase builds a fresh sample app.
    new_app_opens_main_rs();
    accessibility_exposes_project_and_active_document_context();
    ensure_buffers_seeds_active_text();
    fresh_editor_collapses_the_empty_bottom_panel();
}

fn new_app_opens_main_rs() {
    let app = CodeApp::with_sample();
    let active = app.session.active_buffer().unwrap();
    assert_eq!(active.name, "main.rs");
}

fn accessibility_exposes_project_and_active_document_context() {
    let app = CodeApp::with_sample();
    let node = Widget::accessibility(&app).expect("Code has a semantic root");
    assert_eq!(node.role, ui::a11y::Role::Document);
    assert!(
        node.name
            .as_deref()
            .is_some_and(|name| name.starts_with("Code — "))
    );
    assert_eq!(node.value.as_deref(), Some("main.rs"));
    assert!(node.children.iter().any(|child| {
        child
            .name
            .as_deref()
            .is_some_and(|name| name.contains("open file"))
    }));
}

fn invoke_search_hit(app: &mut CodeApp, doc: &mut AutoCommit, query: &str, kind: &str) {
    let hit = ui::search::capability::search_providers(app)[0]
        .search(query)
        .into_iter()
        .find(|hit| hit.kind == kind)
        .expect("search hit");
    let address = hit.action_address().expect("canonical address");
    let resource = ui::resources::resource(app, doc, &address.resource)
        .expect("address resolves through current widget authority");
    let encoded = tilo_actions::door::encode(&[resource], &address, None)
        .expect("lazy resource exports the searched action");
    assert!(matches!(
        Widget::start_operation(
            app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload
            ),
            doc,
            &automerge::ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Completed(_))
    ));
    app.sync(doc, &automerge::ROOT);
}

#[test]
fn search_hits_open_through_exported_actions_and_stale_addresses_are_refused() {
    // Each phase builds a fresh synced sample.
    file_search_uses_the_exported_open_action();
    symbol_search_resolves_and_reproduces_the_exact_caret();
    project_text_search_hit_opens_the_file_at_its_line();
    stale_search_address_is_refused_by_current_widget_authority();
}

fn file_search_uses_the_exported_open_action() {
    let (mut app, mut doc) = synced_sample();
    let prime = app
        .session
        .project
        .nodes
        .values()
        .find(|node| node.name == "prime.rs")
        .unwrap()
        .id;
    invoke_search_hit(
        &mut app,
        &mut doc,
        "prime.rs",
        crate::search_provider::PROJECT_KIND,
    );
    assert_eq!(app.active_file_node(), Some(prime));
}

fn stale_search_address_is_refused_by_current_widget_authority() {
    let (mut app, mut doc) = synced_sample();
    let file = app
        .session
        .project
        .nodes
        .values()
        .find(|node| node.name == "prime.rs")
        .expect("sample file")
        .clone();
    let address = crate::actions::open_file_address(&file.resource_id);
    assert!(ui::resources::resource(&app, &doc, &address.resource).is_some());

    app.session
        .project
        .remove_collaborative_file(&file.resource_id);
    app.invalidate_search_projection();
    app.refresh_search_catalog(&mut doc);
    assert!(ui::resources::resource(&app, &doc, &address.resource).is_none());
    assert!(
        ui::search::capability::search_providers(&app)[0]
            .search("prime.rs")
            .into_iter()
            .all(|hit| hit.action_address() != Some(address.clone()))
    );
}

#[test]
fn search_projection_rebuilds_only_on_revisions_or_an_exhausted_generation() {
    // Each phase builds a fresh synced sample; rebuild counters must not leak.
    search_projection_rebuilds_only_for_document_project_or_result_revisions();
    exhausted_search_projection_generation_rebuilds_instead_of_reusing_a_cursor();
}

fn search_projection_rebuilds_only_for_document_project_or_result_revisions() {
    let (mut app, mut doc) = synced_sample();
    let initial = app.search_refresh_stats();
    app.sync(&mut doc, &automerge::ROOT);
    app.sync(&mut doc, &automerge::ROOT);
    let idle = app.search_refresh_stats();
    assert_eq!(idle.rebuilds, initial.rebuilds);
    assert_eq!(idle.buffer_reads, initial.buffer_reads);
    assert_eq!(idle.indexed_records, initial.indexed_records);
    assert!(idle.skipped >= initial.skipped.saturating_add(2));

    let node = app.active_file_node().expect("sample buffer");
    let key = app.buffer_key(node);
    let text = EditorDoc::buffer_text_key(&doc, &key).expect("buffer text");
    EditorDoc::splice(&mut doc, &text, 0, 0, "fn revision_gate_needle() {}\n");
    app.sync(&mut doc, &automerge::ROOT);
    let after_document = app.search_refresh_stats();
    assert_eq!(after_document.rebuilds, idle.rebuilds.saturating_add(1));
    assert!(
        ui::search::capability::search_providers(&app)[0]
            .search("revision_gate_needle")
            .iter()
            .any(|hit| hit.kind == crate::search_provider::SYMBOL_KIND)
    );

    let root = app.session.project.root;
    let late = app
        .session
        .project
        .add_file(root, "project-revision-needle.rs", "");
    app.invalidate_search_projection();
    app.sync(&mut doc, &automerge::ROOT);
    let after_project = app.search_refresh_stats();
    assert_eq!(
        after_project.rebuilds,
        after_document.rebuilds.saturating_add(1)
    );
    assert!(
        ui::search::capability::search_providers(&app)[0]
            .search("project-revision-needle")
            .iter()
            .any(|hit| {
                hit.action_address()
                    == Some(crate::actions::open_file_address(
                        &app.session
                            .project
                            .get(late)
                            .expect("late file")
                            .resource_id,
                    ))
            })
    );
}

fn exhausted_search_projection_generation_rebuilds_instead_of_reusing_a_cursor() {
    let (mut app, mut doc) = synced_sample();
    app.search_projection_local_generation = Some(u64::MAX);
    app.invalidate_search_projection();
    assert_eq!(app.search_projection_local_generation, None);
    let before = app.search_refresh_stats().rebuilds;
    app.refresh_search_catalog(&mut doc);
    app.refresh_search_catalog(&mut doc);
    assert_eq!(
        app.search_refresh_stats().rebuilds,
        before.saturating_add(2),
        "an exhausted generation must never make a stale catalogue look current"
    );
}

fn large_resource_snapshot_is_bounded_but_late_file_activates_lazily() {
    let mut app = CodeApp::new();
    let root = app.session.project.root;
    let mut late_id = String::new();
    for index in 0..=crate::actions::EAGER_RESOURCE_SNAPSHOT_LIMIT {
        let node = app
            .session
            .project
            .add_file(root, &format!("file-{index}.rs"), "");
        if index == crate::actions::EAGER_RESOURCE_SNAPSHOT_LIMIT {
            late_id = app
                .session
                .project
                .get(node)
                .expect("added file")
                .resource_id
                .clone();
        }
    }
    let mut doc = AutoCommit::new();
    app.refresh_search_catalog(&mut doc);
    let snapshot = ui::resources::resources(&app, &doc);
    assert!(
        snapshot[0].children.len() <= crate::actions::EAGER_RESOURCE_SNAPSHOT_LIMIT,
        "the eager tree must remain bounded by its resource cap"
    );

    let address = crate::actions::open_file_address(&late_id);
    assert!(
        tilo_actions::door::resolve(&snapshot, &address).is_none(),
        "the late file must not be eagerly materialized"
    );
    let resource = ui::resources::resource(&app, &doc, &address.resource)
        .expect("late file is still current widget authority");
    let encoded = tilo_actions::door::encode(&[resource], &address, None)
        .expect("the lazy resource retains its open action");
    assert!(matches!(
        Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload
            ),
            &mut doc,
            &automerge::ROOT
        )
        .expect("operation routes through the widget"),
        Some(ui::widget::ResourceOperationStart::Completed(_))
    ));
}

fn symbol_search_resolves_and_reproduces_the_exact_caret() {
    let (mut app, mut doc) = synced_sample();
    let node = app.active_file_node().unwrap();
    let text = EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).unwrap();
    let expected_line = text
        .lines()
        .position(|line| line.contains("fn parse_limit"))
        .unwrap();
    let expected_col = text
        .lines()
        .nth(expected_line)
        .unwrap()
        .find("parse_limit")
        .unwrap();

    invoke_search_hit(
        &mut app,
        &mut doc,
        "parse_limit",
        crate::search_provider::SYMBOL_KIND,
    );
    assert_eq!(app.active_file_node(), Some(node));
    assert_eq!(app.editor.primary_caret(), (expected_line, expected_col));
}

fn project_text_search_hit_opens_the_file_at_its_line() {
    let dir = tempfile::tempdir().expect("temp project");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn first() {}\n\npub fn canonical_result_needle() {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "nothing to see\n").unwrap();
    let mut app = CodeApp::open_path(dir.path());
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &automerge::ROOT);

    // The palette asks the text provider for a page: it answers at once with
    // nothing and a token, then with the hits once the walk ends.
    let providers = ui::search::capability::search_providers(&app);
    let provider = &providers[1];
    let first = provider.search_page(
        &ui::search::SearchQuery::new("canonical_result_needle"),
        None,
        64,
        16,
    );
    assert!(first.results.is_empty(), "the walk never blocks the query");
    let token = first.pending.expect("the rest is on its way");
    let deadline = ui::time::now() + HANG_BOUND;
    let page = loop {
        match provider.poll_page(&token) {
            ui::search::PagePoll::Ready(page) => break page,
            ui::search::PagePoll::Pending => {
                assert!(ui::time::now() < deadline, "the walk ended");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    };
    assert_eq!(page.results.len(), 1, "one line holds the needle");
    let hit = &page.results[0];
    match hit.row.as_ref().expect("a match in a file draws as one") {
        ui::search::SearchRow::FileHit {
            path,
            line,
            snippet,
            matched,
        } => {
            assert_eq!(path, "src/lib.rs");
            assert_eq!(*line, Some(3));
            assert_eq!(snippet, "pub fn canonical_result_needle() {}");
            assert_eq!(
                matched.clone(),
                Some(7..30),
                "the match is marked in the line"
            );
        }
        other => panic!("not a file hit: {other:?}"),
    }

    // Activating the hit opens the file with the caret on the match, without
    // asking for any panel: the palette is where results are shown.
    let address = hit.action_address().expect("canonical address");
    let resource = ui::resources::resource(&app, &doc, &address.resource)
        .expect("a text hit resolves through current widget authority");
    let encoded = tilo_actions::door::encode(&[resource], &address, None)
        .expect("the hit exports its reveal action");
    assert!(matches!(
        Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload
            ),
            &mut doc,
            &automerge::ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Completed(_))
    ));
    app.sync(&mut doc, &automerge::ROOT);
    assert_eq!(app.session.active_buffer().unwrap().name, "lib.rs");
    assert_eq!(app.editor.primary_caret(), (2, 7));
    assert!(!app.panel_visible, "the editor opens no results panel");
}

#[test]
fn bottom_dock_keeps_a_live_terminal_across_toggles_and_returns_focus_to_its_trigger() {
    // Each phase builds a fresh app (and its own terminal or headless driver).
    ide_embeds_the_real_terminal_widget();
    ide_terminal_runs_a_command_and_shows_output();
    dock_commands_open_the_requested_content_and_toggle_without_clearing_it();
    keyboard_closing_a_focused_terminal_returns_to_the_trigger_without_refocusing_terminal();
    retained_dock_trigger_shares_tab_chrome_and_uses_pointer_and_keyboard_routes();
}

fn ide_embeds_the_real_terminal_widget() {
    // The IDE must embed the real system `TerminalWidget` as its integrated
    // terminal — not ship a second, weaker emulator. Opening the Terminal
    // dock tab spawns a live widget whose grid the panel hands its body to.
    let mut app = CodeApp::with_sample();
    assert!(
        app.shots_terminal_grid_text().is_none(),
        "no terminal exists before the Terminal tab is opened"
    );
    app.shots_open_terminal();
    assert_eq!(app.panel.active_tab(), PanelTab::Terminal);
    assert!(
        app.shots_terminal_grid_text().is_some(),
        "opening the Terminal tab embeds a live TerminalWidget"
    );
}

fn ide_terminal_runs_a_command_and_shows_output() {
    // End-to-end: open the embedded terminal, type a command, pump the
    // app's `sync` (which drains the terminal's PTY), and prove the shell's
    // output flowed back into the IDE's integrated terminal grid.
    let mut app = CodeApp::with_sample();
    app.shots_open_terminal();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    app.shots_terminal_input("printf 'IDE_TERM_OK\\n'\n");
    let mut found = false;
    let deadline = ui::time::now() + HANG_BOUND;
    while ui::time::now() < deadline {
        app.sync(&mut state.doc, &state.root);
        if let Some(t) = app.shots_terminal_grid_text()
            && t.contains("IDE_TERM_OK")
        {
            found = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        found,
        "the IDE's embedded terminal must run the typed command and show its output"
    );
}

fn ensure_buffers_seeds_active_text() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    let node = app.session.active_buffer().unwrap().node;
    let text = EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).unwrap();
    assert!(text.contains("fn main"));
    assert!(app.editor.is_bound());
}

#[test]
fn tab_pills_mirror_open_buffers_by_node_id_and_close_prompts_only_when_dirty() {
    // Each phase builds a fresh sample app.
    opening_a_second_file_adds_a_tab();
    reconcile_tabs_keys_pills_to_open_buffers_by_node_id();
    reconcile_tabs_mirrors_dirty_into_the_shared_bars_modified_mark();
    closing_the_shared_bars_pill_removes_the_matching_buffer_by_id_not_position();
    selecting_a_different_pill_is_read_back_via_active_id();
    closing_a_dirty_tab_prompts_before_discarding();
    closing_a_clean_tab_closes_immediately();
}

fn opening_a_second_file_adds_a_tab() {
    let mut app = CodeApp::with_sample();
    // Find prime.rs.
    let prime = app
        .session
        .project
        .nodes
        .values()
        .find(|n| n.name == "prime.rs")
        .unwrap()
        .id;
    app.open_node(prime);
    assert_eq!(app.session.open.len(), 2);
    assert_eq!(app.session.active_buffer().unwrap().name, "prime.rs");
}

/// Every open buffer's node id, cast the same way `reconcile_tabs` casts
/// it, so a test can compare against `app.tabs.tabs()` ids without
/// duplicating the cast at every call site.
fn open_ids(app: &CodeApp) -> Vec<u64> {
    app.session.open.iter().map(|b| b.node as u64).collect()
}

fn reconcile_tabs_keys_pills_to_open_buffers_by_node_id() {
    let mut app = CodeApp::with_sample();
    let prime = app
        .session
        .project
        .nodes
        .values()
        .find(|n| n.name == "prime.rs")
        .unwrap()
        .id;
    app.open_node(prime); // open_node already reconciles.

    let pill_ids: Vec<u64> = app.tabs.tabs().iter().map(|t| t.id).collect();
    assert_eq!(
        pill_ids,
        open_ids(&app),
        "the bar's pills must be exactly the session's open buffers, in \
             the same order, keyed by node id"
    );
}

fn reconcile_tabs_mirrors_dirty_into_the_shared_bars_modified_mark() {
    let (mut app, mut doc) = synced_sample();
    let id = app.session.active_buffer().unwrap().node as u64;
    assert!(
        !app.tabs.is_modified(id),
        "a freshly-opened buffer is clean"
    );

    app.editor.insert_for_test(&mut doc, "X");
    app.dispatch_editor_text_edits(&mut doc);
    app.ensure_buffers(&mut doc);
    app.reconcile_tabs();
    assert!(
        app.tabs.is_modified(id),
        "an edited buffer's pill must pick up the unsaved-changes mark"
    );
}

fn closing_the_shared_bars_pill_removes_the_matching_buffer_by_id_not_position() {
    // The whole point of reconciling by node id rather than position: the
    // pill the user clicked × on must be the buffer that closes — even
    // one that is neither at index 0 nor the active tab, which is the
    // one case "close index 0" or "close whatever's active" would also
    // get right by accident.
    let mut app = CodeApp::with_sample();
    let names = ["prime.rs", "greeter.rs"];
    let ids: Vec<crate::model::NodeId> = names
        .iter()
        .map(|name| {
            app.session
                .project
                .nodes
                .values()
                .find(|n| n.name == *name)
                .unwrap()
                .id
        })
        .collect();
    for id in ids {
        app.open_node(id);
    }
    // [main.rs, prime.rs, greeter.rs], active = greeter.rs (index 2).
    assert_eq!(app.session.open.len(), 3);
    app.session.active = Some(0); // main.rs is active; prime.rs is neither.

    let prime_id = app.session.open[1].node as u64;
    // Stand-in for the user clicking prime.rs's own × — the widget fades
    // it and publishes the id via `take_close`, independent of position.
    app.tabs.start_closing(prime_id);

    let mut doc = AutoCommit::new();
    app.drain_child_events(&mut doc);

    let remaining: Vec<&str> = app.session.open.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        remaining,
        vec!["main.rs", "greeter.rs"],
        "closing prime.rs's pill must close prime.rs specifically, not \
             index 0 or whatever happens to be active"
    );
}

fn selecting_a_different_pill_is_read_back_via_active_id() {
    let mut app = CodeApp::with_sample();
    let prime = app
        .session
        .project
        .nodes
        .values()
        .find(|n| n.name == "prime.rs")
        .unwrap()
        .id;
    app.open_node(prime); // active is now prime.rs
    let main_node = app.session.open[0].node;
    assert_eq!(app.session.active, Some(1));

    // A press on the bar already moved its own `active_id` (see
    // `TabBar::handle_event`); this stands in for that click landing on
    // main.rs's pill instead.
    app.tabs.set_active(main_node as u64);
    app.bound_node = Some(main_node.wrapping_add(1)); // sentinel, must be cleared

    let mut doc = AutoCommit::new();
    app.drain_child_events(&mut doc);

    assert_eq!(
        app.session.active,
        Some(0),
        "the session must follow the bar's active_id back to main.rs"
    );
    assert_eq!(
        app.bound_node, None,
        "switching which buffer is selected must force the editor to re-bind"
    );
}

#[test]
fn editor_commands_edit_through_the_crdt_and_move_the_caret() {
    // Each phase builds a fresh synced sample.
    editing_routes_through_crdt();
    editor_gesture_and_replace_text_resource_encode_the_same_action();
    duplicate_line_repeats_it_below();
    move_line_down_swaps_with_the_next();
    delete_line_removes_it();
    toggle_comment_comments_then_uncomments();
    go_to_line_moves_the_caret();
    find_highlights_and_navigates_matches();
}

fn editing_routes_through_crdt() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    let before = EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).unwrap();
    app.editor.insert_for_test(&mut doc, "X");
    app.dispatch_editor_text_edits(&mut doc);
    let after = EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).unwrap();
    assert_eq!(after.chars().count(), before.chars().count() + 1);
}

fn editor_gesture_and_replace_text_resource_encode_the_same_action() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    let base = EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).unwrap();
    app.editor.move_caret_to(0, 0);
    app.editor.insert_for_test(&mut doc, "λ");
    let next = format!("λ{base}");
    let expected = ui::resources::resources(&app, &doc)
        .iter()
        .find_map(|resource| {
            find_resource(resource, &actions::file_resource_id(app.buffer_key(node)))
        })
        .and_then(|resource| {
            resource
                .actions
                .iter()
                .find(|action| action.id == "replace-text")
                .cloned()
        })
        .expect("active file publishes Replace text")
        .build_args([("text".to_string(), tilo_actions::ArgValue::Text(next))])
        .expect("valid replace text argument");
    app.dispatch_editor_text_edits(&mut doc);
    let actual = app
        .actions
        .last_dispatched()
        .expect("gesture dispatched action");
    assert_eq!(
        actual, &expected,
        "gesture and resource action are identical"
    );
}

#[test]
fn sidebar_docks_when_wide_overlays_when_narrow_and_follows_file_opens() {
    // Each phase builds a fresh sample app.
    toggle_sidebar_command_hides_it();
    layout_is_docked_when_wide_overlay_when_narrow();
    narrow_window_hides_sidebar_by_default();
    wide_window_docks_sidebar_open_by_default();
    opening_a_file_in_overlay_dismisses_the_flap();
    opening_a_file_keeps_docked_sidebar_pinned();
}

fn toggle_sidebar_command_hides_it() {
    // Default width hint is wide → docked-open; the command flips it shut.
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    assert!(app.sidebar_open());
    app.apply_command(Command::ToggleSidebar, &mut doc);
    assert!(!app.sidebar_open());
    app.apply_command(Command::ToggleSidebar, &mut doc);
    assert!(app.sidebar_open());
}

#[test]
fn project_files_are_created_by_durable_actions_and_adopted_from_peers() {
    // Each phase builds a fresh sample app and document.
    new_file_command_dispatches_the_durable_create_file_action();
    a_collaborative_file_is_adopted_as_a_tracked_file();
}

fn new_file_command_dispatches_the_durable_create_file_action() {
    // The on-screen "New file" command and the Ctrl+K "New file" spec must
    // build the byte-identical action — one source of truth. Both carry the
    // UUID captured at authoring time, never a local project-arena id.
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc); // seed the sample project's open buffers

    let ctx = app.code_resource_ctx();
    let expected_file_id = ctx.next_file_id.clone();
    let resources = crate::actions::provide_resources(&ctx, &doc);
    let spec = resources[0]
        .actions
        .iter()
        .find(|a| a.id == "new-file")
        .unwrap();
    let from_spec = match &spec.input {
        tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
        tilo_actions::ActionInput::Prompted { .. } | tilo_actions::ActionInput::Deferred { .. } => {
            panic!("new-file is an immediate action")
        }
    };

    app.apply_command(Command::NewFile, &mut doc);

    let dispatched = app
        .actions
        .last_dispatched()
        .expect("New File dispatched an action");
    assert_eq!(
        dispatched, &from_spec,
        "the command and the Ctrl+K spec build the identical action"
    );
    // The action actually materialised the buffer in the document.
    assert!(EditorDoc::read_buffer_key(&doc, &expected_file_id).is_some());
}

fn a_collaborative_file_is_adopted_as_a_tracked_file() {
    // A peer's durable file record is surfaced by the next sync. The
    // resource UUID, rather than a coincidental local arena number, is the
    // thing the projection follows.
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    let before = app.session.project.nodes.len();

    let orphan = "peer-file-uuid";
    EditorDoc::create_file(
        &mut doc,
        &crate::doc::CollaborativeFile {
            id: orphan.into(),
            kind: "file".into(),
            name: "peer.rs".into(),
            path: "peer.rs".into(),
            parent: None,
            language: "Rust".into(),
            order: 1,
            deleted: false,
            name_revision: 0,
            structure_revision: 0,
            language_revision: 0,
            deletion_revision: 0,
        },
        "// created out of band\n",
    );
    assert!(app.session.project.node_for_resource(orphan).is_none());

    app.ensure_buffers(&mut doc);
    assert!(
        app.session.project.node_for_resource(orphan).is_some(),
        "the document buffer was adopted as a file"
    );
    assert_eq!(app.session.project.nodes.len(), before + 1);
}

fn layout_is_docked_when_wide_overlay_when_narrow() {
    let mut app = CodeApp::with_sample();
    app.set_width_hint(1200.0);
    assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
    app.set_width_hint(600.0);
    assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
    // Boundary is inclusive at the breakpoint → docked.
    let breakpoint = app.drawer.resolved_breakpoint();
    app.set_width_hint(breakpoint);
    assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
    app.set_width_hint(breakpoint - 1.0);
    assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
}

fn narrow_window_hides_sidebar_by_default() {
    let mut app = CodeApp::with_sample();
    app.set_width_hint(600.0);
    assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
    // A fresh narrow window opens straight to the editor.
    assert!(!app.sidebar_open(), "overlay sidebar is closed by default");
    // The hamburger summons it.
    app.toggle_sidebar();
    assert!(app.sidebar_open());
}

fn wide_window_docks_sidebar_open_by_default() {
    let mut app = CodeApp::with_sample();
    app.set_width_hint(1200.0);
    assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
    assert!(app.sidebar_open(), "docked sidebar opens by default");
}

fn opening_a_file_in_overlay_dismisses_the_flap() {
    let mut app = CodeApp::with_sample();
    app.set_width_hint(600.0);
    app.toggle_sidebar();
    assert!(app.sidebar_open(), "summoned open before selecting a file");
    // Find prime.rs and open it via the tree-event path the sidebar emits.
    let prime = app
        .session
        .project
        .nodes
        .values()
        .find(|n| n.name == "prime.rs")
        .unwrap()
        .id;
    app.apply_tree_event(ExplorerEvent::Activate(prime));
    assert_eq!(app.session.active_buffer().unwrap().name, "prime.rs");
    assert!(
        !app.sidebar_open(),
        "opening a file dismisses the overlay flap"
    );
}

fn opening_a_file_keeps_docked_sidebar_pinned() {
    let mut app = CodeApp::with_sample();
    app.set_width_hint(1200.0);
    assert!(app.sidebar_open());
    let prime = app
        .session
        .project
        .nodes
        .values()
        .find(|n| n.name == "prime.rs")
        .unwrap()
        .id;
    app.apply_tree_event(ExplorerEvent::Activate(prime));
    assert!(app.sidebar_open(), "docked sidebar stays open after open");
}

/// Overwrite a buffer's whole text through the CRDT, the way an edit does.
fn replace_buffer_text(doc: &mut AutoCommit, key: &str, text: &str) {
    let Some(obj) = EditorDoc::ensure_buffer_key(doc, key, "") else {
        return;
    };
    let len = doc.text(&obj).map_or(0, |t| t.chars().count());
    EditorDoc::splice(doc, &obj, 0, len, text);
}

#[test]
fn problems_panel_reports_lint_findings_and_defers_to_the_language_server() {
    // Each phase builds a fresh app and document.
    diagnostics_populate_for_buffer_with_todo();
    an_unfinished_server_replaces_the_no_problems_claim();
    the_panel_learns_the_server_is_still_working();
    a_published_diagnostic_retires_the_bracket_scan();
    explicit_problems_open_survives_an_empty_diagnostics_refresh();
}

fn diagnostics_populate_for_buffer_with_todo() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    // Put a marker the linter is guaranteed to report into the open buffer,
    // so this asserts that the pipeline carries a real finding through to
    // the panel rather than merely that it does not panic.
    let node = app.session.active_buffer().expect("a buffer is open").node;
    replace_buffer_text(
        &mut doc,
        &app.buffer_key(node),
        "// TODO: prove the pipeline\n",
    );
    app.refresh_diagnostics(&doc);
    assert!(
        app.panel.problem_count() > 0,
        "the linter's finding reaches the panel"
    );
}

/// An empty Problems list means two completely different things depending
/// on whether anything has looked yet, and the panel has to distinguish
/// them: "analysed and clean" is a finding, "not analysed yet" is not.
fn an_unfinished_server_replaces_the_no_problems_claim() {
    let busy = LspStatus::Busy {
        server: "rust-analyzer".to_string(),
        title: "Indexing".to_string(),
        detail: None,
        percentage: Some(37),
    };
    let note = busy
        .analysis_note()
        .expect("a working server explains itself");
    assert!(
        note.contains("rust-analyzer"),
        "it names the server: {note}"
    );
    assert!(note.contains("indexing"), "it names the task: {note}");
    assert!(note.contains("37%"), "it carries the progress: {note}");
    assert!(
        note.contains("still"),
        "it reads as work in progress, not a result: {note}"
    );

    assert!(
        LspStatus::Starting {
            server: "rust-analyzer".to_string()
        }
        .analysis_note()
        .is_some(),
        "a server that has not answered yet is also not a verdict"
    );

    // Once something has answered — or there is no server at all and the
    // local linter is the analysis — an empty list is a real finding again.
    for settled in [
        LspStatus::Ready {
            server: "rust-analyzer".to_string(),
        },
        LspStatus::Failed("rust-analyzer exited".to_string()),
        LspStatus::None,
    ] {
        assert_eq!(
            settled.analysis_note(),
            None,
            "{settled:?} must let the panel state its finding"
        );
    }
}

/// And the note reaches the panel through the production path, not just the
/// helper that formats it.
fn the_panel_learns_the_server_is_still_working() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);

    app.refresh_diagnostics(&doc);
    assert_eq!(
        app.panel.analysis(),
        None,
        "with no server the linter is the analysis"
    );

    app.shots_indexing();
    app.refresh_diagnostics(&doc);
    let shown = app.panel.analysis().expect("the panel is told");
    assert!(shown.contains("rust-analyzer"), "got: {shown}");

    app.lsp_status = LspStatus::Ready {
        server: "rust-analyzer".to_string(),
    };
    app.refresh_diagnostics(&doc);
    assert_eq!(
        app.panel.analysis(),
        None,
        "the note clears once the server has settled"
    );
}

#[test]
fn disk_files_load_stat_and_save_back_through_the_crdt() {
    // Each phase uses its own tempdir, app and document.
    open_file_from_path_loads_it();
    a_document_from_a_previous_session_still_stats_the_file();
    edit_and_save_round_trips_to_disk();
}

/// A persisted document handing back a buffer must not let a save skip the
/// conflict check.
///
/// The document outlives the process. On the next launch it already holds
/// text for the node, which used to be read as "this file is loaded" — so
/// the file was never stat'ed, the buffer carried no stamp, and
/// [`crate::disk::save`] takes a missing stamp as "no check requested" and
/// writes unconditionally. Driving the real editor over a real file with a
/// document from an earlier run overwrote 203 bytes of it without a word.
fn a_document_from_a_previous_session_still_stats_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() { on_disk() }\n").unwrap();

    let mut app = CodeApp::open_path(&file);
    let node = app.session.active_buffer().expect("a tab is open").node;

    // A document that already carries this buffer, as a restored one does,
    // holding text that is not what the file says.
    let mut doc = AutoCommit::new();
    EditorDoc::ensure_buffer_key(
        &mut doc,
        &app.buffer_key(node),
        "fn main() { from_last_session() }\n",
    );

    app.ensure_buffers(&mut doc);

    let buf = app
        .session
        .open
        .iter()
        .find(|b| b.node == node)
        .expect("still open");
    assert!(
        buf.stamp.is_some(),
        "the file was stat'ed, so a save has something to check against"
    );
    assert_eq!(
        buf.path.as_deref().map(crate::disk::canonical),
        Some(crate::disk::canonical(&file)),
        "and the buffer knows which file it writes back to"
    );
    assert_eq!(
        buf.saved_text, "fn main() { on_disk() }\n",
        "what counts as 'unmodified' is the bytes on disk, not the document"
    );
    assert!(
        buf.dirty,
        "the restored text differs from the file, so it reads as unsaved \
             edits rather than as agreement"
    );
    // The restored text is kept — a previous session's unsaved work, or a
    // peer's in-flight edits, are not ours to throw away.
    assert_eq!(
        EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).as_deref(),
        Some("fn main() { from_last_session() }\n")
    );
}

/// The structural scan defers to a language server that has published for
/// the buffer, and only to one that has.
fn a_published_diagnostic_retires_the_bracket_scan() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    let node = app.session.active_buffer().expect("a buffer is open").node;
    replace_buffer_text(&mut doc, &app.buffer_key(node), "fn main( {\n");

    app.refresh_diagnostics(&doc);
    assert!(
        app.panel
            .problems()
            .iter()
            .any(|d| d.message.contains("Unclosed")),
        "with nothing better answering, the scan reports the unbalanced bracket"
    );

    // Now let a server publish for this buffer — even an empty publish is
    // an answer, and it is the answer that counts.
    let uri = app
        .lsp_uri
        .clone()
        .unwrap_or_else(|| "file:///probe.rs".to_string());
    app.lsp_uri = Some(uri.clone());
    app.lsp_diagnostics.insert(uri, Vec::new());
    app.refresh_diagnostics(&doc);
    assert!(
        !app.panel
            .problems()
            .iter()
            .any(|d| d.message.contains("Unclosed")),
        "the scanner's guess yields to the compiler's answer: {:?}",
        app.panel
            .problems()
            .iter()
            .map(|d| &d.message)
            .collect::<Vec<_>>()
    );
}

/// Bring up the sample project and get the editor's line cache in sync with
/// the document, so line operations read real line contents.
fn synced_sample() -> (CodeApp, AutoCommit) {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);
    app.refresh_search_catalog(&mut doc);
    app.editor.sync(&mut doc, &automerge::ROOT);
    (app, doc)
}

fn buffer_lines(app: &CodeApp, doc: &AutoCommit, node: TreeId) -> Vec<String> {
    EditorDoc::read_buffer_key(doc, &app.buffer_key(node))
        .unwrap()
        .split('\n')
        .map(str::to_string)
        .collect()
}

fn find_resource<'a>(
    resource: &'a tilo_actions::Resource,
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    (resource.id.as_str() == id)
        .then_some(resource)
        .or_else(|| {
            resource
                .children
                .iter()
                .find_map(|child| find_resource(child, id))
        })
}

/// A real file picked from disk loads its bytes into a tab.
fn open_file_from_path_loads_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("greet.rs");
    std::fs::write(&file, "fn greet() { let x = 1; }\n").unwrap();

    let mut app = CodeApp::open_path(&file);
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc);

    let node = app.session.active_buffer().expect("a tab opened").node;
    assert_eq!(
        EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).as_deref(),
        Some("fn greet() { let x = 1; }\n"),
        "the file's real bytes were read into the buffer"
    );
}

/// Open a real file, edit it through the CRDT, save, and prove the new bytes
/// reached disk.
fn edit_and_save_round_trips_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "first line\n").unwrap();

    let mut app = CodeApp::open_path(&file);
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&mut doc); // reads the file + stamps it

    let node = app.session.active_buffer().expect("a tab opened").node;
    // Edit through the same CRDT splice path the editor uses.
    let obj = EditorDoc::buffer_text_key(&doc, &app.buffer_key(node)).unwrap();
    let at = "first line".chars().count();
    EditorDoc::splice(&mut doc, &obj, at, 0, " EDITED");

    let snapshot = doc.clone();
    assert!(
        app.save_buffer(&snapshot, node, false),
        "the save reported success"
    );
    let on_disk = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        on_disk, "first line EDITED\n",
        "the edited bytes were written back to the real file"
    );
}

/// Find populates real matches over the active buffer, hands them to the
/// editor's highlight layer, and stepping to a match selects it.
fn find_highlights_and_navigates_matches() {
    let (mut app, mut doc) = synced_sample();
    app.open_find();
    app.find.set_query("primes");
    app.drive_find(&mut doc);

    assert!(
        app.find_matches.len() >= 2,
        "'primes' occurs several times in main.rs, got {}",
        app.find_matches.len()
    );
    // The matches are handed to the editor's highlight layer.
    assert_eq!(
        app.editor.find_match_count_for_test(),
        app.find_matches.len(),
        "the editor received every match to highlight"
    );

    // The bar lands on the first match; stepping to the second navigates the
    // caret onto it and marks it as the current highlight.
    assert_eq!(app.find_current, Some(0));
    app.find.next();
    app.drive_find(&mut doc);
    let m = app.find_matches[1];
    assert_eq!(app.find_current, Some(1), "the second match is now current");
    assert_eq!(
        app.editor.primary_caret(),
        (m.line, m.col),
        "the caret navigated onto the match"
    );
}

/// Go-to-line moves the caret to the requested 1-based line and clamps past
/// the end of the buffer.
fn go_to_line_moves_the_caret() {
    let (mut app, _doc) = synced_sample();
    assert!(app.editor.line_count() > 5);

    app.go_to_line(5);
    assert_eq!(app.editor.primary_caret(), (4, 0), "1-based line 5 → row 4");

    let last = app.editor.line_count();
    app.go_to_line(99_999);
    assert_eq!(
        app.editor.primary_caret().0,
        last - 1,
        "a line past the end clamps to the last row"
    );
}

fn duplicate_line_repeats_it_below() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    let before = buffer_lines(&app, &doc, node);

    app.editor.move_caret_to(0, 0);
    app.editor.duplicate_lines(&mut doc, true);
    app.dispatch_editor_text_edits(&mut doc);

    let after = buffer_lines(&app, &doc, node);
    assert_eq!(after.len(), before.len() + 1, "one line added");
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[0], "the copy sits directly below");
    assert_eq!(app.editor.primary_caret().0, 1, "caret rode onto the copy");
}

fn move_line_down_swaps_with_the_next() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    let before = buffer_lines(&app, &doc, node);

    app.editor.move_caret_to(0, 0);
    app.editor.move_lines(&mut doc, false);
    app.dispatch_editor_text_edits(&mut doc);

    let after = buffer_lines(&app, &doc, node);
    assert_eq!(after.len(), before.len(), "no lines gained or lost");
    assert_eq!(after[0], before[1], "line 1 moved up");
    assert_eq!(after[1], before[0], "line 0 moved down");
    assert_eq!(app.editor.primary_caret().0, 1, "caret followed the line");
}

fn delete_line_removes_it() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    let before = buffer_lines(&app, &doc, node);

    app.editor.move_caret_to(0, 0);
    app.editor.delete_lines(&mut doc);
    app.dispatch_editor_text_edits(&mut doc);

    let after = buffer_lines(&app, &doc, node);
    assert_eq!(after.len(), before.len() - 1, "one line removed");
    assert_eq!(after[0], before[1], "the following line is now first");
}

fn toggle_comment_comments_then_uncomments() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    // Replace the buffer with a content-stable single line of Rust.
    let obj = EditorDoc::buffer_text_key(&doc, &app.buffer_key(node)).unwrap();
    let len = doc.text(&obj).unwrap().chars().count();
    EditorDoc::splice(&mut doc, &obj, 0, len, "let x = 1;\n");
    app.editor.sync(&mut doc, &automerge::ROOT);

    app.editor.move_caret_to(0, 0);
    app.editor.toggle_line_comment(&mut doc);
    app.dispatch_editor_text_edits(&mut doc);
    assert_eq!(
        EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).as_deref(),
        Some("// let x = 1;\n"),
        "the line gained a Rust line comment"
    );

    app.editor.move_caret_to(0, 0);
    app.editor.toggle_line_comment(&mut doc);
    app.dispatch_editor_text_edits(&mut doc);
    assert_eq!(
        EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).as_deref(),
        Some("let x = 1;\n"),
        "toggling again removed it"
    );
}

fn closing_a_dirty_tab_prompts_before_discarding() {
    let (mut app, mut doc) = synced_sample();
    let node = app.session.active_buffer().unwrap().node;
    // Edit the buffer so it has unsaved changes, then let a frame recompute
    // the dirty flag the way `sync` does.
    app.editor.insert_for_test(&mut doc, "X");
    app.dispatch_editor_text_edits(&mut doc);
    app.ensure_buffers(&mut doc);
    assert!(
        app.session.buffer_of(node).unwrap().dirty,
        "the buffer now has unsaved edits"
    );

    let before = app.session.open.len();
    let active = app.session.active.unwrap();
    app.request_close_tab(active);
    assert_eq!(
        app.session.open.len(),
        before,
        "a dirty tab is not closed out from under the user"
    );
    assert!(app.close_prompt.is_some(), "the save prompt is raised");

    // Discarding closes it and drops the edits.
    app.resolve_close_prompt(&doc, CloseChoice::Discard);
    assert_eq!(app.session.open.len(), before - 1, "the tab closed");
    assert!(app.close_prompt.is_none());
}

fn closing_a_clean_tab_closes_immediately() {
    let (mut app, _doc) = synced_sample();
    let before = app.session.open.len();
    let active = app.session.active.unwrap();
    app.request_close_tab(active);
    assert!(
        app.close_prompt.is_none(),
        "a saved tab closes without a prompt"
    );
    assert_eq!(app.session.open.len(), before - 1);
}

fn dock_commands_open_the_requested_content_and_toggle_without_clearing_it() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();

    app.apply_command(Command::GoToProblems, &mut doc);
    assert!(app.panel_visible);
    assert_eq!(app.panel.active_tab(), PanelTab::Problems);

    app.apply_command(Command::ToggleBottomPanel, &mut doc);
    assert!(!app.panel_visible);
    app.apply_command(Command::ToggleBottomPanel, &mut doc);
    assert!(app.panel_visible);
    assert_eq!(app.panel.active_tab(), PanelTab::Problems);

    app.apply_command(Command::GoToTerminal, &mut doc);
    assert!(app.panel_visible);
    assert_eq!(app.panel.active_tab(), PanelTab::Terminal);
    assert!(
        app.terminal.is_some(),
        "Terminal command keeps the live terminal opener"
    );
    let terminal_before = app
        .terminal
        .as_ref()
        .map(std::ptr::from_ref)
        .expect("Terminal command creates a retained terminal");
    let terminal_content_before = app.shots_terminal_grid_text();
    app.apply_command(Command::ToggleBottomPanel, &mut doc);
    app.apply_command(Command::ToggleBottomPanel, &mut doc);
    assert_eq!(app.panel.active_tab(), PanelTab::Terminal);
    assert!(
        app.terminal.is_some(),
        "closing and reopening preserves the terminal session"
    );
    assert_eq!(
        app.terminal.as_ref().map(std::ptr::from_ref),
        Some(terminal_before),
        "closing and reopening retains the same live Terminal object"
    );
    assert_eq!(
        app.shots_terminal_grid_text(),
        terminal_content_before,
        "closing and reopening does not replace Terminal content"
    );
}

fn explicit_problems_open_survives_an_empty_diagnostics_refresh() {
    let mut app = CodeApp::new();
    let mut doc = AutoCommit::new();
    app.apply_command(Command::GoToProblems, &mut doc);

    app.refresh_diagnostics(&doc);

    assert!(app.panel.problems().is_empty());
    assert!(
        app.panel_visible,
        "an explicit Problems open is authoritative"
    );
    assert_eq!(app.panel.active_tab(), PanelTab::Problems);
}

fn fresh_editor_collapses_the_empty_bottom_panel() {
    let app = CodeApp::with_sample();

    assert!(
        !app.panel_visible,
        "a fresh editor must give its height to code"
    );
    assert!(
        app.children()
            .iter()
            .all(|child| child.test_id() != Some("bottom_panel")),
        "the hidden dock must leave the retained child tree"
    );
}

fn keyboard_closing_a_focused_terminal_returns_to_the_trigger_without_refocusing_terminal() {
    let mut app = CodeApp::with_sample();
    app.shots_open_terminal();
    let mut driver = ui::headless::Driver::new(ui::State::new(), Box::new(app), (480, 900));

    let panel = driver.targets()["bottom_panel"];
    driver.click_at(panel.center().x, panel.y1 - 20.0);
    driver.set_modifiers(&["ctrl".to_string()]);
    driver.key("j");
    driver.set_modifiers(&[]);

    assert!(!driver.targets().contains_key("bottom_panel"));
    assert_eq!(
        driver
            .focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Show panel"),
        "keyboard close returns focus to the retained disclosure"
    );

    driver.set_modifiers(&["ctrl".to_string()]);
    driver.key("j");
    driver.set_modifiers(&[]);
    assert!(driver.targets().contains_key("bottom_panel"));
    assert_eq!(
        driver
            .focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Hide panel"),
        "reopening keeps focus on the disclosure instead of hidden Terminal content"
    );

    driver.key("Enter");
    assert!(
        !driver.targets().contains_key("bottom_panel"),
        "Enter reaches the focused disclosure after reopening instead of the Terminal"
    );
}

#[allow(
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "one pointer and keyboard walk through the dock; its geometry is exact"
)]
fn retained_dock_trigger_shares_tab_chrome_and_uses_pointer_and_keyboard_routes() {
    fn trigger_semantics(snapshot: &ui::headless::InspectionSnapshot) -> &ui::a11y::AccessNode {
        fn find(node: &ui::headless::InspectionNode) -> Option<&ui::a11y::AccessNode> {
            if node.test_id.as_deref() == Some("code.bottom-panel-toggle") {
                return node.semantics.as_ref();
            }
            node.children.iter().find_map(find)
        }

        find(&snapshot.root).expect("dock trigger semantics")
    }

    let mut driver = ui::headless::Driver::new(
        ui::State::new(),
        Box::new(CodeApp::with_sample()),
        (480, 900),
    );
    let targets = driver.targets();
    let tabs = targets.get("tabs").copied().expect("tab bar target");
    let trigger = targets
        .get("code.bottom-panel-toggle")
        .copied()
        .expect("retained dock trigger");
    // The trigger is an icon button of the toolkit's size, centred on the
    // tab strip's row, so it neither spans the bar nor touches the content.
    let target = f64::from(ui::theme::Theme::dark().tokens.metrics.min_pointer_target);
    assert!(
        (trigger.center().y - tabs.center().y).abs() < 0.01,
        "trigger is centred on the tab row: {} vs {}",
        trigger.center().y,
        tabs.center().y
    );
    assert!(
        trigger.height() >= target && trigger.height() <= tabs.height(),
        "trigger is a pointer-target square inside the row: {} in {}",
        trigger.height(),
        tabs.height()
    );
    assert!(
        tabs.x1 <= trigger.x0,
        "tab pills and dock trigger must not overlap"
    );
    let closed_snapshot = driver.inspection();
    let closed = trigger_semantics(&closed_snapshot);
    assert_eq!(closed.name.as_deref(), Some("Show panel"));
    assert_eq!(closed.state.expanded, Some(false));
    assert!(!closed.state.selected);

    driver
        .click_named("code.bottom-panel-toggle")
        .expect("pointer activates retained trigger");
    assert!(driver.targets().contains_key("bottom_panel"));
    let open_snapshot = driver.inspection();
    let open = trigger_semantics(&open_snapshot);
    assert_eq!(open.name.as_deref(), Some("Hide panel"));
    assert_eq!(open.state.expanded, Some(true));
    assert!(open.state.selected);
    assert_eq!(
        driver
            .focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Hide panel"),
        "click transfers keyboard focus to the retained trigger"
    );

    driver.key("Enter");
    assert!(
        !driver.targets().contains_key("bottom_panel"),
        "keyboard activation routes through the same toggle command"
    );

    driver.set_modifiers(&["ctrl".to_string()]);
    driver.key("j");
    driver.set_modifiers(&[]);
    assert!(
        driver.targets().contains_key("bottom_panel"),
        "Ctrl+J remains an equivalent keyboard route"
    );

    driver.key("Escape");
    assert!(
        !driver.targets().contains_key("bottom_panel"),
        "Escape closes the dock disclosure"
    );
    assert_eq!(
        driver
            .focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Show panel"),
        "Escape returns focus to the retained trigger"
    );

    let trigger_node = driver
        .a11y_node_named("Show panel")
        .expect("trigger is exported to assistive technology");
    driver
        .perform_a11y_action(trigger_node, &ui::a11y::Action::Activate)
        .expect("semantic activation uses the retained control");
    assert!(
        driver.targets().contains_key("bottom_panel"),
        "assistive activation reaches the same canonical toggle"
    );

    let panel_narrow = driver.targets()["bottom_panel"];
    assert!(
        panel_narrow.x0 >= 0.0
            && panel_narrow.y0 >= 0.0
            && panel_narrow.x1 <= 480.0
            && panel_narrow.y1 <= 900.0,
        "open panel stays inside the narrow viewport"
    );
    driver.resize((800, 700)).expect("resize open editor");
    let resized = driver.targets();
    let panel_standard = resized["bottom_panel"];
    let trigger_standard = resized["code.bottom-panel-toggle"];
    assert!(
        panel_standard.x0 >= 0.0
            && panel_standard.y0 >= 0.0
            && panel_standard.x1 <= 800.0
            && panel_standard.y1 <= 700.0,
        "open panel stays inside the resized viewport"
    );
    assert_eq!(panel_standard.height(), panel_narrow.height());
    // Same geometry after the resize: centred on the document tabs' row (the
    // open dock has a tab strip of its own, answering to the same name).
    assert!((trigger_standard.center().y - tabs.center().y).abs() < 0.01);
    assert!((trigger_standard.height() - trigger.height()).abs() < 0.01);
    assert!(trigger_standard.x1 <= 800.0);
    let resized_snapshot = driver.inspection();
    let resized_trigger = trigger_semantics(&resized_snapshot);
    assert_eq!(resized_trigger.name.as_deref(), Some("Hide panel"));
    assert_eq!(resized_trigger.state.expanded, Some(true));
    assert!(resized_trigger.state.selected);

    driver.resize((480, 900)).expect("resize back while open");
    let restored = driver.targets();
    assert!(restored.contains_key("bottom_panel"));
    assert_eq!(restored["bottom_panel"].height(), panel_narrow.height());
    assert!(
        (restored["code.bottom-panel-toggle"].y0 - trigger.y0).abs() < 0.01,
        "the trigger keeps its place in the row"
    );
}

/// One offscreen frame of `app` over the window ground: sync, layout, the main
/// pass and the top layer, as the live host runs them.
fn render(app: &mut CodeApp, viewport: (u32, u32)) -> ui::launch::ShotFrame {
    let mut frame = ui::launch::ShotFrame::new(viewport, Theme::dark());
    frame.sync(app);
    frame.layout(app).expect("the editor lays out");
    let mut scene = frame.draw(app);
    frame.draw_overlay(app, &mut scene);
    frame
}

#[test]
fn the_status_bar_reserves_its_own_band_under_everything() {
    let mut app = CodeApp::with_sample();
    app.shots_open_terminal();
    let _frame = render(&mut app, (1280, 800));
    let status = app.status_rect;
    let bar = ui::chrome::status_bar::height(&Theme::dark());
    assert!(
        (status.height() - bar).abs() < 0.01,
        "the band is the bar's height"
    );
    assert!(
        (status.y1 - 800.0).abs() < 0.01,
        "it sits on the window's bottom edge"
    );
    assert!(
        app.editor.get_rect().y1 <= app.panel.get_rect().y0 + 0.01
            && app.panel.get_rect().y1 <= status.y0 + 0.01,
        "the editor and the dock end where the bar begins; nothing is laid out under it"
    );
}

#[test]
fn the_sidebar_toggle_leads_the_sidebar_when_docked_and_the_tab_strip_otherwise() {
    // Docked and open: the toggle is in the sidebar's header, and the tab
    // strip starts at the editor column.
    let mut app = CodeApp::with_sample();
    let _frame = render(&mut app, (1280, 800));
    let sidebar = app.drawer.sidebar_rect();
    assert!(sidebar.width() > 0.0);
    assert!(sidebar.contains(app.toggle_rect.center()));
    assert!(app.tabs.get_rect().x0 >= sidebar.x1);

    // Hidden: the toggle moves to the leading edge of the tab strip, and the
    // first tab starts after it with a gap.
    app.toggle_sidebar();
    app.drawer.snap(false);
    let _frame = render(&mut app, (1280, 800));
    assert!(app.toggle_in_tab_strip());
    assert!(app.toggle_rect.width() > 0.0);
    assert!(
        app.tabs
            .tab_rects()
            .first()
            .is_some_and(|tab| tab.x0 > app.toggle_rect.x1),
        "the first tab clears the toggle: {:?} vs {:?}",
        app.tabs.tab_rects().first(),
        app.toggle_rect
    );

    // A narrow window starts with the sidebar as a closed drawer.
    let mut narrow = CodeApp::with_sample();
    narrow.set_width_hint(600.0);
    let _frame = render(&mut narrow, (600, 800));
    assert_eq!(narrow.sidebar_layout(), SplitLayout::Overlay);
    assert!(narrow.toggle_in_tab_strip());
    assert!(
        narrow.editor.get_rect().x0.abs() < 0.01,
        "the editor spans the window"
    );
}

#[test]
fn the_sidebar_toggle_is_published_to_assistive_technology() {
    let mut app = CodeApp::with_sample();
    let _frame = render(&mut app, (1280, 800));
    let root = Widget::accessibility(&app).expect("a semantic root");
    let toggle = root
        .children
        .iter()
        .find(|child| child.name.as_deref() == Some("Hide sidebar"))
        .expect("the toggle is a named button");
    assert_eq!(toggle.action_target.as_deref(), Some("sidebar-toggle"));
    let mut doc = AutoCommit::new();
    assert!(Widget::perform_semantic_action(
        &mut app,
        "sidebar-toggle",
        &ui::a11y::Action::Activate,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(!app.sidebar_open());
}

#[test]
fn searching_the_project_and_the_palette_shortcuts_open_the_platform_search() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    // Searching the project and going to a line both go to the platform.
    for (command, query) in [
        (Command::SearchProject, None),
        (Command::GoToLine, Some("Go to line")),
    ] {
        app.apply_command(command, &mut doc);
        let invocation = ui::widget::controls::ActionControls::take_control_invocation(&mut app)
            .expect("the command asks the platform to open search");
        let mut request = ui::search::SearchRequest::this_app();
        if let Some(query) = query {
            request = request.with_query(query);
        }
        assert_eq!(invocation, ui::search::open_search_invocation(&request));
        assert!(
            ui::widget::controls::ActionControls::take_control_invocation(&mut app).is_none(),
            "the request is taken once"
        );
    }
    // There is no in-app search panel left to open.
    app.apply_command(Command::SearchProject, &mut doc);
    assert!(!app.panel_visible);
}

fn three_action_sheet(app: &mut CodeApp) -> Vec<vello::kurbo::Rect> {
    let _frame = render(app, (1280, 800));
    let actions = app.dialog_actions.clone();
    assert_eq!(actions.len(), 3, "a question has three answers");
    for (i, a) in actions.iter().enumerate() {
        assert!(a.area() > 0.0, "action {i} is laid out");
        for b in actions.iter().skip(i + 1) {
            assert!(a.intersect(*b).area() <= 0.0, "actions do not overlap");
        }
    }
    actions
}

#[test]
fn the_unsaved_close_question_is_a_three_action_sheet_over_the_window() {
    let (mut app, mut doc) = synced_sample();
    app.editor.insert_for_test(&mut doc, "X");
    app.dispatch_editor_text_edits(&mut doc);
    app.ensure_buffers(&mut doc);
    app.reconcile_tabs();
    let before = app.session.open.len();
    app.request_close_tab(0);
    assert_eq!(app.question(), Some(dialogs::Question::ClosePrompt));
    three_action_sheet(&mut app);

    // Cancel (the last action) keeps the tab and answers the question.
    app.answer(dialogs::Question::ClosePrompt, 2, &mut doc);
    assert_eq!(app.question(), None);
    assert_eq!(app.session.open.len(), before);

    // Don't save (the middle one) discards the edits and closes it.
    app.request_close_tab(0);
    app.answer(dialogs::Question::ClosePrompt, 1, &mut doc);
    assert_eq!(app.question(), None);
    assert_eq!(app.session.open.len(), before - 1);
}

#[test]
fn a_save_conflict_outranks_the_close_question() {
    let mut app = CodeApp::with_sample();
    app.shots_close_prompt();
    app.shots_conflict();
    assert_eq!(app.question(), Some(dialogs::Question::Conflict));
    three_action_sheet(&mut app);
    assert!(app.dialog_hover.is_none());
}

#[test]
fn find_and_replace_go_through_the_bar_and_the_edit_action() {
    use ui::widget::Findable;
    let (mut app, mut doc) = synced_sample();
    app.open_find();
    app.find.set_query("primes");
    app.drive_find(&mut doc);
    let matches = app.find_matches.len();
    assert!(matches >= 2);
    assert_eq!(
        app.find_current,
        Some(0),
        "the bar lands on the first match"
    );

    let replaced = find_workflow::FindTarget {
        app: &mut app,
        doc: &mut doc,
    }
    .replace_all("odds");
    assert_eq!(replaced, matches);
    let node = app.active_file_node().unwrap();
    let text = EditorDoc::read_buffer_key(&doc, &app.buffer_key(node)).unwrap();
    assert_eq!(text.matches("odds").count(), matches);
    assert!(!text.contains("primes"));
    assert!(
        app.notice().is_some_and(|n| n.text.starts_with("Replaced")),
        "the replacement says how much it changed"
    );
}

#[test]
fn closing_the_find_bar_stops_painting_matches() {
    let (mut app, mut doc) = synced_sample();
    app.open_find();
    app.find.set_query("primes");
    app.drive_find(&mut doc);
    assert!(app.editor.find_match_count_for_test() > 0);
    app.close_find();
    assert!(!app.find_open);
    assert_eq!(app.editor.find_match_count_for_test(), 0);
    let _frame = render(&mut app, (1280, 800));
    assert!(app.find_node.is_none(), "a closed bar takes no row");
}
