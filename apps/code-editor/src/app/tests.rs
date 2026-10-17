use super::*;
use automerge::ReadDoc;

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

#[test]
fn peer_hardware_language_metadata_survives_projection_without_a_source_extension() {
    for language in [Language::Verilog, Language::SystemVerilog] {
        let mut sample = synced_sample();
        let id = "00000000-0000-4000-8000-0000000000ed";
        sample.app.merge_peer_edit(|doc| {
            crate::test_support::peer_file(
                doc,
                id,
                "hardware.txt",
                language.label(),
                "module mac;\nendmodule\n",
            );
        });
        sample.app.settle();
        sample.editor(|app| {
            let node = app
                .session
                .project
                .node_for_resource(id)
                .expect("peer file projected");
            let file = app
                .session
                .project
                .get(node)
                .expect("projected file exists");
            assert!(
                matches!(
                    &file.kind,
                    crate::model::NodeKind::File { language: actual, .. } if *actual == language
                ),
                "persisted {language:?} wins over the .txt extension"
            );
        });
    }
}

#[test]
fn hardware_dialects_use_standard_language_server_ids() {
    assert_eq!(CodeApp::language_id(Language::Verilog), "verilog");
    assert_eq!(
        CodeApp::language_id(Language::SystemVerilog),
        "systemverilog"
    );
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
    assert!(
        node.children
            .iter()
            .any(|child| { child.name.as_deref().is_some_and(|name| name == "main.rs") })
    );
}

#[test]
fn search_projection_rebuilds_only_on_revisions_or_an_exhausted_generation() {
    // Each phase builds a fresh synced sample; rebuild counters must not leak.
    search_projection_rebuilds_only_for_document_project_or_result_revisions();
    exhausted_search_projection_generation_rebuilds_instead_of_reusing_a_cursor();
}

#[test]
fn navigation_and_result_changes_reuse_symbols_but_pending_forks_do_not() {
    use automerge::transaction::Transactable;
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let before = app.search_refresh_stats();
        app.invalidate_search_projection();
        app.refresh_search_entries(doc.doc());
        assert_eq!(app.search_refresh_stats().buffer_reads, before.buffer_reads);
        let node = app.active_file_node().expect("sample source");
        let key = app.buffer_key(node);
        let object = EditorDoc::buffer_text_key(doc, &key).expect("authoritative text");
        let mut first = doc.doc().clone();
        let mut second = doc.doc().clone();
        first
            .splice_text(&object, 0, 0, "fn pending_first() {}\n")
            .unwrap();
        second
            .splice_text(&object, 0, 0, "fn pending_other() {}\n")
            .unwrap();
        assert_eq!(first.pending_ops(), second.pending_ops());
        assert!(first.pending_ops() > 0);
        assert_eq!(first.committed_heads(), second.committed_heads());
        app.refresh_search_entries(&first);
        assert!(
            app.search_symbols
                .iter()
                .any(|symbol| symbol.name == "pending_first")
        );
        app.refresh_search_entries(&second);
        assert!(
            app.search_symbols
                .iter()
                .any(|symbol| symbol.name == "pending_other")
        );
        assert!(
            !app.search_symbols
                .iter()
                .any(|symbol| symbol.name == "pending_first")
        );
        app.refresh_search_entries(doc.doc());
        assert!(
            !app.search_symbols
                .iter()
                .any(|symbol| symbol.name.starts_with("pending_"))
        );
        assert_eq!(
            first.pending_ops(),
            second.pending_ops(),
            "reading did not commit either fork"
        );
    });
}

