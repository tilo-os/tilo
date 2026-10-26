use super::*;
use automerge::ReadDoc;
use ui::tilo_resource::Effect as _;

use tilo_test_env::hang_bound;

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
            .is_some_and(|name| name.starts_with("Code: "))
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
        EditorDoc::splice(&mut first, &object, 0, 0, "fn pending_first() {}\n");
        EditorDoc::splice(&mut second, &object, 0, 0, "fn pending_other() {}\n");
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
fn bottom_dock_keeps_a_live_terminal_across_toggles_and_returns_focus_to_the_code() {
    // Each phase builds a fresh app (and its own terminal or headless driver).
    ide_embeds_the_real_terminal_widget();
    ide_terminal_runs_a_command_and_shows_output();
    dock_commands_open_the_requested_content_and_toggle_without_clearing_it();
    keyboard_closing_a_focused_terminal_returns_to_the_code_without_refocusing_terminal();
    the_dock_opens_and_closes_from_the_bar_the_keyboard_and_escape_inside_the_viewport();
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
    let deadline = ui::time::now() + hang_bound();
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
    let deadline = ui::time::now() + hang_bound();
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
    the_window_bar_names_the_shown_file_and_marks_unsaved_edits();
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

fn the_window_bar_names_the_shown_file_and_marks_unsaved_edits() {
    fn location(app: &CodeApp) -> Option<(String, bool)> {
        app.window_toolbar()
            .location
            .map(|location| (location.label, location.edited))
    }
    let mut sample = synced_sample();
    sample.editor(|app| assert_eq!(location(app), Some(("main.rs".to_owned(), false))));
    sample.with_doc(|app, doc| {
        app.editor.insert_for_test(doc, "X");
        app.dispatch_editor_text_edits(doc);
    });
    sample.app.settle();
    sample.editor(|app| assert_eq!(location(app), Some(("main.rs".to_owned(), true))));
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
            crate::effects::CodeEditorEffect::GO_TO_LINE
                .on(<crate::project::Project as ui::tilo_resource::Resource>::KIND)
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

/// Where the shell puts the file tree, and whether it shows it.
fn shell_sidebar(
    app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>,
) -> (ui::widget::SidebarPlacement, bool) {
    app.with_widget::<ui::widget::NavShell, _>(|shell| {
        (shell.geometry().sidebar, shell.is_sidebar_open())
    })
    .expect("the editor stands in the shell")
}

/// Activate the file tree's row `name`, as a person or an assistive
/// technology does.
fn activate_tree_row(
    app: &mut ui::framework::testing::TestApp<crate::CodeEditorShell>,
    name: &str,
) {
    let (id, _) = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::TreeItem && node.label() == Some(name)
        })
        .unwrap_or_else(|| panic!("no row named {name} in the file tree"));
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("activate {name}: {error:?}"));
    app.settle();
}

fn toggle_sidebar_command(sample: &mut crate::test_support::Sample) {
    sample.with_doc(|app, doc| app.apply_command(Command::ToggleSidebar, doc));
    sample.app.settle();
}

