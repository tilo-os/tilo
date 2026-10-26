//! Editor app tests: preview compiles, file open, save and export, input, accessibility and toolbar layout.

use super::*;
use winit::event::Modifiers;

use automerge::ROOT;
use ui::framework::ResourceApp as _;
use ui::tilo_resource::{Replica, Resource as _};

use crate::resource::{TypstEditorShell, Workspace};

/// A workspace of the bundled examples, presented to an editor showing
/// the first, synced once so it compiled.
fn live_fixture(viewport: (u32, u32)) -> ui::framework::testing::TestApp<TypstEditorShell> {
    let mut app = ui::framework::testing::TestApp::<TypstEditorShell>::new().unwrap();
    for invoke in TypstEditorShell::scenario(None) {
        app.invoke(invoke).unwrap();
    }
    app.harness().resize(viewport).unwrap();
    app.settle();
    app
}

fn fresh() -> (TypstApp, AutoCommit) {
    workspace(None)
}

/// The outline lists the open document's headings beside the source, and
/// going to one puts the source's caret on its line, from the toolbar's
/// toggle and the heading's published effect alike.
#[test]
fn the_outline_goes_to_a_heading_in_the_source() {
    use crate::actions::TypstEffect;
    let (mut app, _doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1400.0, 900.0));
    let headings = crate::outline::headings(&app.source_cache);
    let target = headings.last().expect("the welcome document has headings");
    let outline = app
        .live_rows()
        .into_iter()
        .find(|row| row.id() == "typst.outline")
        .expect("the outline is published")
        .into_resource();
    assert!(
        outline
            .children
            .iter()
            .any(|row| row.id.0 == format!("typst.heading:{}", target.line)),
        "every heading is a row an agent can go to"
    );
    let full = app.editor_rect.width() + app.preview_rect.width();
    app.perform_resource_operation(TypstEffect::Outline);
    assert!(app.outline.visible());
    assert!(
        app.editor_rect.width() + app.preview_rect.width() < full,
        "the docked outline takes room from the panes, not over them"
    );
    app.perform_resource_operation(TypstEffect::GoToHeading { line: target.line });
    assert_eq!(app.caret_line_col().0, target.line);
    assert!(app.editor.focused);
    app.perform_resource_operation(TypstEffect::Outline);
    assert!(!app.outline.visible());
}

/// [`fresh`] with the examples a capture of `scenario` shows.
fn workspace(scenario: Option<&str>) -> (TypstApp, AutoCommit) {
    let mut replica = Replica::<Workspace>::new();
    for invoke in TypstEditorShell::scenario(scenario) {
        replica.apply(invoke).unwrap();
    }
    let mut doc = AutoCommit::load(&replica.save()).unwrap();
    doc.commit();
    let mut app = TypstApp::new();
    present(&mut app, &doc);
    app.sync(&doc, &ROOT);
    (app, doc)
}

fn present(app: &mut TypstApp, doc: &AutoCommit) {
    app.present_state(&Rc::new(Workspace::read(doc).unwrap()));
}

/// The source of the file `app` shows, as `doc` holds it.
fn shown_source(app: &TypstApp, doc: &AutoCommit) -> String {
    let state = Workspace::read(doc).unwrap();
    app.current
        .as_ref()
        .and_then(|id| state.file(id))
        .map(|file| file.source.clone())
        .unwrap_or_default()
}

#[test]
fn hosted_capture_finishes_the_real_preview_compile_before_its_first_render() {
    let mut app =
        ui::framework::testing::TestApp::<TypstEditorShell>::capture_at(None, (1280, 800)).unwrap();
    assert_compiled_welcome(&mut app);
}

#[test]
fn live_preview_finishes_the_real_compile_after_published_source_actions() {
    let mut app = ui::framework::testing::TestApp::<TypstEditorShell>::new().unwrap();
    let file = app.state().files.ids().next().unwrap().clone();
    let (_, sample) = crate::resource::sample_files(None).remove(0);
    let source = Workspace::paths().files.item(&file).source;
    app.invoke(ui::tilo_resource::Invoke::edit(&source.splice(0, 0, sample.source)).unwrap())
        .unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app.harness()
        .wait_for_visuals(tilo_test_env::hang_bound())
        .unwrap();
    assert_compiled_welcome(&mut app);
}