fn search_projection_rebuilds_only_for_document_project_or_result_revisions() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let initial = app.search_refresh_stats();
        app.sync(doc.doc(), &automerge::ROOT);
        app.sync(doc.doc(), &automerge::ROOT);
        let idle = app.search_refresh_stats();
        assert_eq!(idle.rebuilds, initial.rebuilds);
        assert_eq!(idle.buffer_reads, initial.buffer_reads);
        assert_eq!(idle.indexed_records, initial.indexed_records);
        assert!(idle.skipped >= initial.skipped.saturating_add(2));

        let node = app.active_file_node().expect("sample buffer");
        let key = app.buffer_key(node);
        let base = EditorDoc::read_buffer_key(doc, &key).expect("buffer text");
        app.dispatch_text_action(
            doc,
            key,
            &base,
            &format!("fn revision_gate_needle() {{}}\n{base}"),
        )
        .unwrap();
        app.sync(doc.doc(), &automerge::ROOT);
        let after_document = app.search_refresh_stats();
        assert_eq!(after_document.rebuilds, idle.rebuilds.saturating_add(1));
        assert!(
            ui::search::SearchProvider::search(
                &ui::framework::testing::search_source::<crate::project::Project>(
                    app.code_search_entries()
                ),
                "revision_gate_needle"
            )
            .iter()
            .any(|hit| hit.kind == crate::search_entries::SYMBOL_KIND)
        );

        let root = app.session.project.root;
        app.session.project.add_file(
            root,
            "project-revision-needle.rs",
            "fn project_revision_needle() {}\n",
        );
        app.invalidate_search_projection();
        app.sync(doc.doc(), &automerge::ROOT);
        let after_project = app.search_refresh_stats();
        assert_eq!(
            after_project.rebuilds,
            after_document.rebuilds.saturating_add(1)
        );
        app.search_hits.push(search::Hit {
            path: "/virtual/project-revision-needle.rs".into(),
            rel: "project-revision-needle.rs".into(),
            line: 1,
            col: 0,
            len: 6,
            preview: "result_revision_needle".into(),
            preview_offset: 0,
        });
        app.invalidate_search_projection();
        app.sync(doc.doc(), &automerge::ROOT);
        let after_results = app.search_refresh_stats();
        assert_eq!(
            after_results.buffer_reads, after_project.buffer_reads,
            "new result previews do not rescan authoritative source bodies"
        );
        assert_eq!(
            after_results.rebuilds,
            after_project.rebuilds.saturating_add(1)
        );
        assert!(
            ui::search::SearchProvider::search(
                &ui::framework::testing::search_source::<crate::project::Project>(
                    app.code_search_entries()
                ),
                "result_revision_needle"
            )
            .iter()
            .any(|hit| hit.kind == crate::search_entries::RESULT_KIND)
        );
    });
}

fn exhausted_search_projection_generation_rebuilds_instead_of_reusing_a_cursor() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        app.search_projection_local_generation = Some(u64::MAX);
        app.invalidate_search_projection();
        assert_eq!(app.search_projection_local_generation, None);
        let before = app.search_refresh_stats().rebuilds;
        app.refresh_search_entries(doc);
        app.refresh_search_entries(doc);
        assert_eq!(
            app.search_refresh_stats().rebuilds,
            before.saturating_add(2),
            "an exhausted generation must never make a stale catalogue look current"
        );
    });
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
    // app's update, settle and sync passes, and prove the shell's
    // output flowed back into the IDE's integrated terminal grid.
    let mut app = CodeApp::with_sample();
    app.shots_open_terminal();
    let mut state = ui::State::new();
    app.update(&state.doc, &state.root);
    app.settle(&mut ui::widget::Edits::lend(&mut state.doc), &state.root);
    app.sync(&state.doc, &state.root);
    app.shots_terminal_input("printf 'IDE_TERM_OK\\n'\n");
    let mut found = false;
    let deadline = ui::time::now() + HANG_BOUND;
    while ui::time::now() < deadline {
        app.update(&state.doc, &state.root);
        app.settle(&mut ui::widget::Edits::lend(&mut state.doc), &state.root);
        app.sync(&state.doc, &state.root);
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

#[test]
fn the_embedded_terminal_is_the_projects_terminal_child() {
    let mut app = ui::framework::testing::TestApp::<crate::CodeEditorShell>::new().unwrap();
    crate::test_support::editor(&mut app, CodeApp::shots_open_terminal);
    app.settle();
    crate::test_support::editor(&mut app, |app| {
        app.shots_terminal_input("printf 'CHILD_TERM_OK\\n'\n");
    });
    // The shell's output stays in the terminal; the project records the
    // session the process took.
    let shown = |app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>| {
        let recorded = app
            .state()
            .terminal_panel
            .as_ref()
            .is_some_and(|session| !session.pane_id().is_empty());
        recorded
            && crate::test_support::editor(app, |app| app.shots_terminal_grid_text())
                .is_some_and(|text| text.contains("CHILD_TERM_OK"))
    };
    let deadline = ui::time::now() + HANG_BOUND;
    while !shown(&mut app) && ui::time::now() < deadline {
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        shown(&mut app),
        "the shell's output shows, and the session is the one the project records"
    );
    let published = app.harness().resources();
    let child = published
        .iter()
        .flat_map(|resource| resource.children.iter())
        .find(|child| child.id.as_str() == crate::resource::TERMINAL_RESOURCE_ID)
        .expect("the project publishes its terminal");
    assert_eq!(child.kind, "code_editor.project.terminal");
    app.read_document(|doc| {
        assert!(
            doc.get(automerge::ROOT, "terminal_title")
                .unwrap()
                .is_none()
                && doc
                    .get(automerge::ROOT, "terminal_local_session")
                    .unwrap()
                    .is_none(),
            "nothing of the session is written beside the project's files"
        );
    });
}

fn ensure_buffers_seeds_active_text() {
    let mut sample = synced_sample();
    let (text, bound) = sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        (
            EditorDoc::read_buffer_key(doc, &app.buffer_key(node)).unwrap(),
            app.editor.is_bound(),
        )
    });
    assert!(text.contains("fn main"));
    assert!(bound);
}

