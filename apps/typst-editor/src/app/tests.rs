//! Tests of the editor window: its sync, layout, controls and input.

use super::*;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton};

fn fresh() -> (TypstApp, AutoCommit) {
    let mut app = TypstApp::with_demo_documents();
    let mut doc = AutoCommit::new();
    // Drive one sync to seed + compile the first demo document.
    app.sync(&mut doc, &ROOT);
    (app, doc)
}

#[test]
fn file_effects_save_export_and_portal_round_trip() {
    // Case `direct_save_is_accepted_then_succeeds`.
    {
        let (mut app, mut doc) = fresh();
        let dir = unique_dir("direct-lifecycle");
        std::fs::create_dir_all(&dir).unwrap();
        let destination = dir.join("saved.typ");
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::TypstOperation::Save {
            path: Some(destination.display().to_string()),
        })
        .unwrap();
        let started = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::OPERATION_TYPE,
                &payload,
            ),
            &mut doc,
            &ROOT,
        )
        .unwrap()
        .unwrap();
        assert_eq!(started, ui::widget::ResourceOperationStart::Accepted);
        app.sync(&mut doc, &ROOT);
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert!(destination.exists());
        std::fs::remove_dir_all(dir).ok();
    }
    // Case `export_pdf_writes_a_real_pdf_file`:
    // Exporting writes a real `.pdf` next to the source — the file on disk
    // begins with the `%PDF` magic. Driven against a scratch dir so it never
    // touches the user's documents folder.
    {
        let (app, doc) = fresh();
        let dir = unique_dir("pdf");
        let path = app
            .export_pdf_to(&doc, &dir)
            .expect("the clean demo document exports");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("pdf"));
        let bytes = std::fs::read(&path).expect("the pdf was written to disk");
        assert!(bytes.starts_with(b"%PDF"), "the written file is a real PDF");
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `portal_open_then_save_round_trips_a_real_file`:
    // The portal Open → edit → Save round trip through real files on disk,
    // driving the exact per-frame `drain_io` the host chooser feeds, answered
    // by a recording host (no D-Bus dialog). Opening loads an arbitrary `.typ`
    // into the editor's CRDT; saving writes the current source to a chosen
    // path.
    {
        let (mut app, mut doc) = fresh();
        let dir = unique_dir("portal");
        std::fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join("imported.typ");
        std::fs::write(&src_path, "= Imported\nHello from disk\n").unwrap();

        let (services, recorder) = ui::host::HostServices::recording();

        // Answer the Open request and sync: the file loads into the CRDT source.
        recorder.respond_to_pick(PortalOutcome::Opened(src_path));
        ui::host::scoped(&services, || app.open(&doc));
        app.sync(&mut doc, &ROOT);
        assert!(
            app.io.is_none(),
            "a delivered outcome clears the in-flight dialog"
        );
        let loaded = app.read_source(&doc);
        assert!(
            loaded.contains("Hello from disk"),
            "the opened file's text became the editor source: {loaded:?}"
        );

        // Answer the Save request with a new path and sync: the source is written.
        let out_path = dir.join("saved.typ");
        recorder.respond_to_pick(PortalOutcome::Opened(out_path.clone()));
        ui::host::scoped(&services, || app.save(&doc));
        app.sync(&mut doc, &ROOT);
        let titles: Vec<_> = recorder
            .pick_requests()
            .iter()
            .map(|r| r.title().to_string())
            .collect();
        assert_eq!(titles, ["Open a Typst file", "Save Typst file"]);
        let written = std::fs::read_to_string(&out_path).expect("the save wrote the file");
        assert_eq!(
            written,
            app.read_source(&doc),
            "the saved file round-trips the current source"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `portal_cancel_is_silent`:
    // A cancelled chooser clears the in-flight dialog and changes nothing.
    {
        let (mut app, mut doc) = fresh();
        let before = app.read_source(&doc);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Cancelled);
        ui::host::scoped(&services, || app.open(&doc));
        app.sync(&mut doc, &ROOT);
        assert!(app.io.is_none());
        assert_eq!(app.read_source(&doc), before, "cancel loads nothing");
    }
}

fn device_id() -> DeviceId {
    // winit exposes no public DeviceId constructor; tests only need a value.
    unsafe { std::mem::zeroed() }
}