fn assert_compiled_welcome(app: &mut ui::framework::testing::TestApp<TypstEditorShell>) {
    app.with_surface::<TypstApp, _>(|view| {
        assert!(
            !view.compiling(),
            "capture returned while the real source compilation was pending"
        );
        assert!(view.editor.text().contains("Smooth motion, by design"));
        assert_eq!(view.err_count, 0);
        view.preview.select_all();
        let compiled = view
            .preview
            .selected_text()
            .expect("the rendered page has real selectable glyphs");
        view.preview.clear_selection();
        assert!(
            compiled.contains("Smooth motion, by design"),
            "actual compiled text: {compiled}"
        );
    })
    .unwrap();
}

#[test]
fn mounted_toolbar_commands_have_native_action_admission() {
    let mut app = live_fixture((1280, 720));
    for viewport in [(1280, 720), (480, 720)] {
        app.harness().resize(viewport).unwrap();
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{viewport:?}: {audit:?}");
    }
}

#[test]
fn file_effects_save_and_export_real_files() {
    // Case `export_pdf_writes_a_real_pdf_file`:
    // Exporting writes a real `.pdf` — the file on disk begins with the
    // `%PDF` magic. Driven against a scratch dir so it never touches the
    // user's documents folder.
    {
        let (app, _doc) = fresh();
        let dir = unique_dir("pdf");
        let path = dir.join("welcome.pdf");
        app.export_pdf_to_path(&path)
            .expect("the clean demo document exports");
        let bytes = std::fs::read(&path).expect("the pdf was written to disk");
        assert!(bytes.starts_with(b"%PDF"), "the written file is a real PDF");
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `portal_cancel_is_silent`:
    // A cancelled chooser clears the in-flight dialog and changes nothing.
    {
        let (mut app, doc) = fresh();
        let before = shown_source(&app, &doc);
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        ui::host::scoped(&services, || app.open());
        work.run_pending();
        app.sync(&doc, &ROOT);
        assert!(app.io.is_none());
        assert_eq!(shown_source(&app, &doc), before, "cancel loads nothing");
    }
}

/// A host whose files have no paths (a browser page): Open delivers
/// `opened` by contents and Save keeps what is written, by name.
/// What a [`PathlessHost`] was handed to save: each file's name and bytes.
type Kept = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

struct PathlessHost {
    opened: Option<(String, Vec<u8>)>,
    saved: Kept,
}

impl ui::host::FilePicker for PathlessHost {
    fn pick(&self, _: PickRequest) -> ui::portal::PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::PortalOutcome::Failed("no paths".into()));
        rx
    }
    fn pick_multiple(&self, _: PickRequest) -> ui::portal::MultiChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::MultiOutcome::Failed("no paths".into()));
        rx
    }
    fn open_file(&self, _: PickRequest) -> ui::portal::OpenFileChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::OpenFileOutcome::Failed("no paths".into()));
        rx
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::LaunchOutcome::Failed("no paths".into()));
        rx
    }
    fn pick_contents(&self, _: PickRequest) -> ui::portal::ContentsChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(match &self.opened {
            Some((name, bytes)) => {
                ui::portal::ContentsOutcome::Picked(vec![ui::portal::PickedContents {
                    name: name.clone(),
                    media_type: None,
                    bytes: bytes.clone(),
                    location: None,
                }])
            }
            None => ui::portal::ContentsOutcome::Cancelled,
        });
        rx
    }
    fn save_destination(&self, request: PickRequest) -> ui::portal::DestinationChannel {
        let saved = std::sync::Arc::clone(&self.saved);
        let name = request.suggested_name().unwrap_or_default().to_owned();
        let kept = name.clone();
        ui::portal::DestinationChannel::answered(ui::portal::DestinationOutcome::Chosen(
            ui::portal::SaveDestination::kept(name, move |bytes| {
                saved.lock().unwrap().push((kept.clone(), bytes));
                let (tx, rx) = std::sync::mpsc::channel();
                let _ = tx.send(ui::portal::SaveOutcome::Saved);
                rx
            }),
        ))
    }
}