#[test]
fn a_window_shows_one_file_and_asks_before_dropping_unsaved_work() {
    // Each phase builds a fresh sample app.
    opening_another_file_replaces_the_shown_one();
    opening_another_file_over_unsaved_edits_asks_first();
    the_header_names_the_shown_file_and_marks_unsaved_edits();
    the_editor_has_no_tab_strip_and_leaves_ctrl_w_to_the_desktop();
    quick_open_and_go_to_line_open_the_host_palette();
}

fn sample_node(app: &CodeApp, name: &str) -> crate::model::NodeId {
    app.session
        .project
        .nodes
        .values()
        .find(|n| n.name == name)
        .unwrap()
        .id
}

fn opening_another_file_replaces_the_shown_one() {
    let mut app = CodeApp::with_sample();
    let main = app.session.active_buffer().unwrap().node;
    let prime = sample_node(&app, "prime.rs");
    app.open_node(prime);
    assert_eq!(app.session.active_buffer().unwrap().name, "prime.rs");
    assert!(
        app.session.buffer_of(main).is_none(),
        "a window shows one file; main.rs is not kept open behind it"
    );
    assert!(
        app.close_prompt.is_none(),
        "a clean file is replaced without asking"
    );
}

fn opening_another_file_over_unsaved_edits_asks_first() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        app.editor.insert_for_test(doc, "X");
        app.dispatch_editor_text_edits(doc);
    });
    sample.app.settle();
    let main = sample.editor(|app| {
        let shown = app.session.active_buffer().unwrap();
        assert!(shown.dirty, "the buffer now has unsaved edits");
        shown.node
    });

    sample.open("src/prime.rs");
    sample.editor(move |app| {
        assert_eq!(
            app.session.active_buffer().unwrap().node,
            main,
            "unsaved work is not dropped out from under the user"
        );
        assert!(app.close_prompt.is_some(), "the save prompt is raised");
    });

    sample.with_doc(|app, doc| app.resolve_close_prompt(doc, CloseChoice::Cancel));
    sample.editor(move |app| {
        assert_eq!(
            app.session.active_buffer().unwrap().node,
            main,
            "cancel keeps it"
        );
    });

    sample.open("src/prime.rs");
    sample.with_doc(|app, doc| app.resolve_close_prompt(doc, CloseChoice::Discard));
    sample.app.settle();
    sample.editor(|app| {
        assert_eq!(
            app.session.active_buffer().unwrap().name,
            "prime.rs",
            "discarding shows the file that was asked for"
        );
        assert!(app.close_prompt.is_none());
    });
}

