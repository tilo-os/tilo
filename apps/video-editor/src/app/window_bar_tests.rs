//! The editor's controls live in the window's bar: the media toggle, the
//! project and its picker, Split, Export, the inspector's toggle, Import and
//! History. The editor draws no header row and none of those controls; each
//! bar control runs the editor's published action for it.

use std::path::PathBuf;

use ui::framework::testing::TestApp;
use ui::widget::{SplitLayout, Widget};
use ui::window_toolbar::{ToolbarEvent, WindowToolbar};
use vello::kurbo::Rect;

use super::{EXPORT, IMPORT, INSPECTOR, SPLIT};
use crate::actions::VideoEditorEffect;
use crate::app::VideoEditorApp;
use crate::model::Project;

/// The sample project in an editor `w` wide, laid out as a host lays it out;
/// file choosers answered by a recording host, never put on a screen.
fn sample(w: f64) -> VideoEditorApp {
    ui::host::install_default(ui::host::HostServices::recording().0);
    let doc = crate::seed::sample_document();
    let mut app = VideoEditorApp::new();
    app.actions = Some(ui::framework::testing::dispatcher::<Project>(
        <crate::VideoEditor as ui::framework::ResourceApp>::ID,
    ));
    app.rect = Rect::new(0.0, 0.0, w, 800.0);
    ui::framework::ResourceSurface::<Project, VideoEditorEffect>::present(
        &mut app,
        &std::rc::Rc::new(crate::seed::sample_project()),
    );
    Widget::sync(&mut app, &doc, &automerge::ROOT);
    app
}