fn cursor(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: device_id(),
        position: PhysicalPosition::new(p.x, p.y),
    }
}

fn press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: device_id(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

/// Pump one event through the production `Widget::handle_event`, returning
/// whether it was consumed.
fn send(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    let mut handled = false;
    let mut focus = None;
    app.handle_event(
        ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: doc,
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}

/// Run one real draw pass, so the chrome measures and places its controls
/// exactly as it does for the live pointer path.
fn render_once(app: &mut TypstApp) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let presence = PresenceCtx::empty();
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

#[test]
fn source_sync_edits_and_switches_documents() {
    // Case `first_sync_seeds_source_and_compiles`.
    {
        let (app, doc) = fresh();
        assert!(app.source_id.is_some(), "source object seeded");
        let src = app.read_source(&doc);
        assert!(!src.is_empty(), "seeded with demo text");
        assert!(app.initialized);
        // Welcome demo compiles cleanly into at least one page.
        assert!(app.preview.page_count() >= 1, "rendered a page");
        assert_eq!(app.err_count, 0, "welcome demo has no errors");
    }
    // Case `source_edit_requests_a_semantically_stamped_commit`.
    {
        let (mut app, mut doc) = fresh();
        let before = app.read_source(&doc);

        assert!(send(&mut app, &mut doc, &char_press("x")));
        assert_ne!(app.read_source(&doc), before, "the source really changed");
        assert!(ui::undo::needs_commit(&app));

        let context = ui::undo::take_commit_context(&mut app).expect("native edit provenance");
        assert_eq!(context.action_label, "Edit source");
        assert_eq!(
            context.app.as_ref().map(|app| app.id.as_str()),
            Some("so.tilo.typst-editor")
        );
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.kind.as_str()),
            Some("typst.document")
        );

        ui::undo::on_commit(&mut app);
        assert!(
            !ui::undo::needs_commit(&app),
            "commit acknowledgement clears the latch"
        );
    }
    // Case `switching_documents_replaces_source`.
    {
        let (mut app, mut doc) = fresh();
        let first = app.read_source(&doc);
        app.next_doc(&mut doc);
        let second = app.read_source(&doc);
        assert_ne!(first, second, "next_doc swapped the CRDT source");
        assert_eq!(app.current, 1);
    }
    // Case `caret_line_col_tracks_newlines`.
    {
        let mut app = TypstApp::with_demo_documents();
        app.source_cache = "abc\nde\nf".to_string();
        app.editor.set_text("abc\nde\nf");
        app.editor.set_caret(5); // byte 5 → line 2, col 2 (just after 'd')
        assert_eq!(app.caret_line_col(), (2, 2));
    }
    // Case `accessible_source_replaces_the_collaborative_text`.
    {
        use ui::widget::Widget as _;

        let (mut app, mut doc) = fresh();
        assert!(app.perform_semantic_action(
            "typst.source",
            &ui::a11y::Action::SetValue("= Accessible edit".into()),
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(app.read_source(&doc), "= Accessible edit");
        assert!(ui::undo::needs_commit(&app));
    }
}

#[test]
fn idle_and_pending_work_schedule_non_animation_deadlines() {
    // Case `focused_idle_app_requests_no_animation_frame`:
    // A focused editor that isn't compiling or flashing must report no need
    // for animation frames, so the event loop stays idle (no per-vsync
    // redraw just to blink a caret).
    {
        let (app, _doc) = fresh();
        assert!(app.editor.focused, "editor focused by default");
        assert!(
            !app.compiling(),
            "the initial compile ran synchronously; nothing in flight"
        );
        assert!(
            !app.status.is_visible(ui::time::now()),
            "no transient announcement pending"
        );
        assert!(
            !app.needs_animation_frame(),
            "a focused-but-idle editor must not drive continuous redraws"
        );
    }
    // Case `pending_native_dialog_has_a_stable_non_animation_deadline`.
    {
        let (mut app, mut doc) = fresh();
        assert!(app.next_wake_deadline().is_none());
        let (services, _recorder) = ui::host::HostServices::recording();
        ui::host::scoped(&services, || app.open(&doc));
        let deadline = app
            .next_wake_deadline()
            .expect("file dialogs must complete without animation or further input");
        assert_eq!(
            app.next_wake_deadline(),
            Some(deadline),
            "queries must not keep postponing polling"
        );
        app.sync(&mut doc, &ROOT);
        assert!(app.io.is_none());
        assert!(app.next_wake_deadline().is_none());
    }
    // Case `pending_compile_has_a_non_animation_deadline`.
    {
        let (mut app, _) = fresh();
        app.request_compile("= Native compile wakeup");
        assert!(app.compiling());
        assert!(
            app.next_wake_deadline().is_some(),
            "compile completion must be observed with reduced motion"
        );
    }
    // Case `status_bar_reserves_its_band_and_the_bubble_fades`:
    // What a person reads while working is the status bar, which reserves
    // its band: both panes end where it begins and nothing is painted under
    // it. A change worth noticing once is the transient bubble, which shows
    // on the announcement and hides again once its linger elapses.
    {
        let (mut app, _doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert_eq!(app.status_rect.y1, 720.0, "the bar is at the window bottom");
        assert_eq!(
            app.editor_rect.y1, app.status_rect.y0,
            "the editor ends where the status bar begins"
        );
        assert_eq!(
            app.preview_rect.y1, app.status_rect.y0,
            "the preview ends where the status bar begins"
        );
        assert!(app.status_rect.height() > 0.0);

        let now = ui::time::now();
        assert!(!app.status.is_visible(now), "nothing is announced at rest");
        app.set_flash("Saved welcome.typ");
        assert!(
            app.status.is_visible(now),
            "an announcement shows the bubble"
        );
        assert!(app.needs_animation_frame(), "the bubble fades by itself");

        // With an already-expired linger, the bubble is gone.
        app.status = ui::StatusBubble::new().with_linger(std::time::Duration::from_millis(0));
        let past = now
            .checked_sub(std::time::Duration::from_millis(10))
            .unwrap();
        app.status.show("Saved welcome.typ", past);
        assert!(
            !app.status.is_visible(ui::time::now()),
            "the bubble hides after its linger"
        );
    }
}

/// The editor mounted as a real window: its controls are in the layout
/// tree and take pointer and key input through the host's routing.
fn mounted(width: u32, height: u32) -> ui::headless::FrameHarness {
    let mut harness = ui::headless::FrameHarness::for_root(
        ui::State::new(),
        TypstApp::with_demo_documents(),
        (width, height),
        Theme::dark(),
    );
    // One frame draws the chrome, which measures and places its controls; a
    // control it shows or hides joins or leaves the tree on the next pass.
    let _ = harness.render_rgba();
    harness.relayout();
    harness
}

/// The rect of every control the current layout shows.
fn control_rects(app: &TypstApp) -> Vec<(String, Rect)> {
    app.chrome
        .children()
        .iter()
        .filter(|child| child.get_rect().area() > 0.0)
        .map(|child| (child.name().to_string(), child.get_rect()))
        .collect()
}

#[test]
fn controls_stay_in_the_bar_and_never_overlap_at_any_width() {
    for width in [320.0, 360.0, 390.0, 480.0, 560.0, 720.0, 800.0, 1280.0] {
        for (errors, warnings) in [(0, 0), (1, 1)] {
            let (mut app, _) = fresh();
            app.err_count = errors;
            app.warn_count = warnings;
            app.layout_in(Rect::new(0.0, 0.0, width, 760.0));
            render_once(&mut app);
            let rects = control_rects(&app);
            assert!(
                rects.len() >= 4,
                "the picker, switch or arrows, toolbar and History show at {width}px"
            );
            for (index, (left_name, left)) in rects.iter().enumerate() {
                assert!(
                    left.x0 >= app.bar_rect.x0
                        && left.x1 <= app.bar_rect.x1
                        && left.y0 >= app.bar_rect.y0
                        && left.y1 <= app.bar_rect.y1,
                    "{left_name} {left:?} leaves the bar {:?} at {width}px",
                    app.bar_rect
                );
                for (right_name, right) in rects.iter().skip(index + 1) {
                    assert!(
                        left.intersect(*right).area() <= 0.0,
                        "{left_name} {left:?} overlaps {right_name} {right:?} at {width}px"
                    );
                }
            }
        }
    }
}

#[test]
fn resize_reflows_between_split_and_compact() {
    // The responsive contract: layout is re-derived from the *current* width
    // every pass, so resizing across the threshold reflows both ways — it is
    // never frozen to the first frame's mode.
    let (mut app, _doc) = fresh();

    // Wide → split: both panes have area and the divider exists.
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
    assert!(!app.compact);
    assert!(app.editor_rect.area() > 0.0);
    assert!(app.preview_rect.area() > 0.0);
    assert!(app.splitter.get_rect().area() > 0.0);

    // Narrow → compact: a single pane, no divider.
    app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
    assert!(app.compact);
    assert_eq!(app.splitter.get_rect().area(), 0.0);
    assert!(app.editor_rect.area() > 0.0, "editor pane shown");
    assert_eq!(app.preview_rect.area(), 0.0, "preview hidden in compact");

    // Resize back to wide → must reflow to split again (not frozen).
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
    assert!(!app.compact, "reflowed back to split after widening");
    assert!(app.preview_rect.area() > 0.0);
}

#[test]
fn text_scale_grows_the_bar_and_compacts_before_controls_collide() {
    // Enlarged system text changes the geometry as well as glyph metrics:
    // the taller controls stay in their own bar and a formerly cramped
    // split switches to the compact single-pane layout.
    let (mut app, _doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
    let normal = app.frame.bar_h;
    let mut tr = TextRenderer::new();
    tr.set_text_scale(2.0);
    let mut theme = Theme::dark();
    theme.text_scale = 2.0;
    app.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::empty(),
    );

    assert!(
        app.bar_rect.height() > normal,
        "the bar grows with the text"
    );
    assert!(app.compact, "1280px is compact at 200% text scale");
    assert_eq!(app.editor_rect.y0, app.bar_rect.y1);
    for (_, rect) in control_rects(&app) {
        assert!(
            rect.y1 <= app.bar_rect.y1,
            "every control fits the grown bar"
        );
    }
}

#[test]
fn the_toolbar_offers_every_command_and_each_one_reaches_the_app() {
    let (mut app, mut doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1400.0, 760.0));
    render_once(&mut app);
    let targets = app.chrome.toolbar_targets();
    let names: Vec<&str> = targets.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Compile",
            "Problems",
            "Zoom out",
            "Zoom in",
            "Fit width",
            "Save",
            "Open",
            "Export PDF",
        ]
    );
    for (_, target) in &targets {
        assert!(app.chrome.activate_toolbar(target, &mut doc));
    }
    assert_eq!(
        app.chrome.take(),
        [
            Command::Compile,
            Command::ToggleProblems,
            Command::ZoomOut,
            Command::ZoomIn,
            Command::Fit,
            Command::Save,
            Command::Open,
            Command::ExportPdf,
        ]
    );
}