/// Where files have no paths (a browser page), Open reads the chosen
/// file's contents, and Save and Export PDF hand the host the source and
/// the PDF, instead of failing for want of a path.
#[test]
fn open_save_and_export_work_where_files_have_no_paths() {
    let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let host = std::sync::Arc::new(PathlessHost {
        opened: Some(("notes.typ".into(), b"= Notes\nHello".to_vec())),
        saved: std::sync::Arc::clone(&saved),
    });
    let services = ui::host::HostServices::null().with_files(host);
    let (mut app, mut doc) = fresh();
    let dispatcher = ui::framework::testing::dispatcher::<Workspace>(
        <TypstEditorShell as ui::framework::ResourceApp>::ID,
    );
    app.actions = Some(dispatcher.clone());
    ui::host::scoped(&services, || app.open());
    app.sync(&doc, &ROOT);
    ui::framework::testing::resolve(&dispatcher, &mut doc).expect("the open applies");
    present(&mut app, &doc);
    app.sync(&doc, &ROOT);
    assert!(app.io.is_none(), "the open landed");
    let file = app.current_file().cloned().expect("a file is shown");
    assert_eq!(file.source, "= Notes\nHello");
    assert_eq!(file.name.0, "notes.typ");
    assert_eq!(file.path, None, "a browser file lives nowhere until saved");

    ui::host::scoped(&services, || app.save()).unwrap();
    app.sync(&doc, &ROOT);
    app.sync(&doc, &ROOT);
    assert!(app.io.is_none(), "the save landed");
    assert!(matches!(app.saves.state(), Some(ui::SaveState::Saved)));

    ui::host::scoped(&services, || app.export_pdf());
    app.sync(&doc, &ROOT);
    app.sync(&doc, &ROOT);
    assert!(app.io.is_none(), "the export landed");

    // The next Save writes where the first one went, without asking; only
    // Save As asks again.
    ui::host::scoped(&services, || app.save()).unwrap();
    assert!(
        matches!(app.io, Some(PendingIo::Writing { .. })),
        "the second Save writes at once, with no chooser"
    );
    app.sync(&doc, &ROOT);
    assert!(app.io.is_none(), "the second save landed");
    ui::host::scoped(&services, || app.save_as());
    assert!(
        matches!(app.io, Some(PendingIo::SaveTyp(_))),
        "Save As asks where"
    );

    let saved = saved.lock().unwrap().clone();
    assert_eq!(
        saved[0],
        ("notes.typ".to_owned(), b"= Notes\nHello".to_vec())
    );
    assert_eq!(saved[1].0, "notes.pdf");
    assert!(saved[1].1.starts_with(b"%PDF"), "the export is a PDF");
    assert_eq!(
        saved[2],
        ("notes.typ".to_owned(), b"= Notes\nHello".to_vec()),
        "the second Save wrote to the kept place"
    );
}

