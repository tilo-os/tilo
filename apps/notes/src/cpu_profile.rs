//! Test-only observations around the actual Notes projection functions.
//! No timing budgets: the canonical mutation assertions establish the subject.

use ui::tilo_resource::profiling::Sample;

/// Run `operation` and return its wall time with every scope it ran.
fn observe(operation: impl FnOnce()) -> (u128, Vec<Sample>) {
    ui::tilo_resource::profiling::observe(|| {
        let start = ui::time::now();
        operation();
        start.elapsed().as_nanos()
    })
}

pub(super) fn fixture(
    pages: usize,
) -> (
    ui::framework::testing::TestApp<crate::NotesShell>,
    crate::PageId,
) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    let target = crate::PageId::new_v4();
    // Author the corpus on a real collaborator replica, then merge once.
    // Each real action still commits; setup does not render 255 intermediate frames.
    let mut peer = app.peer_document();
    for index in 0..pages {
        let page = if index == 0 {
            target
        } else {
            crate::PageId::new_v4()
        };
        apply_fixture_action(
            &mut peer,
            &crate::actions::create_page_action(page, None, &format!("CPU fixture {index}")),
        );
        if index != 0 {
            let body = format!("Authored fixture {index}. {}", "notebook text ".repeat(160));
            let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
            apply_fixture_action(&mut peer, &crate::actions::write_body_action(page, &blocks));
        }
    }
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.read_document(crate::read::list_pages).len(), pages);
    app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(target))
        .unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted editor");
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    (app, target)
}

fn apply_fixture_action(doc: &mut ui::tilo_resource::Doc, action: &crate::actions::Action) {
    tilo_actions::apply_local(doc, action, crate::actions::encode(action)).unwrap();
}

#[test]
fn mounted_typing_observes_canonical_projection_scaling() {
    for pages in [1, 16, 128] {
        let setup = ui::time::now();
        let (mut app, target) = fixture(pages);
        let setup_ns = setup.elapsed().as_nanos();
        app.harness().begin_profiling();
        for sample in 0..22 {
            // One recorder sees the Notes, framework and resource scopes.
            let (elapsed, rows) = observe(|| app.harness().type_text("a"));
            let (framework_rows, resource_rows) = (&rows, &rows);
            // This maintained observer pumps pending wakes. Measure its work
            // separately; captured passes belong to the combined interval.
            let mut frames = Vec::new();
            let (drain_elapsed, drain_rows) = observe(|| {
                frames = app.harness().take_profile_samples();
            });
            let text = app.read_document(|doc| {
                crate::read::page_block_snapshots(doc, target)
                    .into_iter()
                    .map(|(_, text)| text)
                    .collect::<String>()
            });
            assert_eq!(text, "a".repeat(sample + 1), "real editor mutation");
            for phase in ["surface_sync", "search_blocks"] {
                assert!(rows.iter().any(|row| row.phase == phase), "{phase}");
            }
            assert!(!frames.is_empty(), "actual frame-pass observations");
            for phase in [
                "framework.publication",
                "framework.resource_tree",
                "framework.row_declarations",
                "framework.live_sync",
                "framework.view_storage_refresh",
                "framework.access_refresh",
            ] {
                assert!(framework_rows.iter().any(|row| row.phase == phase));
            }
            for phase in [
                "schema.cached_clone",
                "resource.apply_spec",
                "resource.group_spec",
                "resource.declaration_spec",
            ] {
                assert!(
                    resource_rows.iter().any(|row| row.phase == phase),
                    "{phase}"
                );
            }
            if sample >= 2 {
                eprintln!(
                    "notes-cpu-scaling {}",
                    serde_json::json!({
                        "architecture": std::env::consts::ARCH,
                        "profile": if cfg!(debug_assertions) { "test-debug" } else { "optimized" },
                        "pages": pages, "corpus_repeated_chars_per_other_page": "notebook text ".len() * 160,
                        "sample": sample - 2, "headless_event_pair_elapsed_ns": elapsed,
                        "profile_drain_elapsed_ns": drain_elapsed, "setup_ns": setup_ns,
                        "fixture_initialization": "peer_action_commits",
                        "setup_action_commits": pages * 2 - 1,
                        "phases": rows, "profile_drain_phases": drain_rows, "frames": frames,
                        "framework_phases": framework_rows, "resource_phases": resource_rows,
                        "includes_layout": true, "includes_cpu_scene_draw": true,
                        "includes_gpu_or_display": false,
                    })
                );
            }
        }
    }
}