fn the_header_names_the_shown_file_and_marks_unsaved_edits() {
    let mut sample = synced_sample();
    sample.editor(|app| {
        app.refresh_header();
        assert_eq!(*app.header_label.borrow(), "main.rs");
    });
    sample.with_doc(|app, doc| {
        app.editor.insert_for_test(doc, "X");
        app.dispatch_editor_text_edits(doc);
    });
    sample.app.settle();
    sample.editor(|app| {
        app.refresh_header();
        assert_eq!(*app.header_label.borrow(), "main.rs \u{2022}");
    });
}

fn the_editor_has_no_tab_strip_and_leaves_ctrl_w_to_the_desktop() {
    fn has_tab_bar(node: &ui::a11y::AccessNode) -> bool {
        node.role == ui::a11y::Role::TabBar || node.children.iter().any(has_tab_bar)
    }
    fn walk(widget: &dyn Widget, found: &mut bool) {
        if widget
            .accessibility()
            .is_some_and(|node| has_tab_bar(&node))
        {
            *found = true;
        }
        for child in widget.children() {
            walk(child, found);
        }
    }
    let mut sample = synced_sample();
    let found = sample.editor(|app| {
        let mut found = false;
        walk(app, &mut found);
        found
    });
    assert!(
        !found,
        "documents are desktop tabs; the editor draws no tab strip"
    );
    assert!(
        crate::effects::CodeEditorEffect::specs()
            .iter()
            .all(|spec| !spec.id.contains("tab")),
        "closing is the desktop's tab lifecycle, not an editor command"
    );
}

fn quick_open_and_go_to_line_open_the_host_palette() {
    let mut sample = synced_sample();
    let _ = ui::host_palette::take();
    sample.app.harness().set_modifiers(&["ctrl".to_owned()]);
    sample.app.harness().key("p");
    assert_eq!(
        sample
            .app
            .harness()
            .focused_access_node()
            .expect("host search focus")
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some("Search"))
            .expect("host search query")
            .role,
        ui::a11y::Role::TextInput,
        "quick open exposes the platform search query over project entries"
    );
    sample.app.harness().set_modifiers(&[]);
    sample.app.harness().key("Escape");
    sample.app.harness().set_modifiers(&["ctrl".to_owned()]);
    sample.app.harness().key("g");
    sample.app.harness().set_modifiers(&[]);
    assert_eq!(
        ui::host_palette::take(),
        Some(ui::host_palette::PaletteRequest::Action(
            ui::tilo_actions::ActionAddress::new(
                <crate::project::Project as ui::tilo_resource::Resource>::KIND,
                "go-to-line"
            )
        )),
        "go to line asks for its line on the published effect"
    );
    let entries =
        <crate::project::Project as ui::tilo_resource::Resource>::search(&sample.app.state());
    assert!(
        entries.iter().any(|entry| entry.title == "main.rs"),
        "the project's files are its search entries"
    );
}

#[test]
fn editor_commands_edit_through_the_crdt_and_move_the_caret() {
    // Each phase builds a fresh synced sample.
    editing_routes_through_crdt();
    duplicate_line_repeats_it_below();
    move_line_down_swaps_with_the_next();
    delete_line_removes_it();
    toggle_comment_comments_then_uncomments();
    go_to_line_moves_the_caret();
    find_highlights_and_navigates_matches();
}