#[test]
fn failed_save_keeps_its_retry_destination_and_success_clears_the_failure() {
    let (mut app, _) = fresh();
    let dir = unique_dir("save-recovery");
    let path = dir.join("report.typ");
    // The missing directory makes the actual save fail, without relying
    // on permissions (which differ when the tests run as root).
    assert!(app.save_typ_to(&path).is_err());
    assert_eq!(app.save_path.as_deref(), Some(path.as_path()));
    assert!(matches!(app.saves.state(), Some(ui::SaveState::Failed(_))));

    std::fs::create_dir_all(&dir).unwrap();
    let retry = app.save_path.clone().unwrap();
    app.save_typ_to(&retry).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        app.current_file().unwrap().source
    );
    assert!(
        app.saves.state() == Some(ui::SaveState::Saved),
        "a successful retry dismisses the failure card"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A file written by an action while the window shows its blank
/// untitled file opens; one written beside a file with words in it
/// leaves that file on screen.
#[test]
fn a_new_file_opens_over_a_blank_one_but_never_over_work() {
    let mut app = ui::framework::testing::TestApp::<TypstEditorShell>::new().unwrap();
    app.settle();
    app.invoke(crate::resource::new_file(
        "letter.typ",
        None,
        "= Dear Dr Raman",
    ))
    .unwrap();
    app.settle();
    app.with_surface::<TypstApp, _>(|view| {
        assert_eq!(view.current_file().unwrap().name.0, "letter.typ");
    })
    .unwrap();
    app.invoke(crate::resource::new_file("notes.typ", None, "= Notes"))
        .unwrap();
    app.settle();
    app.with_surface::<TypstApp, _>(|view| {
        assert_eq!(
            view.current_file().unwrap().name.0,
            "letter.typ",
            "a file with words in it stays on screen"
        );
    })
    .unwrap();
}

#[test]
fn opening_another_document_clears_save_recovery_only_after_a_successful_read() {
    use crate::actions::TypstEffect;

    let mut app = ui::framework::testing::TestApp::<TypstEditorShell>::new().unwrap();
    let file = app.state().files.ids().next().unwrap().clone();
    let source = Workspace::paths().files.item(&file).source;
    app.invoke(ui::tilo_resource::Invoke::edit(&source.replace("= Document A")).unwrap())
        .unwrap();
    let dir = unique_dir("open-save-recovery");
    let destination = dir.join("missing/a.typ");
    let other = dir.join("b.typ");
    let save = TypstEffect::Save {
        path: Some(destination.to_string_lossy().into_owned()),
    };
    assert!(app.effect_confirmed(&save).is_err());
    app.settle();
    assert!(
        app.effect_confirmed(&TypstEffect::OpenFile {
            path: Some(other.to_string_lossy().into_owned()),
        })
        .is_err()
    );
    app.settle();
    let retry = app
        .with_surface::<TypstApp, _>(|view| {
            assert!(matches!(view.saves.state(), Some(ui::SaveState::Failed(_))));
            assert_eq!(view.current_file().unwrap().source, "= Document A");
            view.save_path.clone().unwrap()
        })
        .unwrap();
    assert_eq!(retry, destination);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    app.effect_confirmed(&TypstEffect::Save {
        path: Some(retry.to_string_lossy().into_owned()),
    })
    .unwrap();
    app.settle();
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "= Document A"
    );

    // Fail another save of A, then load B through the same live effect path.
    std::fs::remove_file(&destination).unwrap();
    std::fs::remove_dir(destination.parent().unwrap()).unwrap();
    assert!(app.effect_confirmed(&save).is_err());
    app.settle();
    std::fs::write(&other, "= Document B").unwrap();
    app.effect_confirmed(&TypstEffect::OpenFile {
        path: Some(other.to_string_lossy().into_owned()),
    })
    .unwrap();
    app.settle();
    app.with_surface::<TypstApp, _>(|view| {
        assert_eq!(view.current_file().unwrap().source, "= Document B");
        assert!(view.save_path.is_none(), "B must not retry A's destination");
        assert!(
            !matches!(view.saves.state(), Some(ui::SaveState::Failed(_))),
            "no stale Retry card"
        );
    })
    .unwrap();
    assert!(!destination.exists());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "= Document B");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pdf_export_uses_the_live_source_before_the_editor_pulls_it() {
    let mut replica = Replica::<Workspace>::new();
    replica
        .apply(crate::resource::new_file(
            "report.typ",
            None,
            "= Previous draft",
        ))
        .unwrap();
    let mut app = TypstApp::new();
    let mut doc = AutoCommit::load(&replica.save()).unwrap();
    present(&mut app, &doc);
    app.sync(&doc, &ROOT);
    assert_eq!(app.source_cache, "= Previous draft");

    let id = app.current.clone().unwrap();
    let source = Workspace::paths().files.item(&id).source;
    replica
        .apply(ui::tilo_resource::Invoke::edit(&source.replace("#let broken = (")).unwrap())
        .unwrap();
    doc = AutoCommit::load(&replica.save()).unwrap();
    present(&mut app, &doc);
    // As on the real effect path, the resource is presented but the
    // editor's per-frame source cache has not been pulled yet.
    let dir = unique_dir("export-live-source");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("report.pdf");
    std::fs::write(&path, b"previous export").unwrap();
    app.pending_resource_io = Some(ResourceIo::ExportPdf(path.clone()));
    app.sync(&doc, &ROOT);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"previous export",
        "the invalid live source must fail instead of exporting the older draft"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

fn cursor(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

use ui::synthetic_input::mouse_press;

/// Pump one event through the production `Widget::handle_event`, returning
/// whether it was consumed.
fn send(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes = HashMap::new();
    let node = app.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    ui::testing::Dispatch::new()
        .doc(doc)
        .layout(&taffy, node)
        .send(app, ev)
        .handled
}

/// Run one real draw pass so retained control geometry is measured,
/// exactly as they are for the live pointer path.
fn render_once(app: &mut TypstApp) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let presence = PresenceCtx::empty();
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

#[test]
fn source_sync_and_one_document_per_window() {
    // Case `first_sync_shows_the_first_file_and_compiles`.
    {
        let (app, doc) = fresh();
        assert!(!shown_source(&app, &doc).is_empty(), "the welcome example");
        assert_eq!(app.editor.text(), shown_source(&app, &doc));
        assert!(app.initialized);
        assert!(app.preview.page_count() >= 1, "rendered a page");
        assert_eq!(app.err_count, 0, "welcome demo has no errors");
    }
    // Case `there_is_no_document_switcher`: Ctrl+] and Ctrl+[ switch
    // nothing — a window edits one document — and write nothing.
    {
        let (mut app, mut doc) = fresh();
        doc.commit();
        let heads = doc.get_heads();
        let before = shown_source(&app, &doc);
        for key in ["]", "["] {
            let mut handled = false;
            app.handle_keyboard_shortcut(
                &Key::Character(key.into()),
                &char_key(key),
                Modifiers::from(ModifiersState::CONTROL),
                &mut ui::widget::Edits::lend(&mut doc),
                &mut handled,
            );
        }
        app.sync(&doc, &ROOT);
        assert_eq!(shown_source(&app, &doc), before);
        assert_eq!(doc.get_heads(), heads, "no switch, no write");
    }
    // Case `caret_line_col_tracks_newlines`.
    {
        let mut app = TypstApp::new();
        app.source_cache = "abc\nde\nf".to_string();
        app.editor.set_text("abc\nde\nf");
        app.editor.set_caret(5); // byte 5 → line 2, col 2 (just after 'd')
        assert_eq!(app.caret_line_col(), (2, 2));
    }
    // Case `without_a_dispatcher_the_source_cannot_change`.
    {
        use ui::widget::Widget as _;

        let (mut app, mut doc) = fresh();
        let before = shown_source(&app, &doc);
        assert!(send(&mut app, &mut doc, &char_key("x")));
        assert!(app.perform_semantic_action(
            "typst.source",
            &ui::a11y::Action::SetValue("= Accessible edit".into()),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        ));
        app.sync(&doc, &ROOT);
        assert_eq!(shown_source(&app, &doc), before);
        assert_eq!(app.editor.text(), before);
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
        assert!(!app.status.is_shown(), "no notice pending");
        assert!(
            !app.needs_animation_frame(),
            "a focused-but-idle editor must not drive continuous redraws"
        );
    }
    // Case `pending_native_dialog_has_a_stable_non_animation_deadline`.
    {
        let (mut app, doc) = fresh();
        assert!(app.next_wake_deadline().is_none());
        let (services, _recorder) = ui::host::HostServices::recording();
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        ui::host::scoped(&services, || app.open());
        let deadline = app
            .next_wake_deadline()
            .expect("file dialogs must complete without animation or further input");
        assert_eq!(
            app.next_wake_deadline(),
            Some(deadline),
            "queries must not keep postponing polling"
        );
        work.run_pending();
        app.sync(&doc, &ROOT);
        assert!(app.io.is_none());
        // What the dialog came to may be said in the window's bar; only
        // that notice leaving wakes the editor, no polling.
        assert_eq!(
            app.next_wake_deadline(),
            app.status.deadline(),
            "nothing polls once the dialog is done"
        );
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
    // Caret/page metadata belongs to the semantic status strip; explicit
    // compile/export feedback remains transient and fades after its linger.
    {
        let (mut app, doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert_eq!(app.editor_rect.y1, app.status_rect.y0);
        assert_eq!(app.preview_rect.y1, app.status_rect.y0);
        assert_eq!(app.status_rect.y1, app.rect.y1);
        assert!(!app.status_left_label().is_empty());
        assert!(!app.status_right_label().is_empty());
        app.status.show("Compiled".to_owned());
        app.sync(&doc, &ROOT);
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.text),
            Some("Compiled".to_owned()),
            "transient feedback is said in the window's bar"
        );
        let gone = app.status.deadline().expect("it leaves on its own");
        assert!(gone > ui::time::now(), "for a while");
    }
}

// ---- helpers for the preview-interaction + diagnostics tests ----------

use crate::compile::DiagKind;
use winit::keyboard::ModifiersState;

use ui::synthetic_input::char_key;

/// A vertical wheel notch (`+` = up/away). A tilt component can be supplied
/// via `x` for the horizontal-pan path.
fn wheel(x: f32, y: f32) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(x, y)
}

/// Like [`send`] but with explicit modifiers held.
fn send_mods(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent, mods: Modifiers) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes = HashMap::new();
    let node = app.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    ui::testing::Dispatch::new()
        .modifiers(mods)
        .doc(doc)
        .layout(&taffy, node)
        .send(app, ev)
        .handled
}