#[test]
fn mounted_wheel_observes_long_note_main_thread_phases() {
    use automerge::transaction::Transactable;
    use std::fmt::Write as _;
    let (mut app, target) = fixture(1);
    let mut body = String::new();
    for index in 0..180 {
        writeln!(
            body,
            "Paragraph {index}: {}\n",
            "Measured scrolling text with ordinary words. ".repeat(6)
        )
        .unwrap();
    }
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    app.invoke(
        ui::tilo_resource::Invoke::apply(&crate::actions::write_body_action(target, &blocks))
            .unwrap(),
    )
    .unwrap();
    app.harness().relayout_at((1051, 952)).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted long editor");
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 40.0,
        ));
    assert_action_inventory(app.harness().root_widget());
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let event = ui::synthetic_input::mouse_wheel(0.0, -1.0);
    app.harness().begin_profiling();
    for sample in 0u32..150 {
        let mut presence_ns = 0;
        let mut baseline_presence_ns = 0;
        let mut answers = Vec::new();
        for dense in [sample.is_multiple_of(2), !sample.is_multiple_of(2)] {
            let (answer, elapsed) = action_presence(app.harness().root_widget(), dense);
            answers.push(answer);
            if dense {
                baseline_presence_ns = elapsed;
            } else {
                presence_ns = elapsed;
            }
        }
        assert_eq!(
            answers[0], answers[1],
            "presence agrees with complete action inventory"
        );
        let has_action_controls = answers[0];
        let (elapsed, notes_rows) = observe(|| {
            assert!(
                app.harness()
                    .dispatch_to_root_with(&event, &winit::event::Modifiers::default()),
                "real notebook wheel handled"
            );
            app.harness().relayout();
        });
        let framework_rows = &notes_rows;
        let frames = app.harness().take_profile_samples();
        assert!(!frames.is_empty(), "real CPU frame observed");
        eprintln!(
            "notes-scroll-phases {}",
            serde_json::json!({"sample":sample,"baseline_presence_ns":baseline_presence_ns,"presence_ns":presence_ns,"has_action_controls":has_action_controls,"root_type":app.harness().root_widget().concrete_type_name(),"paragraphs":180,"markdown_bytes":body.len(),"viewport":[1051,952],"elapsed_ns":elapsed,"notes_phases":notes_rows,"framework_phases":framework_rows,"frames":frames,"includes_gpu_or_display":false,"architecture":std::env::consts::ARCH})
        );
    }
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before,
        "scrolling does not mutate canonical note"
    );
    assert_action_inventory(app.harness().root_widget());
    let scrolled = app
        .surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().scroll_y_for_test())
        .unwrap();
    assert!(scrolled > 0.0, "observed note content moves");
}

fn action_presence(root: &dyn ui::Widget, dense: bool) -> (bool, u128) {
    let began = ui::time::now();
    let present = if dense {
        ui::widget::controls::control_action_bindings(root)
            .iter()
            .any(|binding| matches!(binding.kind, ui::widget::ControlBindingKind::Action(_)))
    } else {
        ui::widget::controls::has_action_control_bindings(root)
    };
    (present, began.elapsed().as_nanos())
}

fn assert_action_inventory(root: &dyn ui::Widget) {
    assert!(root.concrete_type_name().contains("BoundAppWidget"));
    let expected = ui::widget::controls::control_action_bindings(root)
        .into_iter()
        .filter(|binding| matches!(binding.kind, ui::widget::ControlBindingKind::Action(_)))
        .collect::<Vec<_>>();
    assert!(
        !expected.is_empty(),
        "real Notes action controls are mounted"
    );
    assert_eq!(
        ui::widget::controls::action_control_bindings(root),
        expected
    );
}