fn editing_routes_through_crdt() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        let before = EditorDoc::read_buffer_key(doc, &app.buffer_key(node)).unwrap();
        app.editor.insert_for_test(doc, "X");
        app.dispatch_editor_text_edits(doc);
        let after = EditorDoc::read_buffer_key(doc, &app.buffer_key(node)).unwrap();
        assert_eq!(after.chars().count(), before.chars().count() + 1);
    });
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
    app.apply_command(
        Command::ToggleSidebar,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert!(!app.sidebar_open());
    app.apply_command(
        Command::ToggleSidebar,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert!(app.sidebar_open());
}

#[test]
fn project_files_are_created_by_durable_actions_and_adopted_from_peers() {
    // Each phase builds a fresh sample app and document.
    new_file_command_dispatches_the_durable_create_file_action();
    a_collaborative_file_is_adopted_as_a_tracked_file();
}

fn new_file_command_dispatches_the_durable_create_file_action() {
    // The on-screen "New file" command creates the file through the
    // project's own action, under a UUID minted when it is authored (never a
    // local project-arena id), and opens it.
    let mut sample = synced_sample();
    let created = sample.with_doc(|app, doc| {
        let before: std::collections::HashSet<String> = EditorDoc::files(doc)
            .into_iter()
            .map(|file| file.id)
            .collect();
        app.apply_command(Command::NewFile, doc);
        let created = EditorDoc::files(doc)
            .into_iter()
            .find(|file| !before.contains(&file.id))
            .expect("the action created a file record");
        assert!(EditorDoc::read_buffer_key(doc, &created.id).is_some());
        created
    });
    assert!(uuid::Uuid::parse_str(&created.id).is_ok());
    sample.app.settle();
    let active = sample.editor(|app| app.session.active_buffer().map(|b| b.name.clone()));
    assert_eq!(active, Some(created.name), "the new file is open");
}

fn a_collaborative_file_is_adopted_as_a_tracked_file() {
    // A peer's durable file record is surfaced by the next sync. The
    // resource UUID, rather than a coincidental local arena number, is the
    // thing the projection follows.
    let mut sample = synced_sample();
    let before = sample.editor(|app| app.session.project.nodes.len());
    let orphan = "00000000-0000-4000-8000-0000000000ee";
    sample.app.merge_peer_edit(|doc| {
        crate::test_support::peer_file(doc, orphan, "peer.rs", "Rust", "// created out of band\n");
    });
    sample.app.settle();
    let (adopted, after) = sample.editor(|app| {
        (
            app.session.project.node_for_resource(orphan).is_some(),
            app.session.project.nodes.len(),
        )
    });
    assert!(adopted, "the document buffer was adopted as a file");
    assert_eq!(after, before + 1);
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
    app.ensure_buffers(&doc);
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
    let doc = AutoCommit::new();
    app.ensure_buffers(&doc);

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
fn failed_save_as_preserves_the_original_destination_and_conflict_stamp() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        app.editor.insert_for_test(doc, "// unsaved\n");
        app.dispatch_editor_text_edits(doc);
    });
    sample.app.settle();
    let directory = tempfile::tempdir().unwrap();
    // A directory cannot be replaced with a regular file, even when the
    // tests run with privileges that bypass file permission bits.
    let destination = directory.path().to_path_buf();
    sample.with_doc(move |app, doc| {
        let before = app.session.active_buffer().unwrap().clone();
        let original = before.path.clone().unwrap();
        let key = app.buffer_key(before.node);
        let text = EditorDoc::read_buffer_key(doc, &key).unwrap();
        assert!(before.dirty);
        app.apply_portal_outcome(
            doc,
            Some(PortalIntent::SaveAs(before.node)),
            ui::portal::PortalOutcome::Opened(destination),
        );
        let after = app.session.active_buffer().unwrap();
        assert_eq!(after.path, before.path);
        assert_eq!(after.name, before.name);
        assert_eq!(after.language, before.language);
        assert_eq!(after.stamp, before.stamp);
        assert_eq!(after.saved_text, before.saved_text);
        assert!(after.dirty);
        assert_eq!(app.session.project.disk_path_of(before.node), before.path);
        assert_eq!(EditorDoc::read_buffer_key(doc, &key).unwrap(), text);
        // The old freshness guard must still protect external edits after
        // the failed Save As, rather than saving to the rejected destination.
        std::fs::write(&original, "externally changed source\n").unwrap();
        assert!(!app.save_buffer(doc, before.node, false));
        assert!(app.conflict.is_some());
        assert_eq!(
            std::fs::read_to_string(original).unwrap(),
            "externally changed source\n"
        );
    });
}