#[test]
fn the_problems_toggle_counts_errors_and_warnings() {
    let (mut app, _doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1400.0, 760.0));
    render_once(&mut app);
    assert!(
        app.chrome.toolbar_names().contains(&"Problems".to_string()),
        "no count while there is nothing to count"
    );
    assert!(
        app.take_structure_dirty(),
        "the first placement shows the document arrows"
    );
    assert!(!app.take_structure_dirty());

    // One error and one warning are two problems, not one.
    app.err_count = 1;
    app.warn_count = 1;
    render_once(&mut app);
    assert!(
        app.chrome
            .toolbar_names()
            .contains(&"Problems (2)".to_string()),
        "{:?}",
        app.chrome.toolbar_names()
    );
    assert!(
        app.take_structure_dirty(),
        "a rebuilt toolbar asks for a layout pass"
    );
    assert!(!app.take_structure_dirty(), "the request is read once");

    // Only warnings still count.
    app.err_count = 0;
    app.warn_count = 3;
    render_once(&mut app);
    assert!(
        app.chrome
            .toolbar_names()
            .contains(&"Problems (3)".to_string())
    );
}

#[test]
fn the_document_commands_switch_documents() {
    let (mut app, mut doc) = fresh();
    let count = app.documents.len();
    assert!(count > 2);
    app.run_command(Command::SelectDoc(2), &mut doc);
    assert_eq!(app.current, 2);
    app.run_command(Command::NextDoc, &mut doc);
    assert_eq!(app.current, 3);
    app.run_command(Command::NextDoc, &mut doc);
    assert_eq!(app.current, 0, "next wraps to the first document");
    app.run_command(Command::PrevDoc, &mut doc);
    assert_eq!(app.current, count - 1, "previous wraps to the last");
    app.run_command(Command::SelectDoc(count + 4), &mut doc);
    assert_eq!(app.current, count - 1, "an unknown document is ignored");
}