#[test]
fn the_file_tree_is_the_shells_sidebar() {
    use ui::widget::SidebarPlacement;
    // Case `a_wide_window_docks_it_and_the_command_hides_it`.
    {
        let mut sample = crate::test_support::sample();
        assert_eq!(
            shell_sidebar(&mut sample.app),
            (SidebarPlacement::Docked, true)
        );
        toggle_sidebar_command(&mut sample);
        assert!(!shell_sidebar(&mut sample.app).1, "the command hides it");
        toggle_sidebar_command(&mut sample);
        assert!(shell_sidebar(&mut sample.app).1, "and shows it again");
    }
    // Case `opening_a_file_keeps_a_docked_tree_and_marks_the_file`.
    {
        let mut sample = crate::test_support::sample();
        activate_tree_row(&mut sample.app, "src");
        activate_tree_row(&mut sample.app, "prime.rs");
        let active = sample.editor(|app| app.session.active_buffer().map(|b| b.name.clone()));
        assert_eq!(active.as_deref(), Some("prime.rs"));
        assert!(shell_sidebar(&mut sample.app).1, "a docked tree stays");
        let selected = sample
            .app
            .harness()
            .a11y_nodes()
            .into_iter()
            .any(|(_, node)| {
                node.role() == ui::accesskit::Role::TreeItem
                    && node.label() == Some("prime.rs")
                    && node.is_selected() == Some(true)
            });
        assert!(selected, "the file on screen is the tree's selection");
    }
    // Case `a_narrow_window_floats_it_and_opening_a_file_puts_it_away`.
    {
        let mut sample = crate::test_support::sample();
        sample.app.harness().resize((480, 800)).unwrap();
        sample.app.settle();
        assert_eq!(
            shell_sidebar(&mut sample.app),
            (SidebarPlacement::Drawer, false),
            "a narrow window opens straight to the code"
        );
        toggle_sidebar_command(&mut sample);
        assert!(shell_sidebar(&mut sample.app).1, "the command brings it");
        activate_tree_row(&mut sample.app, "src");
        activate_tree_row(&mut sample.app, "prime.rs");
        let active = sample.editor(|app| app.session.active_buffer().map(|b| b.name.clone()));
        assert_eq!(active.as_deref(), Some("prime.rs"));
        assert!(
            !shell_sidebar(&mut sample.app).1,
            "opening a file puts the drawer away"
        );
    }
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
        LspStatus::Failed(crate::lsp::LspFailure::Stopped {
            server: "rust-analyzer".to_string(),
            detail: None,
        }),
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
/// A file opened where the open itself can submit nothing — a launch hands
/// the editor its file before its record takes changes — still has its disk
/// text recorded and shown: the settlement records it once the file's entry
/// is in the project, instead of the editor staying empty over "No file
/// open" with nothing to select or edit.
#[test]
fn a_file_opened_without_a_way_to_submit_still_shows_its_text() {
    let mut sample = crate::test_support::sample();
    let file = sample.root.join("notes.py");
    std::fs::write(&file, "def greet():\n    pass\n").unwrap();
    let file = crate::disk::canonical(&file);
    sample.editor(|app| {
        let dispatch = app.dispatch.take();
        app.open_disk_file(&file).unwrap().unwrap();
        app.dispatch = dispatch;
    });
    sample.app.settle();
    sample.app.harness().relayout();
    sample.app.settle();
    let (recorded, bound) = sample.with_doc(|app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        (
            EditorDoc::read_buffer_key(doc, &app.buffer_key(node)),
            app.editor.is_bound(),
        )
    });
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

fn keyboard_closing_a_focused_terminal_returns_to_the_code_without_refocusing_terminal() {
    fn focused(driver: &ui::headless::FrameHarness) -> Option<String> {
        driver.focused_access_node().and_then(|node| node.name)
    }
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
    let code = focused(driver);
    assert_eq!(
        code.as_deref(),
        Some("Code editor"),
        "keyboard close gives the keyboard back to the code; the panel's toggle is the window bar's"
    );

    driver.set_modifiers(&["ctrl".to_string()]);
    driver.key("j");
    driver.set_modifiers(&[]);
    assert!(driver.targets().contains_key("bottom_panel"));
    assert_eq!(
        focused(driver),
        code,
        "reopening keeps the keyboard on the code instead of hidden Terminal content"
    );
}

fn the_dock_opens_and_closes_from_the_bar_the_keyboard_and_escape_inside_the_viewport() {
    let mut sample = crate::test_support::sample();
    sample.open("src/main.rs");
    sample.editor(|app| {
        assert!(!app.panel_visible);
        app.run_window_bar_event(&ui::window_toolbar::ToolbarEvent::Action {
            id: window_bar::BOTTOM_PANEL.into(),
        });
    });
    let driver = sample.app.harness();
    driver.resize((480, 900)).unwrap();
    assert!(
        driver.targets().contains_key("bottom_panel"),
        "the window bar's toggle opens the dock"
    );

    driver.key("Escape");
    assert!(
        !driver.targets().contains_key("bottom_panel"),
        "Escape closes the dock"
    );
    assert_eq!(
        driver
            .focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Code editor"),
        "Escape gives the keyboard back to the code"
    );

    driver.set_modifiers(&["ctrl".to_string()]);
    driver.key("j");
    driver.set_modifiers(&[]);
    assert!(
        driver.targets().contains_key("bottom_panel"),
        "Ctrl+J is an equivalent keyboard route"
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
    let panel_standard = driver.targets()["bottom_panel"];
    assert!(
        panel_standard.x0 >= 0.0
            && panel_standard.y0 >= 0.0
            && panel_standard.x1 <= 800.0
            && panel_standard.y1 <= 700.0,
        "open panel stays inside the resized viewport"
    );
    assert!(
        (panel_standard.height() - panel_narrow.height()).abs() < 0.5,
        "the dock keeps its height across a resize"
    );

    driver.resize((480, 900)).expect("resize back while open");
    let restored = driver.targets();
    assert!(restored.contains_key("bottom_panel"));
    assert!((restored["bottom_panel"].height() - panel_narrow.height()).abs() < 0.5);
}

/// With format on save, the person's Save waits for the language server's
/// edits and writes the formatted text; edits made meanwhile are not
/// reformatted from a stale answer but saved as they are.
#[test]
fn format_on_save_writes_what_the_formatter_answered() {
    fn at_start(text: &str) -> crate::lsp::TextEdit {
        crate::lsp::TextEdit {
            range: crate::lsp::LspRange::default(),
            new_text: text.into(),
        }
    }
    let mut sample = crate::test_support::sample();
    sample.open("README.md");
    sample
        .app
        .effect(&crate::effects::CodeEditorEffect::ToggleFormatOnSave)
        .unwrap();
    let path = sample.root.join("README.md");
    let before = std::fs::read_to_string(&path).unwrap();
    let (waited, unchanged) = sample.with_doc({
        let path = path.clone();
        move |app, doc| {
            let node = app.session.active_buffer().unwrap().node;
            app.lsp = Some(crate::lsp::LspClient::inert());
            app.lsp_uri = Some(app.node_uri(node));
            app.lsp_status = LspStatus::Ready {
                server: "test".into(),
            };
            app.apply_command(Command::Save, doc);
            let waited = app.formatting.is_some();
            let unchanged = std::fs::read_to_string(&path).unwrap();
            app.formatted = Some(Ok(vec![at_start("formatted\n")]));
            app.apply_formatted(doc);
            (waited, unchanged)
        }
    });
    assert!(waited, "the save waits for the formatter");
    assert_eq!(unchanged, before, "nothing is written before it answered");
    let formatted = format!("formatted\n{before}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), formatted);

    let stale = sample.with_doc(move |app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        app.lsp = Some(crate::lsp::LspClient::inert());
        app.lsp_uri = Some(app.node_uri(node));
        app.lsp_status = LspStatus::Ready {
            server: "test".into(),
        };
        app.apply_command(Command::Save, doc);
        let key = app.buffer_key(node);
        let text = EditorDoc::read_buffer_key(doc, &key).unwrap();
        app.dispatch_text_action(doc, key, &text, &format!("{text}typed\n"))
            .unwrap();
        app.formatted = Some(Ok(vec![at_start("stale\n")]));
        app.apply_formatted(doc);
        app.formatting.is_some()
    });
    assert!(!stale, "the answer was taken up");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{formatted}typed\n"),
        "the edit made meanwhile is saved, unformatted"
    );

    sample
        .app
        .effect(&crate::effects::CodeEditorEffect::ToggleFormatOnSave)
        .unwrap();
    let waited = sample.with_doc(move |app, doc| {
        let node = app.session.active_buffer().unwrap().node;
        app.lsp = Some(crate::lsp::LspClient::inert());
        app.lsp_uri = Some(app.node_uri(node));
        app.lsp_status = LspStatus::Ready {
            server: "test".into(),
        };
        let key = app.buffer_key(node);
        let text = EditorDoc::read_buffer_key(doc, &key).unwrap();
        app.dispatch_text_action(doc, key, &text, &format!("{text}off\n"))
            .unwrap();
        app.apply_command(Command::Save, doc);
        app.formatting.is_some()
    });
    assert!(!waited, "without format on save, Save writes at once");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .ends_with("typed\noff\n")
    );
}