#[test]
fn successful_save_as_writes_current_text_before_adopting_the_destination() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        app.editor.insert_for_test(doc, "// unsaved\n");
        app.dispatch_editor_text_edits(doc);
    });
    sample.app.settle();
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("saved.py");
    sample.with_doc(move |app, doc| {
        let before = app.session.active_buffer().unwrap().clone();
        let original = before.path.unwrap();
        let original_text = std::fs::read_to_string(&original).unwrap();
        let text = EditorDoc::read_buffer_key(doc, &app.buffer_key(before.node)).unwrap();
        app.apply_portal_outcome(
            doc,
            Some(PortalIntent::SaveAs(before.node)),
            ui::portal::PortalOutcome::Opened(destination.clone()),
        );
        let after = app.session.active_buffer().unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), text);
        assert_eq!(std::fs::read_to_string(original).unwrap(), original_text);
        assert_eq!(after.path.as_ref(), Some(&destination));
        assert_eq!(after.language, Language::Python);
        assert_eq!(after.saved_text, text);
        assert!(!after.dirty);
        assert_eq!(after.stamp, Some(disk::Stamp::of(&destination).unwrap()));
        assert_eq!(
            app.session.project.disk_path_of(before.node),
            Some(destination)
        );
    });
}

#[test]
fn disk_files_load_stat_and_save_back_through_the_crdt() {
    // Each phase uses its own tempdir, app and document.
    a_document_from_a_previous_session_still_stats_the_file();
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
    let launch = [file.to_str().unwrap()];
    let mut app =
        ui::framework::testing::TestApp::<crate::CodeEditorShell>::launched(&launch).unwrap();

    // Unsaved edits the project's document keeps from this session.
    let key = crate::test_support::editor(&mut app, |app| {
        app.buffer_key(app.session.active_buffer().unwrap().node)
    });
    app.invoke(
        ui::tilo_resource::Invoke::apply(&actions::text_edit_action(
            key.clone(),
            "fn main() { on_disk() }\n",
            "fn main() { from_last_session() }\n",
        ))
        .unwrap(),
    )
    .unwrap();

    // The next launch finds that text in the document.
    let mut app = app.relaunch(&launch).unwrap();
    app.settle();
    let (stamped, path, saved, dirty) = crate::test_support::editor(&mut app, |app| {
        let buf = app.session.active_buffer().expect("still open");
        (
            buf.stamp.is_some(),
            buf.path.clone(),
            buf.saved_text.clone(),
            buf.dirty,
        )
    });
    assert!(
        stamped,
        "the file was stat'ed, so a save has something to check against"
    );
    assert_eq!(
        path.as_deref().map(crate::disk::canonical),
        Some(crate::disk::canonical(&file)),
        "and the buffer knows which file it writes back to"
    );
    assert_eq!(
        saved, "fn main() { on_disk() }\n",
        "what counts as 'unmodified' is the bytes on disk, not the document"
    );
    assert!(
        dirty,
        "the restored text differs from the file, so it reads as unsaved \
             edits rather than as agreement"
    );
    // The restored text is kept — a previous session's unsaved work, or a
    // peer's in-flight edits, are not ours to throw away.
    assert_eq!(
        app.read_document(|doc| EditorDoc::read_buffer_key(doc, &key))
            .as_deref(),
        Some("fn main() { from_last_session() }\n")
    );
}

/// The structural scan defers to a language server that has published for
/// the buffer, and only to one that has.
fn a_published_diagnostic_retires_the_bracket_scan() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();
    app.ensure_buffers(&doc);
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

/// The sample project with `src/main.rs` open and bound, its text read from
/// disk.
/// The text a file is opened with reaches the document even when its first
/// recording never arrives — at a launch the editor is handed its file before
/// its record takes changes — so the editor shows the file instead of an
/// empty "No file open" with nothing to select or edit.
#[test]
fn an_opened_files_text_lost_on_the_way_is_recorded_again() {
    let mut sample = crate::test_support::sample();
    let file = sample.root.join("notes.py");
    std::fs::write(&file, "def greet():\n    pass\n").unwrap();
    let file = crate::disk::canonical(&file);
    sample.editor(|app| {
        // The submission the open makes goes nowhere.
        let dispatch = app.dispatch.take();
        app.open_disk_file(&file).unwrap();
        app.dispatch = dispatch;
    });
    sample.app.settle();
    let text = |sample: &mut crate::test_support::Sample| {
        sample.with_doc(|app, doc| {
            let node = app.session.active_buffer().unwrap().node;
            (
                EditorDoc::read_buffer_key(doc, &app.buffer_key(node)),
                app.editor.is_bound(),
            )
        })
    };
    let (lost, _) = text(&mut sample);
    assert_eq!(lost, None, "the first recording never arrived");

    std::thread::sleep(std::time::Duration::from_millis(1100));
    sample.app.harness().relayout();
    sample.app.settle();
    let (recorded, bound) = text(&mut sample);
    assert_eq!(recorded.as_deref(), Some("def greet():\n    pass\n"));
    assert!(bound, "the editor shows the file");
}