#[test]
fn the_edit_preview_switch_and_the_document_picker_take_real_pointer_input() {
    // Case `edit_preview_switch_via_pointer`:
    // Tapping the compact Edit / Preview control through the host's
    // routing swaps the visible pane.
    {
        let mut harness = mounted(560, 720);
        assert!(harness.root::<TypstApp>().compact);
        assert!(matches!(
            harness.root::<TypstApp>().compact_view,
            CompactPane::Editor
        ));
        let switch = harness.targets()["typst.view"];
        assert!(switch.area() > 0.0, "the switch is placed: {switch:?}");
        harness.click_at(switch.x1 - switch.width() / 4.0, switch.center().y);
        assert!(matches!(
            harness.root::<TypstApp>().compact_view,
            CompactPane::Preview
        ));
        let _ = harness.render_rgba();
        let app = harness.root::<TypstApp>();
        assert!(app.preview_rect.area() > 0.0);
        assert_eq!(app.editor_rect.area(), 0.0);

        harness.click_at(switch.x0 + switch.width() / 4.0, switch.center().y);
        assert!(matches!(
            harness.root::<TypstApp>().compact_view,
            CompactPane::Editor
        ));
    }
    // Case `document_picker_via_keyboard_after_a_click`:
    // Opening the picker and choosing the next document with the keys
    // loads that document.
    {
        let mut harness = mounted(1280, 720);
        let picker = harness.targets()["typst.document"];
        harness.click_at(picker.center().x, picker.center().y);
        harness.key("Down");
        harness.key("Enter");
        let _ = harness.render_rgba();
        assert_eq!(harness.root::<TypstApp>().current, 1);
    }
}