#[test]
fn the_status_and_header_say_only_what_the_person_needs() {
    // Case `a_project_still_loading_reads_in_plain_words`.
    {
        let busy = |percentage| LspStatus::Busy {
            server: "rust-analyzer".to_string(),
            title: "Fetching cargo metadata".to_string(),
            detail: Some("?".to_string()),
            percentage,
        };
        assert_eq!(busy(None).label(), "Loading project\u{2026}");
        assert_eq!(busy(Some(37)).label(), "Loading project 37%");
        assert_eq!(
            LspStatus::Starting {
                server: "rust-analyzer".to_string()
            }
            .label(),
            "Loading project\u{2026}"
        );
        assert_eq!(
            LspStatus::Ready {
                server: "rust-analyzer".to_string()
            }
            .label(),
            ""
        );
    }
    // Case `opening_a_folder_is_not_narrated`.
    {
        let mut sample = crate::test_support::sample();
        let left = sample.editor(|app| app.status.left());
        assert!(!left.contains("Opened"), "status: {left}");
    }
    missing_code_help_says_so_and_keeps_why_behind_a_click();
}

fn missing_code_help_says_so_and_keeps_why_behind_a_click() {
    use super::code_help::CodeHelpAnswer;
    let mut sample = crate::test_support::sample();
    // A computer with no rust-analyzer anywhere the editor looks.
    sample.editor(|app| app.lsp_tools = Some(tilo_process::tools::ToolDirs::default()));
    sample
        .app
        .settings::<crate::settings::CodeSettings>()
        .set(&crate::settings::CodeSettings {
            language_servers: true,
            ..Default::default()
        })
        .unwrap();
    sample.open("src/main.rs");
    let status = sample.editor(|app| app.lsp_status.clone());
    assert_eq!(
        status,
        LspStatus::Failed(crate::lsp::LspFailure::NotInstalled {
            server: "rust-analyzer".to_owned(),
        }),
        "the launch reports the server missing, as itself"
    );
    let readout = "Rust: code help unavailable";
    let bounds = sample
        .app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::Button && node.label() == Some(readout)
        })
        .and_then(|(_, node)| node.bounds())
        .expect("the readout is a button where it is painted");
    sample.app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    sample.app.settle();
    let (question, body) = sample.editor(|app| {
        let question = app.question();
        (question, question.map(|q| app.wording(q).body))
    });
    assert_eq!(question, Some(super::dialogs::Question::CodeHelp));
    let body = body.unwrap_or_default();
    assert!(
        body.starts_with("rust-analyzer isn't installed on this computer.")
            && body.contains("rustup component add rust-analyzer"),
        "the sheet says why and how to install it: {body}"
    );
    let answers = sample.editor(|app| app.code_help_answers());
    assert_eq!(
        answers,
        [CodeHelpAnswer::TryAgain, CodeHelpAnswer::Close],
        "nothing more to copy than the sheet says"
    );
    let (asked, failed) = sample.editor(|app| {
        app.answer_code_help(0);
        (app.question(), app.lsp_failed.len())
    });
    assert_eq!(
        (asked, failed),
        (None, 0),
        "trying again forgets the failure"
    );
    let detail = "signal: 9 (SIGKILL)";
    let (body, answers) = sample.editor(|app| {
        app.lsp_status = LspStatus::Failed(crate::lsp::LspFailure::Stopped {
            server: "rust-analyzer".to_owned(),
            detail: Some(detail.to_owned()),
        });
        (
            app.wording(super::dialogs::Question::CodeHelp).body,
            app.code_help_answers(),
        )
    });
    assert_eq!(
        body.lines().next(),
        Some("rust-analyzer stopped. Suggestions, problems and go to definition come from it.")
    );
    assert!(
        !body.contains(detail),
        "the system's words stay behind Copy details: {body}"
    );
    assert!(answers.contains(&CodeHelpAnswer::CopyDetails));
}

/// The editor's text size is the person's: a zoom step grows the code and
/// its line numbers, is kept in their settings, and reset goes back to the
/// theme's own size.
#[test]
fn the_editor_text_zooms_and_remembers_its_size() {
    use ui::content_zoom::{ContentZoom as _, ZoomStep};
    let mut sample = synced_sample();
    let before = sample.editor(|app| app.editor.text_zoom());
    let factor = sample
        .editor(|app| app.zoom_content(ZoomStep::In))
        .expect("the editor's text takes the zoom");
    assert!(factor > before, "the code grew: {before} -> {factor}");
    let stored = sample.editor(|app| {
        app.settings
            .as_ref()
            .expect("the editor is mounted with its settings")
            .get()
            .editor_zoom
    });
    assert!(
        (stored - factor).abs() < f32::EPSILON,
        "the chosen size is remembered"
    );
    let reset = sample
        .editor(|app| app.zoom_content(ZoomStep::Reset))
        .expect("reset is the editor's own size");
    assert!((reset - 1.0).abs() < 1e-6);
}
