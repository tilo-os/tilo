//! The editor's controls live in the window's bar: the media toggle, the
//! project and its picker, Split, Export, the inspector's toggle, Import and
//! History. The editor draws no header row and none of those controls; each
//! bar control runs the editor's published action for it.

use ui::framework::testing::TestApp;
use ui::widget::{SplitLayout, Widget};
use ui::window_toolbar::ToolbarEvent;
use vello::kurbo::Rect;

use crate::actions::VideoEditorEffect;
use crate::app::VideoEditorApp;
use crate::model::{Project, TrackKind};

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
    app.present(&std::rc::Rc::new(crate::seed::sample_project()));
    Widget::sync(&mut app, &doc, &automerge::ROOT);
    app
}

fn bar_action(app: &VideoEditorApp, name: &str) -> ui::window_toolbar::ToolbarAction {
    app.bar_entry(name)
        .unwrap_or_else(|| panic!("{name} is a bar action"))
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
fn the_bar_holds_the_editors_controls() {
    for width in [1280.0, 420.0] {
        let app = sample(width);
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
        let icons: Vec<&str> = toolbar.actions.iter().map(|a| a.icon.as_str()).collect();
        assert_eq!(
            icons,
            [
                "scissors",
                "columns-2",
                "upload",
                "panel-right",
                "folder-open"
            ]
        );
        assert!(
            bar_action(&app, "Import media").menu,
            "Import is in the menu"
        );
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
    app.press_bar("panel-right");
    assert_eq!(app.inspector_flap.is_open(app.inspector_split()), !open);
    let toggle = bar_action(&app, "panel-right");
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
        !bar_action(&app, "Split at playhead").enabled,
        "with nothing selected, Split has nothing to cut"
    );
    app.selection.select_only(clip.id);
    assert!(bar_action(&app, "Split at playhead").enabled);
    let before = app.project.clips.len();
    app.press_bar("Split at playhead");
    assert!(
        app.project.clips.len() > before,
        "Split cut the selected clip at the playhead"
    );

    app.press_bar("Export");
    assert!(app.presentation.export_open, "Export opens its settings");
    app.presentation.export_open = false;

    app.press_bar("Import media");
    assert!(
        app.import_rx.is_some() || app.import_contents_rx.is_some(),
        "Import asks for media"
    );
}

#[test]
fn the_project_picker_offers_a_project_file_in_and_out() {
    let app = sample(1280.0);
    let location = app.window_toolbar().location.expect("the project");
    let rows: Vec<&str> = location
        .choices
        .iter()
        .map(|choice| choice.label.as_str())
        .collect();
    assert_eq!(rows, ["Open project file…", "Export project file…"]);
    assert!(!location.edited, "the project is kept as it is edited");
    assert!(location.choices.iter().all(|choice| !choice.current));
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

/// Press the notice's action `index` in a bar `width` wide, by pointer or
/// through its semantic action.
fn press_notice(app: &mut TestApp<crate::VideoEditor>, width: f64, index: usize, semantic: bool) {
    let harness = app.harness();
    let pressed = if semantic {
        harness.invoke_window_notice_action(width, index)
    } else {
        harness.click_window_notice_action(width, index)
    };
    pressed.expect("the notice's action is in the bar");
    app.settle();
}

/// The labels of the notice's buttons in a bar `width` wide.
fn notice_buttons(app: &mut TestApp<crate::VideoEditor>, width: f64) -> Vec<String> {
    app.harness()
        .window_bar_notice(width)
        .expect("the notice is up")
        .buttons
        .into_iter()
        .map(|button| button.name)
        .collect()
}

const WAYS: [(f64, bool); 4] = [
    (1280.0, false),
    (1280.0, true),
    (420.0, false),
    (420.0, true),
];

/// An import whose files were refused carries Choose media…, which opens
/// the media chooser again.
#[test]
fn a_refused_imports_choose_media_opens_the_chooser() {
    for (width, semantic) in WAYS {
        let dir = std::env::temp_dir().join(format!("tilo-ve-refused-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("not-a-video.mp4");
        std::fs::write(&bad, b"<!DOCTYPE html><html><body>nope</body></html>").unwrap();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = opened();
        app.harness().present_in_window_bar();
        app.surface_mut::<VideoEditorApp>()
            .pending_imports
            .push(bad);
        app.settle();
        assert_eq!(notice_buttons(&mut app, width), ["Choose media…"]);
        let asked = recorder.pick_requests().len();
        press_notice(&mut app, width, 0, semantic);
        assert_eq!(
            recorder.pick_requests().len(),
            asked.saturating_add(1),
            "the media chooser was asked again ({width}, semantic: {semantic})"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// An Open that replaced the timeline says Undo to go back, and carries
/// Undo, which brings the timeline back.
#[test]
fn an_opens_undo_brings_the_timeline_back() {
    for (width, semantic) in WAYS {
        let dir = std::env::temp_dir().join(format!("tilo-ve-undo-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("Other.qvp");
        std::fs::write(
            &other,
            crate::project_file::encode(&Project::new("Other")).unwrap(),
        )
        .unwrap();
        let mut app = opened();
        app.harness().present_in_window_bar();
        let before = app.surface_mut::<VideoEditorApp>().project.clips.len();
        assert!(before > 0, "the sample has a timeline to replace");
        app.effect(&VideoEditorEffect::OpenProjectFile {
            path: Some(ui::tilo_resource::AbsolutePath::new(&other).unwrap()),
        })
        .expect("the project file opens");
        app.settle();
        assert_eq!(app.surface_mut::<VideoEditorApp>().project.clips.len(), 0);
        assert_eq!(notice_buttons(&mut app, width), ["Undo"]);
        press_notice(&mut app, width, 0, semantic);
        assert_eq!(
            app.surface_mut::<VideoEditorApp>().project.clips.len(),
            before,
            "Undo brought the timeline back ({width}, semantic: {semantic})"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn a_compact_preview_starts_right_under_the_window_bar() {
    let app = sample(420.0);
    let r = app.regions();
    let inset = app.preview_inset();
    assert!(
        (r.canvas.y0 - (r.preview.y0 + inset)).abs() < 1.0,
        "the picture starts right under the bar, not below an empty band ({:?} in {:?})",
        r.canvas,
        r.preview
    );
    assert!(
        (r.canvas.x0 - (r.preview.x0 + inset)).abs() < 1.0
            && (r.canvas.x1 - (r.preview.x1 - inset)).abs() < 1.0,
        "the picture fills the preview's width"
    );
    assert!(
        (r.transport.y0 - (r.canvas.y1 + inset)).abs() < 1.0,
        "the transport sits right under the picture"
    );
    assert!(
        (app.timeline_split.ratio().position() - super::PREVIEW_SHARE).abs() < 1e-6,
        "the chosen split is kept for a wider window"
    );

    let wide = sample(1280.0);
    let r = wide.regions();
    let chosen = wide.timeline_split.ratio().first_extent(wide.rect.height());
    assert!(
        (r.timeline.y0 - (wide.rect.y0 + chosen).round()).abs() < 1e-9,
        "wide, the timeline starts where the split puts it"
    );
}

/// The blank editor `width` wide, choosers answered by `services`.
fn blank_at(width: u32) -> TestApp<crate::VideoEditor> {
    let mut app = TestApp::<crate::VideoEditor>::new().expect("the editor opens");
    app.harness().present_in_window_bar();
    app.harness()
        .relayout_at((width, 800))
        .expect("the editor lays out");
    app.settle();
    app
}

/// The empty timeline's button in the exported tree: its node and where
/// it stands.
fn timeline_empty_button(
    app: &mut TestApp<crate::VideoEditor>,
    label: &str,
) -> (ui::accesskit::NodeId, Rect) {
    let Some((drawn, action)) = app
        .with_surface::<VideoEditorApp, _>(|editor| {
            editor.timeline_empty_action_rect(&editor.regions())
        })
        .expect("the editor surface")
    else {
        panic!("the empty timeline offers {label}");
    };
    assert_eq!(action.label(), label);
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(id, node)| {
            let bounds = node.bounds()?;
            let bounds = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
            (node.role() == ui::accesskit::Role::Button
                && node.label() == Some(label)
                && (bounds.center() - drawn.center()).hypot() < 1.0)
                .then_some((id, bounds))
        })
        .unwrap_or_else(|| panic!("{label} is in the tree where it is drawn"))
}

/// Fold the Media pane in or out, as its toggle in the bar does, and let
/// it land.
fn toggle_media_pane(app: &mut TestApp<crate::VideoEditor>) {
    let editor = app.surface_mut::<VideoEditorApp>();
    editor.toggle_bin();
    let layout = editor.bin_layout();
    editor.bin_flap.settle(layout);
    app.settle();
}

/// Whether the empty timeline offers its own button now, drawn and in the
/// tree: a Button inside the timeline's empty state.
fn timeline_offers_a_button(app: &mut TestApp<crate::VideoEditor>) -> bool {
    let (drawn, content) = app
        .with_surface::<VideoEditorApp, _>(|editor| {
            let r = editor.regions();
            (
                editor.timeline_empty_action_rect(&r).is_some(),
                editor.timeline_content_area(&r),
            )
        })
        .expect("the editor surface");
    let described = app.harness().a11y_nodes().into_iter().any(|(_, node)| {
        node.role() == ui::accesskit::Role::Button
            && node.bounds().is_some_and(|b| {
                content.contains(vello::kurbo::Point::new(
                    f64::midpoint(b.x0, b.x1),
                    f64::midpoint(b.y0, b.y1),
                ))
            })
    });
    assert_eq!(drawn, described, "what is drawn is what the tree says");
    drawn
}

/// One button per job: while the Media pane is shown, its Import answers
/// the empty timeline's words and the timeline carries no button of its
/// own; hidden, at any width, the timeline carries Import media, which
/// asks for files as the pane's Import does, by pointer and through its
/// semantic action. The button comes and goes as the pane is toggled.
#[test]
fn the_empty_timelines_import_media_stands_in_for_a_hidden_media_pane() {
    let mut wide = blank_at(1280);
    assert!(
        !timeline_offers_a_button(&mut wide),
        "the pane is shown wide"
    );
    toggle_media_pane(&mut wide);
    assert!(
        timeline_offers_a_button(&mut wide),
        "hidden, the button shows"
    );
    toggle_media_pane(&mut wide);
    assert!(!timeline_offers_a_button(&mut wide), "shown again, it goes");

    for (width, semantic) in [(1280, false), (1280, true), (420, false), (420, true)] {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = blank_at(width);
        if app
            .with_surface::<VideoEditorApp, _>(|editor| editor.media_pane_shown())
            .expect("the editor surface")
        {
            toggle_media_pane(&mut app);
        }
        let (node, bounds) = timeline_empty_button(&mut app, "Import media");
        let asked = recorder.pick_requests().len();
        if semantic {
            app.harness()
                .perform_a11y_action(node, &ui::a11y::Action::Activate)
                .expect("Import media answers its action");
        } else {
            app.harness().click_at(bounds.center().x, bounds.center().y);
        }
        app.settle();
        let picks = recorder.pick_requests();
        assert_eq!(
            picks.len(),
            asked + 1,
            "{width} px, semantic {semantic}: one chooser"
        );
        assert_eq!(
            picks.last().map(|pick| pick.title().to_owned()).as_deref(),
            Some("Import media")
        );
    }
}

/// With media to drag but the Media pane folded away, the empty timeline
/// carries Show media, which shows it; where the pane is in view it needs
/// no button.
#[test]
fn the_empty_timeline_shows_the_media_it_asks_to_drag() {
    let mut compact = sample(420.0);
    compact.project.clips.clear();
    let r = compact.regions();
    assert!(!compact.bin_is_visible(&r), "the Media pane starts folded");
    let (_, action) = compact
        .timeline_empty_action_rect(&r)
        .expect("the empty timeline offers an action");
    assert_eq!(action.label(), "Show media");
    let route = [
        ui::a11y::hash_key(&super::timeline_empty::TIMELINE_EMPTY_KEY),
        ui::a11y::hash_key(&super::timeline_empty::TIMELINE_EMPTY_ACTION_KEY),
    ];
    assert!(compact.a11y_timeline_action(&ui::a11y::Action::Activate, &route));
    assert!(
        compact.bin_flap.is_open(compact.bin_layout()),
        "Show media opened the Media pane"
    );

    let mut wide = sample(1280.0);
    wide.project.clips.clear();
    let r = wide.regions();
    assert!(wide.bin_is_visible(&r));
    assert!(
        wide.timeline_empty_action_rect(&r).is_none(),
        "beside the Media pane the words point at it"
    );
}

/// With clips the playhead crosses the ruler and the tracks and ends at
/// the last one: it never runs through the add-track row under them, with
/// one track or with more.
#[test]
fn the_playhead_ends_at_the_last_track_clear_of_the_add_track_row() {
    for width in [420.0, 1280.0] {
        let mut app = sample(width);
        for added in [false, true] {
            if added {
                app.add_track(TrackKind::Video);
            }
            let r = app.regions();
            assert!(!app.project.clips.is_empty(), "fixture: there are clips");
            let px = app.time_to_x(app.playhead, &r.lanes);
            let line = Rect::new(px - 1.0, r.ruler.y0, px + 1.0, app.playhead_bottom(&r));
            let lanes = app.lane_rects(&r);
            let last = lanes.last().map(|(_, _, lane)| *lane).expect("a track");
            assert!(
                (line.y1 - last.y1).abs() < 1e-9 && line.y1 > r.ruler.y1,
                "{width} px, {} tracks: the playhead ends at the last track: {line:?} vs {last:?}",
                lanes.len()
            );
            let buttons = app.add_track_buttons(&r);
            assert!(!buttons.is_empty(), "fixture: the add-track row is in view");
            for (kind, button) in buttons {
                let row = Rect::new(r.timeline.x0, button.y0, r.timeline.x1, button.y1);
                assert!(
                    line.intersect(row).area() <= 0.0,
                    "{width} px, {} tracks: the playhead {line:?} crosses the \
                     add-track row at {kind:?} {row:?}",
                    lanes.len()
                );
            }
        }
    }
}

/// On an empty timeline the playhead stays in the ruler, clear of the
/// add-track row and the empty state, which is centred on the tracks'
/// content beside the header column; with a clip it crosses the tracks.
#[test]
fn an_empty_timelines_playhead_stays_in_the_ruler() {
    for width in [420.0, 1280.0] {
        let mut app = sample(width);
        let r = app.regions();
        let last_track = app.lane_rects(&r).last().map(|(_, _, lane)| lane.y1);
        assert_eq!(
            Some(app.playhead_bottom(&r)),
            last_track,
            "{width} px: with clips the playhead spans the tracks"
        );

        app.project.clips.clear();
        app.project.media.clear();
        if app.media_pane_shown() {
            app.toggle_bin();
            let layout = app.bin_layout();
            app.bin_flap.settle(layout);
        }
        let r = app.regions();
        let bottom = app.playhead_bottom(&r);
        assert!(
            (bottom - r.ruler.y1).abs() < 1e-9,
            "{width} px: the playhead ends at the ruler"
        );
        let buttons = app.add_track_buttons(&r);
        assert!(!buttons.is_empty(), "{width} px: the add-track row shows");
        for (_, button) in &buttons {
            assert!(
                bottom <= button.y0,
                "{width} px: the playhead stays clear of {button:?}"
            );
        }
        let empty = app.timeline_empty(&r).expect("the empty state shows");
        let content = app.timeline_content_area(&r);
        assert_eq!(empty.area, content, "{width} px: centred on the content");
        assert!(
            content.x0 >= r.gutter.x1 - 1e-9,
            "{width} px: the header column is not counted"
        );
        assert!(bottom <= content.y0, "{width} px: clear of the empty state");
        for (_, button) in &buttons {
            assert!(
                button.y1 <= content.y0,
                "{width} px: the add-track row stands above the empty state"
            );
        }
        let (action, _) = app
            .timeline_empty_action_rect(&r)
            .expect("the empty state carries Import media");
        assert!(
            (action.center().x - content.center().x).abs() < 1.0,
            "{width} px: the empty state is centred on the content, {action:?} in {content:?}"
        );
    }
}

/// The editor opened on the sample project `width` wide, its playhead
/// parked past the last clip, where the preview has nothing to show.
fn parked_between_clips(width: u32) -> TestApp<crate::VideoEditor> {
    let mut app = opened();
    app.harness()
        .relayout_at((width, 800))
        .expect("the editor lays out");
    let editor = app.surface_mut::<VideoEditorApp>();
    editor.playhead = editor.project.duration();
    app.settle();
    app
}

/// The preview shows a clip: one is under the playhead, and the hint and
/// its button are gone.
fn preview_shows_a_clip(app: &mut TestApp<crate::VideoEditor>) -> bool {
    app.with_surface::<VideoEditorApp, _>(|editor| {
        editor.project.top_video_clip_at(editor.playhead).is_some()
            && editor.go_to_clip_button(&editor.regions()).is_none()
    })
    .expect("the editor surface")
}

/// The preview's hint says to move the playhead over a clip, so it carries
/// Go to clip, which lands the playhead on the nearest clip, by pointer and
/// through its semantic action, compact and wide; the published action
/// does the same.
#[test]
fn go_to_clip_lands_the_playhead_on_a_clip_the_preview_shows() {
    for (width, semantic) in [(420, false), (420, true), (1280, false), (1280, true)] {
        let mut app = parked_between_clips(width);
        assert!(
            !preview_shows_a_clip(&mut app),
            "{width} px: parked past the clips"
        );
        let drawn = app
            .with_surface::<VideoEditorApp, _>(|editor| editor.go_to_clip_button(&editor.regions()))
            .expect("the editor surface")
            .unwrap_or_else(|| panic!("{width} px: the hint carries Go to clip"));
        let (node, bounds) = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find_map(|(id, node)| {
                let b = node.bounds()?;
                let b = Rect::new(b.x0, b.y0, b.x1, b.y1);
                (node.role() == ui::accesskit::Role::Button
                    && node.label() == Some("Go to clip")
                    && (b.center() - drawn.center()).hypot() < 1.0)
                    .then_some((id, b))
            })
            .unwrap_or_else(|| panic!("{width} px: Go to clip is in the tree where it is drawn"));
        if semantic {
            app.harness()
                .perform_a11y_action(node, &ui::a11y::Action::Activate)
                .expect("Go to clip answers its action");
        } else {
            app.harness().click_at(bounds.center().x, bounds.center().y);
        }
        app.settle();
        assert!(
            preview_shows_a_clip(&mut app),
            "{width} px, semantic {semantic}: the playhead is on a clip the preview shows"
        );
    }

    let mut app = parked_between_clips(1280);
    app.effect(&VideoEditorEffect::GoToClip)
        .expect("the published action runs");
    app.settle();
    assert!(
        preview_shows_a_clip(&mut app),
        "the published action lands it too"
    );
}

/// Go to clip takes the next clip after the playhead, and the previous one
/// only where none follows.
#[test]
fn go_to_clip_takes_the_next_clip_then_the_previous() {
    let mut app = sample(1280.0);
    for clip in &mut app.project.clips {
        clip.start += 5.0;
    }
    let first = app
        .project
        .clips
        .iter()
        .filter(|clip| {
            app.project
                .track(clip.track)
                .is_some_and(|track| track.kind == crate::model::TrackKind::Video)
        })
        .map(|clip| clip.start)
        .fold(f64::INFINITY, f64::min);
    app.playhead = 1.0;
    assert_eq!(app.go_to_clip_target(), Some(first), "the next clip after");
    app.playhead = app.project.duration() + 1.0;
    let last = app
        .project
        .clips
        .iter()
        .filter(|clip| {
            app.project
                .track(clip.track)
                .is_some_and(|track| track.kind == crate::model::TrackKind::Video)
        })
        .map(|clip| clip.start)
        .fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(app.go_to_clip_target(), Some(last), "else the previous one");
}

/// A strip too narrow to draw Undo and Redo does not take them away: the
/// bar's overflow menu carries them, named, disabled exactly when the strip's
/// own pair would have been. A capability is never lost to a width.
#[test]
fn a_strip_too_narrow_for_undo_offers_it_in_the_bar() {
    // Wide enough for the transport's own pair: the bar does not repeat them.
    let wide = sample(1280.0);
    let r = wide.regions();
    assert!(
        !wide.history_buttons(&r).is_empty(),
        "a wide strip draws its own Undo and Redo"
    );
    assert!(
        wide.bar_entry("Undo").is_none() && wide.bar_entry("Redo").is_none(),
        "the bar does not offer a second Undo beside the strip's"
    );

    // Narrower than the strip needs: the pair moves into the bar's menu.
    let narrow = [320.0_f64, 360.0, 400.0, 420.0, 480.0, 560.0]
        .into_iter()
        .map(sample)
        .find(|app| app.history_buttons(&app.regions()).is_empty())
        .expect("a narrow strip gives the buttons up before the playhead time");
    let undo = bar_action(&narrow, "Undo");
    let redo = bar_action(&narrow, "Redo");
    assert!(
        undo.menu && redo.menu,
        "menu-only, so the bar's own row does not change with a resize"
    );
    assert_eq!(
        undo.enabled,
        narrow.undo.can(ui::UndoRequest::Undo),
        "offered exactly when the strip's pair would act"
    );

    // And they act: the bar's press asks the host for the step, exactly as
    // the strip's own button does (and, like it, raises nothing while the
    // host says there is nothing to undo).
    let mut narrow = narrow;
    narrow.press_bar("Undo");
    assert_eq!(
        narrow.undo.take_request(),
        None,
        "an unavailable step raises no request, from the bar as from the strip"
    );
    ui::undo::set_undo_status(&mut narrow, &mut || ui::UndoStatus {
        can_undo: true,
        undo_summary: Some("2 fields".into()),
        ..Default::default()
    });
    assert!(
        bar_action(&narrow, "Undo").enabled,
        "the bar's Undo is live when the host says it can undo"
    );
    narrow.press_bar("Undo");
    assert_eq!(
        narrow.undo.take_request(),
        Some(ui::UndoRequest::Undo),
        "one press, one request"
    );
}