#[test]
fn unchanged_publication_reuses_tree_and_remote_and_locale_changes_refresh_it() {
    use automerge::transaction::Transactable;
    fn contains(resources: &[tilo_actions::Resource], id: &str) -> bool {
        resources
            .iter()
            .any(|resource| resource.id.as_str() == id || contains(&resource.children, id))
    }

    let (mut app, _) = fixture(16);
    // Settle and publish once before observing ordinary repeated catalogue reads.
    let original = app.harness().resources();
    assert!(contains(&original, "notes.notebook"));
    let before = app.read_document(Transactable::base_heads);
    let ((), rows) = ui::tilo_resource::profiling::observe(|| {
        for _ in 0..10 {
            let resources = app.harness().resources();
            assert!(contains(&resources, "notes.notebook"));
            assert_eq!(resources[0].children.len(), original[0].children.len());
        }
    });
    assert!(
        !rows
            .iter()
            .any(|row| row.phase == "framework.resource_tree"),
        "unchanged catalogue reads must reuse the published resource tree"
    );
    assert_eq!(app.read_document(Transactable::base_heads), before);

    let locale = ui::i18n::I18nManager::pseudo_locale();
    ui::i18n::I18nManager::set_pseudo_locale(ui::i18n::PseudoLocale::Expanded);
    let ((), rows) = ui::tilo_resource::profiling::observe(|| {
        assert!(contains(&app.harness().resources(), "notes.notebook"));
    });
    ui::i18n::I18nManager::set_pseudo_locale(locale);
    assert!(
        rows.iter()
            .any(|row| row.phase == "framework.resource_tree"),
        "translation changes must invalidate cached resource labels"
    );
    assert_eq!(app.read_document(Transactable::base_heads), before);

    let page = crate::PageId::new_v4();
    let mut peer = app.peer_document();
    apply_fixture_action(
        &mut peer,
        &crate::actions::create_page_action(page, None, "Remote creation"),
    );
    app.merge_peer(&mut peer).unwrap();
    let resources = app.harness().resources();
    assert!(
        contains(&resources, &crate::actions::page_resource_id(page)),
        "the cache must expose collaborator changes at the new revision"
    );
}

#[test]
fn mounted_text_note_view_only_scroll_contract_respects_title_editing() {
    use automerge::transaction::Transactable;
    let (mut app, target) = fixture(1);
    let body = "Ordinary text-note scrolling words.\n\n".repeat(32);
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    app.invoke(
        ui::tilo_resource::Invoke::apply(&crate::actions::write_body_action(target, &blocks))
            .unwrap(),
    )
    .unwrap();
    app.harness().relayout_at((1051, 240)).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted canonical editor");
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 20.0,
        ));
    let event = ui::synthetic_input::mouse_wheel(0.0, -1.0);
    assert!(
        app.harness().root_widget().view_only_scroll(&event),
        "mounted canonical text-note wheel opts in through its bound owner"
    );
    app.surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().begin_title_edit())
        .unwrap();
    assert!(
        !app.harness().root_widget().view_only_scroll(&event),
        "title editing keeps the full input path"
    );
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::keyboard_input(
            winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: winit::keyboard::PhysicalKey::Code(
                        winit::keyboard::KeyCode::Escape,
                    ),
                    state: winit::event::ElementState::Pressed,
                },
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                None,
                winit::keyboard::KeyLocation::Standard,
                false,
                None,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            ),
        ));
    assert!(
        app.harness().root_widget().view_only_scroll(&event),
        "cancelled title resumes text-note scrolling"
    );

    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    assert!(
        app.harness().dispatch_to_root(&event),
        "real text-note wheel is handled"
    );
    let scrolled = app
        .surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().scroll_y_for_test())
        .unwrap();
    assert!(scrolled > 0.0, "real note body scrolls");
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before
    );
}

#[test]
fn mounted_text_note_wheel_avoids_editor_projection_and_still_edits() {
    use automerge::transaction::Transactable;
    use std::fmt::Write as _;
    let (mut app, target) = fixture(1);
    let mut body = String::new();
    let paragraph = "Measured scrolling text with ordinary words. ".repeat(6);
    for index in 0..180 {
        write!(&mut body, "Paragraph {index}: {paragraph}\n\n").unwrap();
    }
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    app.invoke(
        ui::tilo_resource::Invoke::apply(&crate::actions::write_body_action(target, &blocks))
            .unwrap(),
    )
    .unwrap();
    app.harness().relayout_at((1051, 952)).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted canonical editor");
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 20.0,
        ));
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let event = ui::synthetic_input::mouse_wheel(0.0, -1.0);
    let (elapsed, rows) = observe(|| {
        for _ in 0..100 {
            assert!(
                app.harness().dispatch_to_root(&event),
                "real note wheel handled"
            );
        }
    });
    let editor_calls = rows
        .iter()
        .filter(|row| row.phase == "rich_text_input")
        .map(|row| row.calls)
        .sum::<usize>();
    eprintln!(
        "notes-wheel-editor-boundary {}",
        serde_json::json!({
            "paragraphs":180,"markdown_bytes":body.len(),"viewport":[1051,952],"wheel_events":100,
            "elapsed_ns":elapsed,"editor_input_calls":editor_calls,"phases":rows,
            "canonical_notes_framework":true,"native_window_runner":false,"gpu_or_scanout":false,
            "architecture":std::env::consts::ARCH,"profile":if cfg!(debug_assertions) {"test-debug"} else {"optimized"},
        })
    );
    assert_eq!(
        editor_calls, 0,
        "handled scrolling must not enter the editor's document-projection path"
    );
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before,
        "view scrolling leaves the canonical note untouched"
    );
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().scroll_y_for_test())
            .unwrap()
            > 0.0
    );
    app.harness().relayout();
    app.harness().click_at(rect.x0 + 40.0, rect.y0 + 20.0);
    let (_, editing) = observe(|| app.harness().type_text("verified "));
    assert!(
        editing
            .iter()
            .any(|row| row.phase == "rich_text_input" && row.calls > 0),
        "a real text edit must still reach the instrumented editor"
    );
    assert_ne!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before,
        "the real edit reaches the canonical record"
    );
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.editor_text_for_test())
            .unwrap()
            .contains("verified ")
    );
}