#[test]
fn the_previous_and_next_buttons_switch_documents_in_a_wide_window() {
    let mut harness = mounted(1280, 720);
    let next = harness.targets()["typst.next-document"];
    harness.click_at(next.center().x, next.center().y);
    assert_eq!(harness.root::<TypstApp>().current, 1);
    let previous = harness.targets()["typst.previous-document"];
    harness.click_at(previous.center().x, previous.center().y);
    assert_eq!(harness.root::<TypstApp>().current, 0);
}

#[test]
fn a_narrow_toolbar_keeps_every_command_reachable_in_its_more_menu() {
    let mut harness = mounted(390, 720);
    let wanted = [
        "Compile",
        "Save",
        "Open",
        "Export PDF",
        "Zoom out",
        "Zoom in",
        "Fit width",
        "Problems",
    ];
    let shown = harness.root::<TypstApp>().chrome.toolbar_names();
    assert!(
        shown.iter().any(|name| name == "More commands"),
        "what does not fit folds away: {shown:?}"
    );
    let chip = harness
        .root::<TypstApp>()
        .chrome
        .toolbar_chip()
        .expect("the More chip is placed");
    harness.click_at(chip.center().x, chip.center().y);
    let _ = harness.render_rgba();
    let names = harness.root::<TypstApp>().chrome.toolbar_names();
    for name in wanted {
        assert!(
            names.iter().any(|shown| shown == name),
            "{name} is reachable in a narrow window: {names:?}"
        );
    }
}

#[test]
fn dragging_the_library_splitter_resizes_the_panes() {
    let mut harness = mounted(1280, 720);
    let (before, grab) = {
        let app = harness.root::<TypstApp>();
        (app.editor_rect.width(), app.splitter.get_rect().center())
    };
    harness.drag_at((grab.x, grab.y), (grab.x + 120.0, grab.y));
    let _ = harness.render_rgba();
    let app = harness.root::<TypstApp>();
    assert!(
        app.editor_rect.width() > before + 60.0,
        "the editor grew from {before} to {}",
        app.editor_rect.width()
    );
    assert!(app.split_ratio <= 0.82 && app.split_ratio >= 0.18);
}