fn unique_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "typst_editor_{tag}_{}_{}",
        std::process::id(),
        ui::time::try_unix_now_ns().unwrap()
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
            send_mods(&mut app, &mut doc, &char_key("="), ctrl),
            "Ctrl+= consumed"
        );
        let zoomed_in = app.preview.zoom();
        assert!(
            zoomed_in > base,
            "Ctrl+= zoomed the preview in ({base} -> {zoomed_in})"
        );

        assert!(
            send_mods(&mut app, &mut doc, &char_key("-"), ctrl),
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
        let (mut app, mut doc) = workspace(Some("errors"));
        app.stage_scenario(Some("errors"));
        app.sync(&doc, &ROOT);
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));

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
            let p = app.diagnostics.row_rect(i).center();
            send(&mut app, &mut doc, &cursor(p));
            assert!(
                send(&mut app, &mut doc, &mouse_press()),
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
        let broken = compile::compile_source(&app.parts, "Body #undefined_var end.\n");
        assert!(
            broken.document.is_none(),
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
            app.err_count >= 1 && !app.show_diagnostics,
            "a failure while typing is counted, not popped open"
        );

        app.compile_feedback = true;
        app.apply_output(compile::compile_source(
            &app.parts,
            "Body #undefined_var end.\n",
        ));
        assert!(
            app.show_diagnostics,
            "an explicit Compile shows why it failed"
        );

        let good = compile::compile_source(&app.parts, "= Fixed\n\nBody.\n");
        assert!(good.document.is_some());
        app.apply_output(good);
        assert!(
            !app.preview.is_stale(),
            "a clean recompile clears the stale flag"
        );
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
    // The app's own tree is the two panes: the commands and the views are
    // the window bar's slots, which the window publishes, not the editor.
    {
        let (mut app, _doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
        render_once(&mut app);
        let tree = app.accessibility().expect("Typst semantics");
        assert_eq!(tree.role, ui::a11y::Role::Document);
        assert!(
            tree.children
                .iter()
                .all(|node| node.role != ui::a11y::Role::Button),
            "no command or view switch is drawn in the body: {:?}",
            tree.children
                .iter()
                .map(|node| node.role)
                .collect::<Vec<_>>()
        );
        let declared = ui::window_toolbar::declared().expect("the window declares its slots");
        assert!(declared.views.is_some(), "the panes are the bar's views");
        assert!(
            declared.location.is_some(),
            "the document is the bar's location"
        );
    }
}

fn control_rects(app: &TypstApp) -> Vec<(String, Rect)> {
    app.chrome
        .children()
        .iter()
        .filter(|child| child.get_rect().area() > 0.0)
        .map(|child| (child.name().to_string(), child.get_rect()))
        .collect()
}
/// An empty source shows the preview's empty state, not the blank white
/// page it compiles to; writing something brings the pages, and a late
/// result for text since deleted does not bring them back.
#[test]
fn an_empty_document_previews_as_empty_not_as_a_blank_page() {
    let mut live = ui::framework::testing::TestApp::<TypstEditorShell>::new().unwrap();
    live.harness().resize((1280, 800)).unwrap();
    live.settle();
    live.with_surface::<TypstApp, _>(|view| {
        assert!(view.current_file().unwrap().source.is_empty());
        assert!(view.preview.is_blank(), "a new document previews as empty");
        assert_eq!(view.status_right_label(), "Empty document");
    })
    .unwrap();

    let (mut app, _doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1400.0, 900.0));
    app.worker = None;
    let written = compile::compile_source(&app.parts, "= Written\n");
    app.request_compile("  \n");
    assert!(app.preview.is_blank());
    assert_eq!(app.preview.page_count(), 0);
    app.apply_output(written);
    assert!(
        app.preview.is_blank(),
        "a result for text since deleted stays superseded"
    );
    app.request_compile("= Written\n");
    assert!(!app.preview.is_blank());
    assert_eq!(app.preview.page_count(), 1);
    assert_eq!(app.status_right_label(), "1 page, fit width");
    app.preview.zoom_in();
    assert_eq!(app.status_right_label(), "1 page, 125%");
}