#[test]
fn mounted_text_note_draw_frame_phase_probe() {
    use automerge::transaction::Transactable;
    use std::fmt::Write as _;
    let (mut app, target) = fixture(1);
    let mut body = String::new();
    let paragraph = "Measured scrolling text with ordinary words. ".repeat(6);
    for index in 0..180 {
        write!(&mut body, "Paragraph {index}: {paragraph}\n\n").unwrap();
    }
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    app.invoke(
        ui::tilo_resource::Invoke::apply(&crate::actions::write_body_action(target, &blocks))
            .unwrap(),
    )
    .unwrap();
    app.harness().relayout_at((1051, 952)).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted canonical note");
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 80.0,
        ));
    let document = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    assert_eq!(document.1, 0);
    app.harness().begin_profiling();
    for sample in 0..12 {
        let started = ui::time::now();
        assert!(
            app.harness()
                .dispatch_to_root(&ui::synthetic_input::mouse_wheel(0.0, -1.0))
        );
        app.harness().relayout_at((1051, 952)).unwrap();
        let frames = app.harness().take_profile_samples();
        assert!(
            !frames.is_empty(),
            "observe actual canonical Notes layout and paint"
        );
        assert!(frames.iter().any(|frame| frame.draw_us > 0));
        assert!(
            !app.harness()
                .scene()
                .encoding()
                .resources
                .glyph_runs
                .is_empty(),
            "actual visible text is encoded"
        );
        assert_eq!(
            app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
            document,
            "drawing and view input preserve the canonical document"
        );
        eprintln!(
            "notes-draw-frame-phase {}",
            serde_json::json!({"sample":sample,"paragraphs":180,"viewport":[1051,952],"canonical_notes":true,"body_wraps":true,"gpu_or_scanout":false,"profile":"test-debug","elapsed_ns":started.elapsed().as_nanos(),"frames":frames})
        );
    }
}

/// Choosing a note writes nothing, so it must not read the notebook again:
/// that read costs every note, half a second in a notebook of 400.
#[test]
fn choosing_a_note_reads_no_notebook_and_shows_it() {
    let (mut app, _) = fixture(16);
    let other = app
        .read_document(crate::read::list_pages)
        .into_iter()
        .find(|page| page.title == "CPU fixture 5")
        .expect("fixture note")
        .id;
    let ((), rows) = ui::tilo_resource::profiling::observe(|| {
        app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(other))
            .unwrap();
        app.harness().relayout();
    });
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.editor_text_for_test())
            .unwrap()
            .contains("Authored fixture 5"),
        "the chosen note is shown"
    );
    let reads: Vec<_> = rows
        .iter()
        .filter(|row| row.phase == "read_notebook")
        .collect();
    assert!(
        reads.is_empty(),
        "choosing a note read the notebook: {reads:?}"
    );
}

/// Top-level notes in the owned notebook; each holds [`OWNED_CHILDREN`].
const OWNED_FOLDERS: usize = 24;
const OWNED_CHILDREN: usize = 16;
/// Sections of the long note the profile types into and scrolls.
const OWNED_LONG_SECTIONS: usize = 45;

