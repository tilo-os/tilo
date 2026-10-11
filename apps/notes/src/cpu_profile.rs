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

fn fixture(
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
        .dispatch_to_root(&winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(rect.x0 + 40.0, rect.y0 + 40.0),
        });
    assert_action_inventory(app.harness().root_widget());
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let event = winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, -1.0),
        phase: winit::event::TouchPhase::Moved,
    };
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
        .dispatch_to_root(&winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(rect.x0 + 40.0, rect.y0 + 20.0),
        });
    let event = winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, -1.0),
        phase: winit::event::TouchPhase::Moved,
    };
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
        .dispatch_to_root(&winit::event::WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: winit::event::KeyEvent::synthetic(
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
            is_synthetic: false,
        });
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
        .dispatch_to_root(&winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(rect.x0 + 40.0, rect.y0 + 20.0),
        });
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let event = winit::event::WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, -1.0),
        phase: winit::event::TouchPhase::Moved,
    };
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
        .dispatch_to_root(&ui::winit::event::WindowEvent::CursorMoved {
            device_id: ui::winit::event::DeviceId::dummy(),
            position: ui::winit::dpi::PhysicalPosition::new(rect.x0 + 40.0, rect.y0 + 80.0),
        });
    let document = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    assert_eq!(document.1, 0);
    app.harness().begin_profiling();
    for sample in 0..12 {
        let started = ui::time::now();
        assert!(
            app.harness()
                .dispatch_to_root(&ui::winit::event::WindowEvent::MouseWheel {
                    device_id: ui::winit::event::DeviceId::dummy(),
                    delta: ui::winit::event::MouseScrollDelta::LineDelta(0.0, -1.0),
                    phase: ui::winit::event::TouchPhase::Moved,
                })
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