fn synced_sample() -> crate::test_support::Sample {
    let mut sample = crate::test_support::sample();
    sample.open("src/main.rs");
    sample.app.settle();
    sample
}

fn buffer_lines(app: &CodeApp, doc: &AutoCommit, node: TreeId) -> Vec<String> {
    EditorDoc::read_buffer_key(doc, &app.buffer_key(node))
        .unwrap()
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// Find populates real matches over the active buffer, hands them to the
/// editor's highlight layer, and stepping to a match selects it.
fn find_highlights_and_navigates_matches() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        app.open_find();
        app.find.set_query("primes");
        app.drive_find(doc);

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

        // Stepping to the second match navigates the caret onto it and marks it
        // as the current (filled) highlight.
        app.focus_match(Some(1));
        let m = app.find_matches[1];
        assert_eq!(app.find_current, Some(1), "the second match is now current");
        assert_eq!(
            app.editor.primary_caret(),
            (m.line, m.col),
            "the caret navigated onto the match"
        );
    });
}

/// Go-to-line moves the caret to the requested 1-based line and clamps past
/// the end of the buffer.
fn go_to_line_moves_the_caret() {
    let mut sample = synced_sample();
    sample.with_doc(|app, _doc| {
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
    });
}

fn duplicate_line_repeats_it_below() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        let before = buffer_lines(app, doc, node);

        app.editor.move_caret_to(0, 0);
        app.pending_command = Some(Command::DuplicateLine);
        app.sync(doc.doc(), &automerge::ROOT);
        assert_eq!(
            buffer_lines(app, doc, node),
            before,
            "projection leaves the queued edit unwritten"
        );
        assert!(matches!(app.pending_command, Some(Command::DuplicateLine)));
        app.settle(doc, &automerge::ROOT);

        let after = buffer_lines(app, doc, node);
        assert_eq!(after.len(), before.len() + 1, "one line added");
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1], before[0], "the copy sits directly below");
        assert_eq!(app.editor.primary_caret().0, 1, "caret rode onto the copy");
    });
}

fn move_line_down_swaps_with_the_next() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        let before = buffer_lines(app, doc, node);

        app.editor.move_caret_to(0, 0);
        app.editor.move_lines(doc, false);
        app.dispatch_editor_text_edits(doc);

        let after = buffer_lines(app, doc, node);
        assert_eq!(after.len(), before.len(), "no lines gained or lost");
        assert_eq!(after[0], before[1], "line 1 moved up");
        assert_eq!(after[1], before[0], "line 0 moved down");
        assert_eq!(app.editor.primary_caret().0, 1, "caret followed the line");
    });
}

fn delete_line_removes_it() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        let before = buffer_lines(app, doc, node);

        app.editor.move_caret_to(0, 0);
        app.editor.delete_lines(doc);
        app.dispatch_editor_text_edits(doc);

        let after = buffer_lines(app, doc, node);
        assert_eq!(after.len(), before.len() - 1, "one line removed");
        assert_eq!(after[0], before[1], "the following line is now first");
    });
}