/// A disposable notebook shaped like a working one: nested notes, tags,
/// `[[links]]` between notes, headings, lists, code and one long note.
/// Every note is authored on a collaborator replica through the same
/// actions a person's edits use, then merged once.
fn owned_notebook() -> (
    ui::framework::testing::TestApp<crate::NotesShell>,
    crate::PageId,
    Vec<crate::PageId>,
) {
    use std::fmt::Write as _;
    ui::i18n::I18nManager::init("en-US");
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    let mut peer = app.peer_document();
    let tags: Vec<crate::model::TagId> = ["work", "reading", "ideas", "travel", "home"]
        .into_iter()
        .map(|name| {
            let tag = crate::model::TagId::new_v4();
            apply_fixture_action(&mut peer, &crate::actions::new_tag_action(tag, name));
            tag
        })
        .collect();
    let mut pages = Vec::new();
    let mut titles = std::collections::HashMap::new();
    for folder in 0..OWNED_FOLDERS {
        let parent = crate::PageId::new_v4();
        let title = format!("Project {folder}");
        apply_fixture_action(
            &mut peer,
            &crate::actions::create_page_action(parent, None, &title),
        );
        titles.insert(title, parent);
        pages.push(parent);
        for child in 0..OWNED_CHILDREN {
            let page = crate::PageId::new_v4();
            let title = format!("Meeting {folder}-{child}");
            apply_fixture_action(
                &mut peer,
                &crate::actions::create_page_action(page, Some(parent), &title),
            );
            titles.insert(title, page);
            pages.push(page);
        }
    }
    let resolve = |title: &str| titles.get(title).copied();
    for (index, page) in pages.iter().enumerate() {
        let mut body = String::from("## Summary\n\n");
        for paragraph in 0..8 {
            writeln!(
                body,
                "Discussed item {paragraph} with the team; see [[Project {}]] for context and \
                 follow up on the open questions before the next review.\n",
                (index + paragraph) % OWNED_FOLDERS
            )
            .unwrap();
        }
        body.push_str("- first action item\n- second action item\n- [ ] check in\n\n");
        writeln!(
            body,
            "```rust\nfn main() {{\n    println!(\"{index}\");\n}}\n```\n"
        )
        .unwrap();
        let blocks = crate::MainPanel::seed_blocks(&body, resolve);
        apply_fixture_action(
            &mut peer,
            &crate::actions::write_body_action(*page, &blocks),
        );
        apply_fixture_action(
            &mut peer,
            &crate::actions::set_tag_assignment_action(*page, tags[index % tags.len()], true),
        );
    }
    let long = crate::PageId::new_v4();
    apply_fixture_action(
        &mut peer,
        &crate::actions::create_page_action(long, None, "Research journal"),
    );
    let mut body = String::new();
    for section in 0..OWNED_LONG_SECTIONS {
        writeln!(
            body,
            "## Section {section}\n\nParagraph {section} records ordinary findings with **bold** \
             words, `inline code` and a link to [[Meeting {}-{}]] among enough prose to wrap. \
             {}\n\n- observation {section}\n- follow-up {section}\n",
            section % OWNED_FOLDERS,
            section % OWNED_CHILDREN,
            "Measured text with ordinary words. ".repeat(5)
        )
        .unwrap();
    }
    let blocks = crate::MainPanel::seed_blocks(&body, resolve);
    apply_fixture_action(&mut peer, &crate::actions::write_body_action(long, &blocks));
    pages.push(long);
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(
        app.read_document(crate::read::list_pages).len(),
        pages.len()
    );
    app.harness().relayout_at((1280, 900)).unwrap();
    app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(long))
        .unwrap();
    app.harness().relayout_at((1280, 900)).unwrap();
    (app, long, pages)
}

/// Samples per interaction: the full count where the timings mean something,
/// a few in an unoptimised build, where every interaction and its assertions
/// still run but the rows are not a profile.
const fn owned_samples(optimised: usize) -> usize {
    if cfg!(debug_assertions) {
        if optimised < 3 { optimised } else { 3 }
    } else {
        optimised
    }
}

/// Print one interaction sample with the frames and scopes it ran.
fn emit_owned(
    interaction: &str,
    sample: usize,
    elapsed: u128,
    rows: &[Sample],
    frames: &[ui::profiling::FrameTiming],
) {
    eprintln!(
        "notes-owned-profile {}",
        serde_json::json!({
            "interaction": interaction, "sample": sample, "elapsed_ns": elapsed,
            "pages": OWNED_FOLDERS * (OWNED_CHILDREN + 1) + 1,
            "long_note_sections": OWNED_LONG_SECTIONS,
            "phases": rows, "frames": frames, "viewport": [1280, 900],
            "includes_layout": true, "includes_cpu_scene_draw": true,
            "includes_gpu_or_display": false, "architecture": std::env::consts::ARCH,
            "profile": if cfg!(debug_assertions) { "test-debug" } else { "optimized" },
        })
    );
}

type OwnedApp = ui::framework::testing::TestApp<crate::NotesShell>;

/// The text of the note the editor shows.
fn shown_text(app: &mut OwnedApp) -> String {
    app.surface(|notes: &mut crate::NotesApp| notes.editor_text_for_test())
        .unwrap()
}