/// The window bar holds the editor's controls, so the body holds none: the
/// panes have the window from its top edge to the status bar at every width,
/// which is what the 40 px row above them used to take.
#[test]
fn the_panes_have_the_window_from_its_top_edge() {
    for width in [320.0, 360.0, 390.0, 480.0, 560.0, 720.0, 800.0, 1280.0] {
        for (errors, warnings) in [(0, 0), (1, 1)] {
            let (mut app, _) = fresh();
            app.err_count = errors;
            app.warn_count = warnings;
            app.layout_in(Rect::new(0.0, 0.0, width, 760.0));
            render_once(&mut app);
            let top = app.editor_rect.union(app.preview_rect).union(app.diag_rect);
            assert_eq!(top.y0, 0.0, "no row stands above the panes at {width}px");
            assert_eq!(
                top.y1, app.status_rect.y0,
                "the panes reach the status bar at {width}px"
            );
            assert_eq!(top.width(), width, "the panes span the window");
        }
    }
}

/// The bar says where the window is — the document's name, in the one place
/// a person looks first — and holds every command the row used to.
#[test]
fn the_bar_says_where_the_window_is_and_holds_every_command() {
    let (mut app, _) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 760.0));
    render_once(&mut app);
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    let name = app.current_name();
    let location = declared.location.expect("the document is the location");
    assert_eq!(location.label, name);
    assert!(location.editable, "the name is renamed in place");
    let ids: Vec<&str> = declared
        .actions
        .iter()
        .flat_map(|action| {
            std::iter::once(action.id.as_str())
                .chain(action.items.iter().map(|item| item.id.as_str()))
        })
        .collect();
    for wanted in [
        "typst.compile",
        "typst.problems",
        "typst.outline",
        "typst.zoom-in",
        "typst.zoom-out",
        "typst.fit",
        "typst.save",
        "typst.open",
        "typst.export-pdf",
    ] {
        assert!(ids.contains(&wanted), "{wanted} is a slot in {ids:?}");
    }
    let views = declared
        .views
        .expect("the source and the preview are views");
    assert_eq!(
        views.choices.len(),
        2,
        "one view per pane, at every width: no switch of the app's own"
    );
}