#[test]
fn the_zoom_pair_and_the_problems_toggle_survive_overflow_together() {
    for width in (728..=1000).step_by(8) {
        let harness = mounted(width, 720);
        let names = harness.root::<TypstApp>().chrome.toolbar_names();
        assert_eq!(
            names.iter().any(|name| name == "Zoom out"),
            names.iter().any(|name| name == "Zoom in"),
            "zoom out and zoom in show or fold together at {width}px: {names:?}"
        );
        assert!(
            names.iter().any(|name| name.starts_with("Problems")),
            "the Problems toggle shows at {width}px: {names:?}"
        );
    }
}

#[test]
fn the_problems_panel_closes_with_its_own_button() {
    let mut harness = mounted(1280, 720);
    harness.root_mut::<TypstApp>().apply_demo_scenario("errors");
    harness.relayout();
    let _ = harness.render_rgba();
    assert!(harness.root::<TypstApp>().show_diagnostics);
    let close = harness.targets()["typst.close-problems"];
    assert!(close.area() > 0.0, "the close button is placed: {close:?}");
    harness.click_at(close.center().x, close.center().y);
    assert!(!harness.root::<TypstApp>().show_diagnostics);
}

#[test]
fn the_status_bar_reads_the_caret_and_the_pages() {
    let (mut app, _doc) = fresh();
    assert_eq!(app.status_left_label(), "Ln 1, Col 1");
    assert_eq!(app.status_right_label(), "1 page, 100%");
    app.preview.clear();
    assert_eq!(app.status_right_label(), "No pages");
}