fn toggle_comment_comments_then_uncomments() {
    let mut sample = synced_sample();
    sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        // Replace the buffer with a content-stable single line of Rust.
        let key = app.buffer_key(node);
        let base = EditorDoc::read_buffer_key(doc, &key).unwrap();
        app.dispatch_text_action(doc, key, &base, "let x = 1;\n")
            .unwrap();
        app.editor.sync(doc.doc(), &automerge::ROOT);

        app.editor.move_caret_to(0, 0);
        app.editor.toggle_line_comment(doc);
        app.dispatch_editor_text_edits(doc);
        assert_eq!(
            EditorDoc::read_buffer_key(doc, &app.buffer_key(node)).as_deref(),
            Some("// let x = 1;\n"),
            "the line gained a Rust line comment"
        );

        app.editor.move_caret_to(0, 0);
        app.editor.toggle_line_comment(doc);
        app.dispatch_editor_text_edits(doc);
        assert_eq!(
            EditorDoc::read_buffer_key(doc, &app.buffer_key(node)).as_deref(),
            Some("let x = 1;\n"),
            "toggling again removed it"
        );
    });
}

fn dock_commands_open_the_requested_content_and_toggle_without_clearing_it() {
    let mut app = CodeApp::with_sample();
    let mut doc = AutoCommit::new();

    app.apply_command(
        Command::GoToProblems,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert!(app.panel_visible);
    assert_eq!(app.panel.active_tab(), PanelTab::Problems);

    app.apply_command(
        Command::ToggleBottomPanel,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert!(!app.panel_visible);
    app.apply_command(
        Command::ToggleBottomPanel,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert!(app.panel_visible);
    assert_eq!(app.panel.active_tab(), PanelTab::Problems);

    app.apply_command(
        Command::GoToTerminal,
        &mut ui::widget::Edits::lend(&mut doc),
    );
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
    app.apply_command(
        Command::ToggleBottomPanel,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.apply_command(
        Command::ToggleBottomPanel,
        &mut ui::widget::Edits::lend(&mut doc),
    );
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
    app.apply_command(
        Command::GoToProblems,
        &mut ui::widget::Edits::lend(&mut doc),
    );

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
    let mut sample = crate::test_support::sample();
    sample.open("src/main.rs");
    sample.editor(CodeApp::shots_open_terminal);
    let driver = sample.app.harness();
    driver.resize((480, 900)).unwrap();

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

    let mut sample = crate::test_support::sample();
    sample.open("src/main.rs");
    let driver = sample.app.harness();
    driver.resize((480, 900)).unwrap();
    let targets = driver.targets();
    let header = targets
        .get("code-editor.file")
        .copied()
        .expect("file header target");
    let trigger = targets
        .get("code.bottom-panel-toggle")
        .copied()
        .expect("retained dock trigger");
    // The trigger shares the chrome row with the file header, centred on the
    // same line, and never overlaps it.
    assert!(
        (trigger.center().y - header.center().y).abs() < 1.0,
        "trigger and file name share the chrome row: {trigger:?} vs {header:?}"
    );
    assert!(
        header.x1 <= trigger.x0,
        "file header and dock trigger must not overlap"
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
    let header_standard = resized["code-editor.file"];
    let trigger_standard = resized["code.bottom-panel-toggle"];
    assert!(
        panel_standard.x0 >= 0.0
            && panel_standard.y0 >= 0.0
            && panel_standard.x1 <= 800.0
            && panel_standard.y1 <= 700.0,
        "open panel stays inside the resized viewport"
    );
    assert_eq!(panel_standard.height(), panel_narrow.height());
    // Same chrome row after the resize, still clear of the file header.
    assert!((trigger_standard.center().y - header_standard.center().y).abs() < 1.0);
    assert!(header_standard.x1 <= trigger_standard.x0);
    let resized_snapshot = driver.inspection();
    let resized_trigger = trigger_semantics(&resized_snapshot);
    assert_eq!(resized_trigger.name.as_deref(), Some("Hide panel"));
    assert_eq!(resized_trigger.state.expanded, Some(true));
    assert!(resized_trigger.state.selected);

    driver.resize((480, 900)).expect("resize back while open");
    let restored = driver.targets();
    assert!(restored.contains_key("bottom_panel"));
    assert_eq!(restored["bottom_panel"].height(), panel_narrow.height());
    assert!(restored["code-editor.file"].x1 <= restored["code.bottom-panel-toggle"].x0);
}
