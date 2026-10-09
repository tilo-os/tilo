//! Test-only observations around the actual Notes projection functions.
//! No timing budgets: the canonical mutation assertions establish the subject.

use std::cell::RefCell;
use std::time::Instant;

thread_local! {
    static RECORDS: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

const MAX_PHASES: usize = 32;

#[derive(Default)]
struct Capture {
    rows: Vec<Sample>,
    overflow: bool,
}

/// Repeated scopes aggregate by name, with exact counts and bounded memory.
/// These nested durations must not be added to their enclosing scope.
#[derive(serde::Serialize)]
struct Sample {
    phase: &'static str,
    elapsed_ns: u128,
    calls: usize,
}

pub(crate) struct Phase(Option<(&'static str, Instant)>);

impl Phase {
    pub(crate) fn start(name: &'static str) -> Self {
        Self(RECORDS.with(|records| records.borrow().as_ref().map(|_| (name, ui::time::now()))))
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some((name, start)) = self.0 {
            RECORDS.with(|records| {
                if let Some(capture) = records.borrow_mut().as_mut() {
                    let elapsed_ns = start.elapsed().as_nanos();
                    if let Some(row) = capture.rows.iter_mut().find(|row| row.phase == name) {
                        match (
                            row.elapsed_ns.checked_add(elapsed_ns),
                            row.calls.checked_add(1),
                        ) {
                            (Some(elapsed), Some(calls)) => {
                                row.elapsed_ns = elapsed;
                                row.calls = calls;
                            }
                            _ => capture.overflow = true,
                        }
                    } else if capture.rows.len() < MAX_PHASES {
                        capture.rows.push(Sample {
                            phase: name,
                            elapsed_ns,
                            calls: 1,
                        });
                    } else {
                        capture.overflow = true;
                    }
                }
            });
        }
    }
}

fn observe(operation: impl FnOnce()) -> (u128, Vec<Sample>) {
    RECORDS.with(|records| assert!(records.replace(Some(Capture::default())).is_none()));
    let start = ui::time::now();
    operation();
    let elapsed = start.elapsed().as_nanos();
    let capture = RECORDS.with(|records| records.take().unwrap());
    assert!(
        !capture.overflow,
        "Notes phase observation exceeded its bound"
    );
    (elapsed, capture.rows)
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
            &crate::actions::append_page_action(page, None, &format!("CPU fixture {index}")),
        );
        if index != 0 {
            let body = format!("Authored fixture {index}. {}", "notebook text ".repeat(160));
            let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
            apply_fixture_action(&mut peer, &crate::actions::write_body_action(page, &blocks));
        }
    }
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(
        app.read_document(crate::doc::NotesDoc::list_pages).len(),
        pages
    );
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

fn apply_fixture_action(doc: &mut ui::tilo_resource::Doc, action: &crate::actions::NotesAction) {
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
            let (((elapsed, rows), framework_rows), resource_rows) =
                ui::tilo_resource::profiling::observe(|| {
                    ui::framework::testing::phases::observe(|| {
                        observe(|| app.harness().type_text("a"))
                    })
                });
            // This maintained observer pumps pending wakes. Measure its work
            // separately; captured passes belong to the combined interval.
            let mut frames = Vec::new();
            let (drain_elapsed, drain_rows) = observe(|| {
                frames = app.harness().take_profile_samples();
            });
            let text = app.read_document(|doc| {
                crate::doc::NotesDoc::page_block_snapshots(doc, target)
                    .into_iter()
                    .map(|(_, text)| text)
                    .collect::<String>()
            });
            assert_eq!(text, "a".repeat(sample + 1), "real editor mutation");
            for phase in [
                "search_blocks",
                "search_delta_adopted",
                "canonical_projection_adopted",
                "notebook_children",
                "page_tree_commands",
                "page_tree_action",
                "page_body_resource",
                "page_body_publish",
                "page_body_continuations",
                "duplicate_sources",
                "page_preview",
                "continuation_stamp",
                "publication_backlinks",
                "page_decoration",
                "projection_bodies",
                "projection_rich_states",
            ] {
                assert!(rows.iter().any(|row| row.phase == phase));
            }
            assert!(rows.iter().all(|row| row.phase != "search_body_decode"));
            assert!(rows.iter().any(|row| row.phase == "tree_metadata_plan"));
            if pages > 1 {
                for phase in [
                    "tree_metadata_delta",
                    "tree_metadata_encode",
                    "tree_metadata_spec",
                ] {
                    assert!(rows.iter().any(|row| row.phase == phase));
                }
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
                "child.publish_parts",
                "child.rewrap",
                "schema.cached_clone",
                "resource.apply_spec",
                "resource.group_spec",
                "resource.declaration_spec",
            ] {
                assert!(resource_rows.iter().any(|row| row.phase == phase));
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
        let ((elapsed, notes_rows), framework_rows) =
            ui::framework::testing::phases::observe(|| {
                observe(|| {
                    assert!(
                        app.harness()
                            .dispatch_to_root_with(&event, &winit::event::Modifiers::default()),
                        "real notebook wheel handled"
                    );
                    app.harness().relayout();
                })
            });
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
    let (_, rows) = observe(|| {
        for _ in 0..10 {
            let resources = app.harness().resources();
            assert!(contains(&resources, "notes.notebook"));
            assert_eq!(resources[0].children.len(), original[0].children.len());
        }
    });
    assert!(
        !rows.iter().any(|row| row.phase == "notebook_children"),
        "unchanged catalogue reads must reuse the canonical resource tree"
    );
    assert_eq!(app.read_document(Transactable::base_heads), before);

    let locale = ui::i18n::I18nManager::pseudo_locale();
    ui::i18n::I18nManager::set_pseudo_locale(ui::i18n::PseudoLocale::Expanded);
    let (_, rows) = observe(|| {
        assert!(contains(&app.harness().resources(), "notes.notebook"));
    });
    ui::i18n::I18nManager::set_pseudo_locale(locale);
    assert!(
        rows.iter().any(|row| row.phase == "notebook_children"),
        "translation changes must invalidate cached resource labels"
    );
    assert_eq!(app.read_document(Transactable::base_heads), before);

    let page = crate::PageId::new_v4();
    let mut peer = app.peer_document();
    apply_fixture_action(
        &mut peer,
        &crate::actions::append_page_action(page, None, "Remote creation"),
    );
    app.merge_peer(&mut peer).unwrap();
    let resources = app.harness().resources();
    assert!(
        contains(&resources, &crate::actions::page_resource_id(page)),
        "the cache must expose collaborator changes at the new revision"
    );
}