/// The owned-fixture profile: typing, scrolling, pointer motion, an idle
/// frame, opening notes, writes from the note tree and a collaborator, search
/// and reading the notebook, each on the real mounted app with its canonical
/// effect asserted. No timing budgets; the printed rows are the profile.
#[test]
fn owned_notebook_profile_covers_every_interaction() {
    let (mut app, long, pages) = owned_notebook();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted editor");
    app.harness().click_at(rect.x0 + 30.0, rect.y0 + 12.0);
    app.harness().begin_profiling();
    profile_typing(&mut app);
    profile_viewing(&mut app, (rect.x0 + 60.0, rect.y0 + 200.0));
    profile_opening(&mut app, &pages);
    app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(long))
        .unwrap();
    app.harness().relayout();
    profile_writes(&mut app, &pages);
    profile_search_and_read(&mut app, pages.len());
    profile_restore(&mut app, &pages);
    profile_launch(app, pages.len());
}

fn profile_typing(app: &mut OwnedApp) {
    let before = shown_text(app);
    for sample in 0..owned_samples(40) {
        let (elapsed, rows) = observe(|| app.harness().type_text("a"));
        let frames = app.harness().take_profile_samples();
        assert!(!frames.is_empty(), "typing draws a frame");
        emit_owned("type", sample, elapsed, &rows, &frames);
    }
    assert_eq!(
        shown_text(app).len(),
        before.len() + owned_samples(40),
        "every keystroke reached the note"
    );
}

/// Scrolling the long note, moving the pointer over it and an idle frame.
fn profile_viewing(app: &mut OwnedApp, pointer: (f64, f64)) {
    use automerge::transaction::Transactable;
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(pointer.0, pointer.1));
    let heads = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let wheel = ui::synthetic_input::mouse_wheel(0.0, -1.0);
    for sample in 0..owned_samples(60) {
        let (elapsed, rows) = observe(|| {
            assert!(app.harness().dispatch_to_root(&wheel), "wheel handled");
            app.harness().relayout();
        });
        let frames = app.harness().take_profile_samples();
        emit_owned("scroll", sample, elapsed, &rows, &frames);
    }
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().scroll_y_for_test())
            .unwrap()
            > 0.0,
        "the long note scrolls"
    );
    for sample in 0..owned_samples(40) {
        let x = pointer.0 + f64::from(u8::try_from(sample % 7).unwrap());
        let moved = ui::synthetic_input::cursor_moved(x, pointer.1);
        let (elapsed, rows) = observe(|| {
            app.harness().dispatch_to_root(&moved);
            app.harness().relayout();
        });
        let frames = app.harness().take_profile_samples();
        emit_owned("pointer_move", sample, elapsed, &rows, &frames);
    }
    for sample in 0..owned_samples(40) {
        let (elapsed, rows) = observe(|| app.harness().relayout());
        let frames = app.harness().take_profile_samples();
        emit_owned("idle_frame", sample, elapsed, &rows, &frames);
    }
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        heads,
        "viewing leaves the notebook untouched"
    );
}

/// Choosing notes across the notebook, as the note tree does.
fn profile_opening(app: &mut OwnedApp, pages: &[crate::PageId]) {
    let chosen = pages.iter().step_by(17).take(owned_samples(24));
    for (sample, page) in chosen.enumerate() {
        let (elapsed, rows) = observe(|| {
            app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(*page))
                .unwrap();
            app.harness().relayout();
        });
        let frames = app.harness().take_profile_samples();
        assert!(
            shown_text(app).contains("Discussed item 0"),
            "the opened note is shown"
        );
        emit_owned("open_note", sample, elapsed, &rows, &frames);
    }
}

/// A rename from the note tree, on the tree's own event path, and a
/// collaborator's rename arriving.
fn profile_writes(app: &mut OwnedApp, pages: &[crate::PageId]) {
    let titled = |app: &mut OwnedApp, title: &str| {
        app.read_document(crate::read::list_pages)
            .iter()
            .any(|page| page.title == title)
    };
    for sample in 0..owned_samples(12) {
        let title = format!("Renamed locally {sample}");
        let rename = crate::sidebar::OutEvent::Rename {
            id: pages[5],
            new_title: title.clone(),
        };
        let (elapsed, rows) = observe(|| {
            app.surface(|notes: &mut crate::NotesApp| notes.apply_event_for_test(rename))
                .unwrap();
            app.harness().relayout();
        });
        let frames = app.harness().take_profile_samples();
        assert!(titled(app, &title), "the rename reached the notebook");
        emit_owned("rename_note", sample, elapsed, &rows, &frames);
    }
    for sample in 0..owned_samples(30) {
        let mut peer = app.peer_document();
        let title = format!("Renamed remotely {sample}");
        apply_fixture_action(
            &mut peer,
            &crate::actions::rename_page_action(pages[3], &title),
        );
        let (elapsed, rows) = observe(|| app.merge_peer(&mut peer).unwrap());
        let frames = app.harness().take_profile_samples();
        assert!(titled(app, &title), "the collaborator's edit landed");
        emit_owned("remote_edit", sample, elapsed, &rows, &frames);
    }
}