/// A document with changes nobody has written yet says so beside its name;
/// writing it to a file says no more.
#[test]
fn the_bar_marks_an_unsaved_document_until_it_is_saved() {
    let mut app = ui::framework::testing::TestApp::<TypstEditorShell>::new().unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app.settle();
    let file = app.state().files.ids().next().expect("a file").clone();
    let source = Workspace::paths().files.item(&file).source;
    app.with_surface::<TypstApp, _>(|view| {
        assert!(!view.unsaved(), "a document nobody typed in is not unsaved")
    })
    .unwrap();

    // Typing is a splice into the source field, as the editor's keys make it.
    app.invoke(ui::tilo_resource::Invoke::edit(&source.splice(0, 0, "= Typed\n")).unwrap())
        .unwrap();
    app.harness()
        .wait_for_visuals(tilo_test_env::hang_bound())
        .unwrap();
    app.with_surface::<TypstApp, _>(|view| assert!(view.unsaved(), "typed text is unsaved work"))
        .unwrap();
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    assert!(
        declared
            .location
            .expect("the document is the location")
            .edited,
        "the bar says so after the document's name"
    );

    // Writing it where it can go ends that.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("typed.typ");
    app.with_surface::<TypstApp, _>(|view| view.save_typ_to(&path).expect("the file is written"))
        .unwrap();
    app.settle();
    app.with_surface::<TypstApp, _>(|view| {
        assert!(!view.unsaved(), "the source as written is not unsaved work")
    })
    .unwrap();
}