#[test]
fn live_library_reads_disk_and_starts_empty() {
    // Case `live_library_reads_typ_files_from_disk`.
    {
        let dir = std::env::temp_dir().join(format!(
            "typst_editor_lib_{}_{}",
            std::process::id(),
            ui::time::wall()
                .duration_since(ui::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("beta.typ"), "= Beta").unwrap();
        std::fs::write(dir.join("alpha.typ"), "= Alpha").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let app = TypstApp::from_library(Some(&dir));
        let names: Vec<_> = app.documents.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names, vec!["alpha.typ", "beta.typ"], "sorted, .txt ignored");
        assert!(app.documents.iter().all(|d| d.from_disk));

        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `live_library_starts_with_empty_source`.
    {
        let dir = std::env::temp_dir().join(format!(
            "typst_editor_empty_{}_{}",
            std::process::id(),
            ui::time::wall()
                .duration_since(ui::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let app = TypstApp::from_library(Some(&dir));
        assert_eq!(app.documents.len(), 1);
        assert_eq!(app.documents[0].name, "untitled.typ");
        assert!(!app.documents[0].from_disk, "starter is not a disk file");
        assert!(
            app.documents[0].source.is_empty(),
            "live documents must not contain sample text"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

// ---- helpers for the preview-interaction + diagnostics tests ----------

use crate::compile::DiagKind;
use winit::event::TouchPhase;
use winit::keyboard::{KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

/// A character keypress through the real event path. winit's per-platform
/// `KeyEventExtra` is `pub(crate)`, so the struct is zeroed and only its
/// public fields set — the same trick the toolkit's own widget tests use.
fn char_press(c: &str) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(c.into());
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(c.into());
    WindowEvent::KeyboardInput {
        device_id: device_id(),
        event: ev,
        is_synthetic: false,
    }
}

/// A vertical wheel notch (`+` = up/away). A tilt component can be supplied
/// via `x` for the horizontal-pan path.
fn wheel(x: f32, y: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: device_id(),
        delta: MouseScrollDelta::LineDelta(x, y),
        phase: TouchPhase::Moved,
    }
}

/// Like [`send`] but with explicit modifiers held.
fn send_mods(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent, mods: Modifiers) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    let mut handled = false;
    let mut focus = None;
    app.handle_event(
        ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: doc,
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}

fn unique_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "typst_editor_{tag}_{}_{}",
        std::process::id(),
        ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn preview_zoom_and_diagnostics_respond_to_input() {
    // Case `ctrl_plus_minus_keys_zoom_preview`:
    // Ctrl+= zooms the rendered preview in and Ctrl+- zooms it back out, driven
    // through the production keyboard-shortcut path.
    {
        let (mut app, mut doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        let base = app.preview.zoom();
        assert!(
            send_mods(&mut app, &mut doc, &char_press("="), ctrl),
            "Ctrl+= consumed"
        );
        let zoomed_in = app.preview.zoom();
        assert!(
            zoomed_in > base,
            "Ctrl+= zoomed the preview in ({base} -> {zoomed_in})"
        );

        assert!(
            send_mods(&mut app, &mut doc, &char_press("-"), ctrl),
            "Ctrl+- consumed"
        );
        assert!(
            app.preview.zoom() < zoomed_in,
            "Ctrl+- zoomed the preview back out ({zoomed_in} -> {})",
            app.preview.zoom()
        );
    }
    // Case `ctrl_wheel_over_preview_zooms`:
    // Ctrl+mouse-wheel over the preview pane zooms it (cursor-anchored), routed
    // through the real `MouseWheel` handler.
    {
        let (mut app, mut doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        let over = app.preview_rect.center();
        send_mods(&mut app, &mut doc, &cursor(over), ctrl);
        let base = app.preview.zoom();
        assert!(
            send_mods(&mut app, &mut doc, &wheel(0.0, 1.0), ctrl),
            "Ctrl+wheel consumed"
        );
        assert!(
            app.preview.zoom() > base,
            "Ctrl+wheel over the preview zoomed in ({base} -> {})",
            app.preview.zoom()
        );
    }
    // Case `diagnostic_click_jumps_editor_for_both_severities`:
    // The Problems panel shows both an error and a warning from the real
    // compiler, and clicking a row jumps the editor caret to that diagnostic's
    // line — proven for both severities through the production pointer path.
    {
        let mut app = TypstApp::with_demo_documents();
        app.apply_demo_scenario("errors");
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT); // first sync compiles synchronously
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        // One draw measures the rows, which sizes the panel to them.
        render_once(&mut app);

        let diags = app.diagnostics.diags().to_vec();
        assert!(
            diags.iter().any(|d| d.kind == DiagKind::Error),
            "an error is listed"
        );
        assert!(
            diags.iter().any(|d| d.kind == DiagKind::Warning),
            "a warning is listed"
        );
        assert!(
            app.show_diagnostics && app.diag_rect.area() > 0.0,
            "Problems panel is open"
        );
        assert!(
            diags.len() >= 2 && diags[0].line != diags[1].line,
            "rows target distinct lines"
        );

        for (i, d) in diags.iter().enumerate() {
            let Some(line) = d.line else { continue };
            let row = app.diagnostics.row_rect(i);
            let p = Point::new(row.x0 + 40.0, row.center().y);
            send(&mut app, &mut doc, &cursor(p));
            assert!(
                send(&mut app, &mut doc, &press()),
                "diagnostic row click consumed"
            );
            assert_eq!(
                app.caret_line_col().0,
                line,
                "clicking the {:?} row jumped the editor to line {line}",
                d.kind
            );
        }
    }
    // Case `failed_recompile_keeps_last_good_preview`:
    // A failed recompile keeps the last successful render on screen (flagged
    // stale) instead of blanking the preview — so a transient typo doesn't
    // throw away the reader's place. Drives the real compile → `apply_output`
    // path the background worker feeds.
    {
        let (mut app, _doc) = fresh();
        assert!(
            app.preview.page_count() >= 1,
            "the welcome demo rendered a page"
        );
        assert!(!app.preview.is_stale(), "a clean render is not stale");

        // Compile a broken source (an undefined variable is a fatal error that
        // stops layout), then feed the result exactly as the worker would.
        let broken = compile::compile_source(&app.parts, "Body #undefined_var end.\n", PPP);
        assert!(
            broken.pages.is_empty(),
            "the broken source produced no pages"
        );
        assert!(broken.error_count() >= 1, "and carries a blocking error");
        app.apply_output(broken);

        assert!(
            app.preview.page_count() >= 1,
            "the last good render is retained after a failed compile"
        );
        assert!(
            app.preview.is_stale(),
            "the retained render is flagged stale"
        );
        assert!(
            app.err_count >= 1 && app.show_diagnostics,
            "the error is surfaced"
        );

        // A subsequent clean compile clears the stale flag and refreshes pages.
        let good = compile::compile_source(&app.parts, "= Fixed\n\nBody.\n", PPP);
        assert!(!good.pages.is_empty());
        app.apply_output(good);
        assert!(
            !app.preview.is_stale(),
            "a clean recompile clears the stale flag"
        );

        // Opening another document starts from its own render: it never
        // shows the last good pages of the one it replaced, and the
        // problems of the old document are not its own.
        let (mut app, mut doc) = fresh();
        let broken = compile::compile_source(&app.parts, "Body #undefined_var end.\n", PPP);
        app.apply_output(broken);
        assert!(app.err_count >= 1);
        app.next_doc(&mut doc);
        assert_eq!(app.preview.page_count(), 0, "the old pages are gone");
        assert!(!app.preview.is_stale(), "nothing stale carries over");
        assert_eq!((app.err_count, app.warn_count), (0, 0));
        assert!(app.diagnostics.diags().is_empty());
    }
}

#[test]
fn accessibility_exposes_editor_state_and_toolbar_controls() {
    // Case `accessibility_exposes_source_and_preview_state`.
    {
        use ui::widget::Widget as _;

        let (app, _doc) = fresh();
        let tree = app.accessibility().expect("Typst semantics");
        assert_eq!(tree.role, ui::a11y::Role::Document);
        assert_eq!(tree.children[0].role, ui::a11y::Role::MultilineTextInput);
        assert_eq!(tree.children[1].role, ui::a11y::Role::Canvas);
        assert!(
            tree.children[1]
                .value
                .as_deref()
                .is_some_and(|value| value.contains("Page"))
        );
        assert!(tree.children.iter().all(|node| node.semantic_key.is_some()));
    }
    // Case `controls_are_retained_widgets_that_name_themselves`:
    // The controls above the panes are library widgets in the tree the
    // host walks, each naming itself; the painted app lists only its two
    // painted panes by hand.
    {
        let (mut app, mut doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 1400.0, 760.0));
        render_once(&mut app);
        let tree = app.accessibility().expect("Typst semantics");
        assert_eq!(
            tree.children.len(),
            2,
            "only the source and the preview are listed by hand"
        );
        let named: Vec<String> = app
            .chrome
            .children()
            .iter()
            .filter_map(|child| child.accessibility())
            .filter_map(|node| node.name)
            .collect();
        for expected in ["Previous document", "Next document", "Document"] {
            assert!(
                named.iter().any(|name| name == expected),
                "{expected} is named: {named:?}"
            );
        }
        assert!(
            app.chrome.toolbar_names().iter().any(|name| name == "Save"),
            "the toolbar names its commands"
        );

        // Case `listed_problems_are_reachable_and_jump_to_their_line`:
        // Each problem the panel lists is an accessibility node whose
        // activation moves the caret to the line it names.
        {
            let mut app = TypstApp::with_demo_documents();
            app.apply_demo_scenario("errors");
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            render_once(&mut app);
            let tree = app.accessibility().expect("Typst semantics");
            let rows: Vec<_> = tree
                .children
                .iter()
                .filter(|node| node.role == ui::a11y::Role::ListItem)
                .collect();
            assert_eq!(rows.len(), app.diagnostics.diags().len());
            assert!(
                rows.iter().all(|row| row.bounds.is_some()),
                "each row says where it is"
            );
            let target = rows
                .iter()
                .find(|row| {
                    row.name
                        .as_deref()
                        .is_some_and(|name| name.contains("undefined_value"))
                })
                .and_then(|row| row.action_target.clone())
                .expect("the error row");
            assert!(app.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut doc,
                &ROOT
            ));
            assert_eq!(app.caret_line_col().0, 5, "the error is on line 5");
        }

        // Focusing the source from assistive technology brings the editor
        // into view in a narrow window; nothing else is a painted control.
        app.compact = true;
        app.compact_view = CompactPane::Preview;
        assert!(app.perform_semantic_action(
            "typst.source",
            &ui::a11y::Action::Focus,
            &mut doc,
            &ROOT
        ));
        assert!(matches!(app.compact_view, CompactPane::Editor));
        assert!(!app.perform_semantic_action(
            "typst.show-preview",
            &ui::a11y::Action::Activate,
            &mut doc,
            &ROOT
        ));
    }
}