/// The notebook's search, and reading the notebook whole as a launch or a
/// restored view does.
fn profile_search_and_read(app: &mut OwnedApp, notes: usize) {
    // The framework indexes the notebook's entries off the main thread; the
    // first query starts that build and later ones read the finished index.
    let hits = |app: &mut OwnedApp| -> usize {
        ui::search::capability::search_providers(app.harness().root_widget())
            .iter()
            .map(|provider| provider.search_limited("follow up", 50).len())
            .sum()
    };
    let started = ui::time::now();
    let (_, rows) = ui::tilo_resource::profiling::observe(|| hits(app));
    while hits(app) == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the notebook's search index arrives"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    emit_owned("search_index", 0, started.elapsed().as_nanos(), &rows, &[]);
    for sample in 0..owned_samples(20) {
        let ((found, elapsed), rows) = ui::tilo_resource::profiling::observe(|| {
            let began = ui::time::now();
            (hits(app), began.elapsed().as_nanos())
        });
        assert!(found > 0, "the notebook's search finds its blocks");
        emit_owned("search", sample, elapsed, &rows, &[]);
    }
    for sample in 0..owned_samples(5) {
        let ((projected, elapsed), rows) = ui::tilo_resource::profiling::observe(|| {
            let began = ui::time::now();
            let notebook = ui::tilo_resource::profiling::measure("notebook_read.resource", || {
                app.read_document(<crate::Notebook as ui::tilo_resource::Resource>::read)
            })
            .unwrap();
            let projected =
                ui::tilo_resource::profiling::measure("notebook_read.workspace", || {
                    crate::Workspace::of(&notebook, &crate::Workspace::default())
                });
            (projected, began.elapsed().as_nanos())
        });
        assert_eq!(projected.pages().len(), notes, "every note is read");
        assert_eq!(
            projected.pages(),
            app.read_document(crate::read::project).unwrap().pages(),
            "the profiled read is the one Notes projects"
        );
        assert_eq!(bodies_read(&rows), notes, "every body is read once");
        emit_owned("notebook_read", sample, elapsed, &rows, &[]);
    }
}

/// How many rich texts `rows` read: one body spine per read.
fn bodies_read(rows: &[Sample]) -> usize {
    rows.iter()
        .find(|row| row.phase == "rich_text.read.spine")
        .map_or(0, |row| row.calls)
}

/// Restoring a saved window view, as a launch that reopens the window does,
/// to the frame that shows it. Hosted, the framework has read the notebook
/// already: the restore takes that state and reads no note a second time,
/// and a collaborator's change that arrived after the framework read is
/// shown all the same.
fn profile_restore(app: &mut OwnedApp, pages: &[crate::PageId]) {
    use ui::application_view::ApplicationViewState as _;
    let saved_page = pages[40];
    app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(saved_page))
        .unwrap();
    app.harness().relayout();
    let saved = app
        .surface(|notes: &mut crate::NotesApp| notes.capture_application_view())
        .unwrap()
        .unwrap()
        .expect("Notes saves its view");
    let leave = |app: &mut OwnedApp| {
        app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(pages[0]))
            .unwrap();
        app.harness().relayout();
        drop(app.harness().take_profile_samples());
    };
    for sample in 0..owned_samples(10) {
        leave(app);
        let doc = app.read_document(Clone::clone);
        let (elapsed, rows) = observe(|| {
            app.surface(|notes: &mut crate::NotesApp| {
                notes.restore_application_view(&saved, &doc).unwrap();
                notes.finish_application_view_restore().unwrap();
            })
            .unwrap();
            app.harness().relayout();
        });
        let frames = app.harness().take_profile_samples();
        assert_eq!(
            app.surface(|notes: &mut crate::NotesApp| notes.selected_for_test())
                .unwrap(),
            Some(saved_page),
            "the saved note is selected again"
        );
        assert!(
            shown_text(app).contains("Discussed item 0"),
            "the saved note is shown"
        );
        let reads: Vec<_> = rows
            .iter()
            .filter(|row| row.phase == "read_notebook")
            .collect();
        assert!(
            reads.is_empty(),
            "a hosted restore read the notebook again: {reads:?}"
        );
        // Binding the editor to the saved note reads that note's body.
        assert!(
            bodies_read(&rows) <= 2,
            "a hosted restore read more than the note it shows: {rows:?}"
        );
        emit_owned("restore_view", sample, elapsed, &rows, &frames);
    }
    // A collaborator renames a note after the framework read the notebook:
    // the document the restore is given holds the rename, the live notebook
    // has not followed it yet.
    leave(app);
    let title = "Renamed before restore";
    let mut peer = app.peer_document();
    apply_fixture_action(
        &mut peer,
        &crate::actions::rename_page_action(pages[9], title),
    );
    let doc = peer.clone();
    let (shown, rows) = app
        .surface(|notes: &mut crate::NotesApp| {
            ui::tilo_resource::profiling::observe(|| {
                notes.restore_application_view(&saved, &doc).unwrap();
                notes.finish_application_view_restore().unwrap();
                notes
                    .workspace_pages_for_test()
                    .iter()
                    .any(|page| page.title == title)
            })
        })
        .unwrap();
    assert!(
        shown,
        "the restore shows a change that arrived after the framework read"
    );
    assert!(
        bodies_read(&rows) <= 2,
        "catching up on a rename read note bodies: {rows:?}"
    );
    app.merge_peer(&mut peer).unwrap();
    drop(app.harness().take_profile_samples());
}