/// The window's search finds in the source: a query is matched in the text the
/// editor shows, and stepping to a match puts the caret there and scrolls it
/// into view.
#[test]
fn the_windows_search_finds_in_the_source() {
    use ui::widget::{FindOptions, Findable};

    let (mut app, _doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 760.0));
    render_once(&mut app);
    let body = app.editor.text().to_owned();
    assert!(body.contains("Smooth motion"), "the sample is the source");
    let found = app.editor.find("motion", FindOptions::default());
    let expected = body.to_lowercase().matches("motion").count();
    assert_eq!(found.len(), expected, "every match of the query is found");
    // A step lands the caret on the match, on its line.
    app.editor.reveal(0);
    let first = found[0].clone();
    assert_eq!(app.editor.caret(), first.end);
    let line = body[..first.start].bytes().filter(|b| *b == b'\n').count() + 1;
    assert_eq!(app.caret_line_col().0, line, "the caret is on its line");
    // Closing the search stops painting the matches.
    app.editor.clear_find();
    assert!(app.editor.find("", FindOptions::default()).is_empty());
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
    assert!(app.splitter.hit_rect().area() > 0.0);

    // Narrow → compact: a single pane, no divider.
    app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
    assert!(app.compact);
    assert_eq!(app.splitter.hit_rect().area(), 0.0);
    assert!(app.editor_rect.area() > 0.0, "editor pane shown");
    assert_eq!(app.preview_rect.area(), 0.0, "preview hidden in compact");

    // Resize back to wide → must reflow to split again (not frozen).
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
    assert!(!app.compact, "reflowed back to split after widening");
    assert!(app.preview_rect.area() > 0.0);
}
/// Larger text narrows what fits in one pane: the compact layout is chosen
/// from the space the text leaves, and the panes still have the whole window.
#[test]
fn text_scale_compacts_before_the_panes_collide() {
    let (mut app, _doc) = fresh();
    app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
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

    assert!(app.compact, "1280px is compact at 200% text scale");
    assert_eq!(app.editor_rect.y0, 0.0, "the pane starts at the top edge");
    for (_, rect) in control_rects(&app) {
        assert!(
            rect.y1 <= app.status_rect.y0,
            "the only control left in the body is the panel's close button"
        );
    }
}

/// The bar's views pick the pane a narrow window shows, and its slots run
/// the commands the row's buttons ran — zoom among them.
#[test]
fn the_bars_views_pick_the_pane_and_its_slots_run_the_commands() {
    let mut app = live_fixture((560, 720));
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
            id: "typst.view.preview".to_owned(),
        });
    app.settle();
    assert!(
        app.with_widget(|surface: &mut TypstApp| matches!(
            surface.compact_view,
            CompactPane::Preview
        ) && surface.preview_rect.area() > 0.0
            && surface.editor_rect.area() == 0.0)
            .unwrap()
    );
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
            id: "typst.view.editor".to_owned(),
        });
    app.settle();
    assert!(
        app.with_widget(|surface: &mut TypstApp| matches!(
            surface.compact_view,
            CompactPane::Editor
        ))
        .unwrap()
    );

    // Every command is a slot at any width, and the zoom ones zoom.
    for theme in [Theme::dark(), Theme::dark().with_text_scale(2.0)] {
        app.harness().relayout_themed((390, 520), theme).unwrap();
        app.settle();
        let declared = ui::window_toolbar::declared().expect("the window declares its slots");
        for wanted in [
            "typst.compile",
            "typst.save",
            "typst.open",
            "typst.export-pdf",
            "typst.zoom-out",
            "typst.zoom-in",
            "typst.fit",
            "typst.problems",
        ] {
            assert!(
                declared.actions.iter().any(|action| action.id == wanted),
                "{wanted} remains a slot at 390px"
            );
        }
        let before = app
            .with_widget(|surface: &mut TypstApp| surface.preview.zoom())
            .unwrap();
        app.harness()
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
                id: "typst.zoom-in".to_owned(),
            });
        app.settle();
        assert!(
            app.with_widget(|surface: &mut TypstApp| surface.preview.zoom() > before)
                .unwrap(),
            "the bar's zoom in zooms the preview"
        );
    }
}