fn action(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

fn bar_action(toolbar: &WindowToolbar, id: &str) -> ui::window_toolbar::ToolbarAction {
    toolbar
        .actions
        .iter()
        .find(|action| action.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} is a bar action"))
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

/// The editor on the sample project, opened as a person opens it.
fn opened() -> TestApp<crate::VideoEditor> {
    ui::host::install_default(ui::host::HostServices::recording().0);
    let mut app = TestApp::<crate::VideoEditor>::new().expect("the editor opens");
    app.invoke_confirmed(
        ui::tilo_resource::Invoke::edit(&crate::resource::load_project(
            &crate::seed::sample_project(),
        ))
        .expect("the project encodes"),
    )
    .expect("the project loads");
    app.harness().relayout();
    app
}

#[test]
fn the_editor_draws_no_header_row_and_its_controls_are_in_the_window_bar() {
    for width in [1280.0, 420.0] {
        let app = sample(width);
        let r = app.regions();
        assert!(
            (r.preview.y0 - app.rect.y0).abs() < 1e-9,
            "{width}px: the preview starts at the editor's top edge"
        );
        assert!(
            (r.inspector.y0 - app.rect.y0).abs() < 1e-9 || r.inspector_overlay,
            "{width}px: a docked inspector starts at the top edge"
        );

        let toolbar = app.window_toolbar();
        assert_eq!(
            toolbar.sidebar,
            Some(app.bin_flap.is_open(app.bin_layout())),
            "{width}px: the media toggle is the bar's sidebar slot"
        );
        assert_eq!(
            toolbar
                .location
                .as_ref()
                .map(|location| location.label.as_str()),
            Some(app.project_label().as_str()),
            "{width}px: the bar names the project"
        );
        let ids: Vec<&str> = toolbar.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, [SPLIT, EXPORT, INSPECTOR, IMPORT]);
        assert!(bar_action(&toolbar, IMPORT).menu, "Import is in the menu");

        let mut drawn = Vec::new();
        labels(
            &Widget::accessibility(&app).expect("the editor's semantics"),
            &mut drawn,
        );
        for own in [
            "Editor commands",
            "Toggle media bin",
            "Toggle inspector",
            "Import media",
            "Split at playhead",
            "Export video",
            ui::widget::HISTORY_CONTROL_ID,
        ] {
            assert!(
                !drawn.iter().any(|label| label == own),
                "{width}px: the editor draws no {own} of its own: {drawn:?}"
            );
        }
    }
}

#[test]
fn the_bar_shows_and_hides_the_media_and_the_inspector() {
    let mut app = sample(1280.0);
    assert_eq!(app.bin_layout(), SplitLayout::Docked);
    let open = app.bin_flap.is_open(app.bin_layout());
    app.run_window_bar_event(&ToolbarEvent::ToggleSidebar);
    assert_eq!(app.bin_flap.is_open(app.bin_layout()), !open);
    assert_eq!(app.window_toolbar().sidebar, Some(!open));

    let open = app.inspector_flap.is_open(app.inspector_split());
    app.run_window_bar_event(&action(INSPECTOR));
    assert_eq!(app.inspector_flap.is_open(app.inspector_split()), !open);
    let toggle = bar_action(&app.window_toolbar(), INSPECTOR);
    assert_eq!(toggle.on, !open, "the toggle shows the inspector's state");
    assert_eq!(
        toggle.name,
        if open {
            "Show inspector"
        } else {
            "Hide inspector"
        }
    );
}

#[test]
fn the_bar_splits_exports_and_imports_through_the_editors_handlers() {
    let mut app = sample(1280.0);
    let clip = app
        .project
        .top_video_clip_at(10.0)
        .cloned()
        .expect("the sample has a clip at 10s");
    app.selection.clear();
    app.playhead = clip.start + clip.duration() / 2.0;
    assert!(
        !bar_action(&app.window_toolbar(), SPLIT).enabled,
        "with nothing selected, Split has nothing to cut"
    );
    app.selection.select_only(clip.id);
    assert!(bar_action(&app.window_toolbar(), SPLIT).enabled);
    let before = app.project.clips.len();
    app.run_window_bar_event(&action(SPLIT));
    assert!(
        app.project.clips.len() > before,
        "Split cut the selected clip at the playhead"
    );

    app.run_window_bar_event(&action(EXPORT));
    assert!(app.presentation.export_open, "Export opens its settings");
    app.presentation.export_open = false;

    app.run_window_bar_event(&action(IMPORT));
    assert!(
        app.import_rx.is_some() || app.import_contents_rx.is_some(),
        "Import asks for media"
    );
}

#[test]
fn the_project_picker_lists_open_save_and_the_recent_projects() {
    let mut app = sample(1280.0);
    let open = PathBuf::from("/home/you/Videos/Open.qvp");
    let other = PathBuf::from("/home/you/Videos/Reel.qvp");
    app.project_path = Some(open.clone());
    app.recents = vec![open, other];
    let location = app.window_toolbar().location.expect("the project");
    let rows: Vec<(&str, bool)> = location
        .choices
        .iter()
        .map(|choice| (choice.label.as_str(), choice.separated))
        .collect();
    assert_eq!(
        rows,
        [
            ("Open…", false),
            ("Save", false),
            ("Save as…", false),
            ("Reel", true)
        ],
        "the project already open is not offered as somewhere to go"
    );
    assert!(location.choices.iter().all(|choice| !choice.current));

    // A value naming no recent project opens nothing.
    app.run_window_bar_event(&ToolbarEvent::Commit {
        text: "project:recent:/elsewhere/Other.qvp".into(),
    });
    assert!(app.pending_open.is_none() && app.project_rx.is_none());
}

#[test]
fn the_editors_bar_actions_are_published_actions() {
    let mut app = opened();
    let editor = |app: &mut TestApp<crate::VideoEditor>| -> (bool, bool, bool) {
        let editor = app.surface_mut::<VideoEditorApp>();
        (
            editor.bin_flap.is_open(editor.bin_layout()),
            editor.inspector_flap.is_open(editor.inspector_split()),
            editor.presentation.export_open,
        )
    };
    let (media, inspector, _) = editor(&mut app);
    app.effect(&VideoEditorEffect::ToggleMedia)
        .expect("Show or hide media runs");
    app.effect(&VideoEditorEffect::ToggleInspector)
        .expect("Show or hide the inspector runs");
    let (media_now, inspector_now, export_open) = editor(&mut app);
    assert_eq!(media_now, !media);
    assert_eq!(inspector_now, !inspector);
    assert!(!export_open);
    app.effect(&VideoEditorEffect::ShowExport)
        .expect("Show export settings runs");
    assert!(editor(&mut app).2, "the export settings show");
    app.surface_mut::<VideoEditorApp>().presentation.export_open = false;

    let clip = app
        .state()
        .top_video_clip_at(10.0)
        .cloned()
        .expect("the sample has a clip at 10s");
    let before = app.state().clips.len();
    {
        let editor = app.surface_mut::<VideoEditorApp>();
        editor.selection.select_only(clip.id);
        editor.playhead = clip.start + clip.duration() / 2.0;
    }
    app.effect(&VideoEditorEffect::SplitSelection)
        .expect("Split selected clips runs");
    app.settle();
    assert!(
        app.state().clips.len() > before,
        "the selected clip is cut in the record"
    );
    assert!(app.undo(), "the split is one step");
    assert_eq!(app.state().clips.len(), before);
}

#[test]
fn history_is_the_window_bars_control() {
    let mut app = TestApp::<crate::VideoEditor>::capture(None).expect("the sample opens");
    app.harness().present_in_window_bar();
    app.harness().relayout();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Closed),
        "the window's bar offers History"
    );
    app.harness().press_window_history().expect("History opens");
    app.settle();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Open)
    );
}