/// Closing the window and launching Notes again on the same profile, to its
/// first frame: the framework reads the notebook once, and Notes projects
/// that read rather than reading every note a second time.
fn profile_launch(mut app: OwnedApp, notes: usize) {
    for sample in 0..owned_samples(5) {
        let ((reopened, elapsed), rows) = ui::tilo_resource::profiling::observe(|| {
            let began = ui::time::now();
            let mut reopened = app.reopen().unwrap();
            reopened.harness().relayout_at((1280, 900)).unwrap();
            (reopened, began.elapsed().as_nanos())
        });
        app = reopened;
        assert_eq!(
            app.surface(|notes: &mut crate::NotesApp| notes.workspace_pages_for_test())
                .unwrap()
                .len(),
            notes,
            "the relaunched window shows every note"
        );
        assert!(
            rows.iter().all(|row| row.phase != "read_notebook"),
            "a hosted launch read the notebook in Notes: {rows:?}"
        );
        assert!(
            (notes..2 * notes).contains(&bodies_read(&rows)),
            "a launch reads the notebook once: {rows:?}"
        );
        // The notes' dates come from the whole history; a launch reads it
        // away from the UI thread and its first frame does not wait for it.
        assert!(
            rows.iter().all(|row| row.phase != "schema_changes.read"),
            "a launch read the notebook's history on the UI thread: {rows:?}"
        );
        emit_owned("launch", sample, elapsed, &rows, &[]);
        await_note_dates(&mut app);
    }
    // The read a launch hands to a background thread, run here: what it
    // costs that thread.
    for sample in 0..owned_samples(5) {
        let ((read, elapsed), rows) = ui::tilo_resource::profiling::observe(|| {
            let began = ui::time::now();
            let read = app.read_document(read_note_dates_in_place);
            (read, began.elapsed().as_nanos())
        });
        assert!(
            rows.iter().any(|row| row.phase == "schema_changes.read"),
            "the history was read: {rows:?}"
        );
        drop(read);
        emit_owned("note_dates_read", sample, elapsed, &rows, &[]);
    }
}

/// Every note's dates, read from the whole history on this thread.
fn read_note_dates_in_place(doc: &ui::tilo_resource::Doc) -> crate::metadata::NoteDateIndex {
    let mut index = crate::metadata::NoteDateIndex::default();
    index.sync_reading_off_thread_from(doc, u64::MAX);
    index
}

/// Wait for the open note's dates to land from the read a launch started
/// off the UI thread, and check they are the ones the history gives.
fn await_note_dates(app: &mut OwnedApp) {
    let page = app
        .surface(|notes: &mut crate::NotesApp| notes.selected_for_test())
        .unwrap()
        .expect("the relaunched window opens a note");
    let expected = app.read_document(read_note_dates_in_place).get(page);
    assert!(expected.edited.is_some(), "the open note has an edit time");
    let started = ui::time::now();
    while app
        .surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().dates_for_test())
        .unwrap()
        != expected
    {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the open note's dates land after a launch"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
        app.harness().relayout();
    }
}