#[test]
fn the_location_says_when_the_project_has_unsaved_edits() {
    let mut app = sample(1280.0);
    let location = |app: &VideoEditorApp| {
        app.window_toolbar()
            .location
            .expect("the project is the window's location")
    };
    app.project_path = Some(PathBuf::from("/home/you/Videos/Harbour.tvp"));
    // The sample has content the file does not hold yet.
    let unsaved = location(&app);
    assert!(unsaved.edited, "unsaved work reads as edited");
    assert_eq!(
        unsaved.label,
        app.project_label(),
        "the label is the name alone"
    );
    assert_eq!(unsaved.name, app.project_status_line());

    app.saved_heads = Some(app.doc_heads.clone());
    let saved = location(&app);
    assert!(!saved.edited, "a saved project does not");
    assert_eq!(saved.label, unsaved.label);
    assert_eq!(saved.name, app.project_status_line());
    assert_ne!(
        saved.name, unsaved.name,
        "a screen reader hears the difference"
    );
}

/// A failed save's Try again stands in the window's bar right after the
/// failure, wide and compact: it names itself where the row has room and
/// never goes into the overflow menu, and the failure is cut short only
/// where nothing that yields to it stands beside it.
#[test]
fn a_failed_saves_try_again_stays_in_the_window_bar_wide_and_compact() {
    let mut app = opened();
    app.harness().present_in_window_bar();
    {
        let editor = app.surface_mut::<VideoEditorApp>();
        editor.project_path = Some(PathBuf::from("/definitely/not/a/directory/x.qvp"));
        editor.queue_save_project();
    }
    app.settle();
    let held = ui::window_toolbar::notice().expect("the failure is in the bar");
    assert_eq!(
        held.action.as_ref().map(|action| action.id.as_str()),
        Some(crate::app::notices::SAVE_RETRY_ACTION),
        "with Try again beside it: {held:?}"
    );
    for width in [1280.0, 420.0] {
        let shown = app
            .harness()
            .window_bar_notice(width)
            .expect("the failure is up in the bar");
        let button = shown.button.as_ref().expect("Try again stands in the row");
        assert_eq!(button.name, "Try again");
        assert!(
            button.bounds.x0 > shown.text_bounds.x1,
            "{width} px: {shown:?}"
        );
        // Wide, inline; compact, in a strip of its own under the row,
        // its text whole.
        assert_eq!(
            shown.strip.is_some(),
            width < 1280.0,
            "{width} px: {shown:?}"
        );
        assert!(shown.whole && button.labelled, "{width} px: {shown:?}");
    }
}
