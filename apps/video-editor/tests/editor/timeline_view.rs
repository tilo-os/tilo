//! The timeline's view and its selection edits, through the hosted editor:
//! wheel, keys and the reader's nodes in, the timeline's spoken view and the
//! project the host holds out.

use ui::accesskit::{Node, NodeId, Role};
use ui::prelude::*;
use video_editor::model::Marker;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

/// The demo project: V1 holds the drone (0–8 s), the interview (8–20 s) and
/// the market cutaway (20–26 s); V2 the logo and the time-lapse; A1 and A2
/// one sound each.
fn editor() -> HeadlessApp<VideoEditor> {
    hosting(&sample_project())
}

fn hosting(project: &Project) -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app
}

fn find(
    app: &mut HeadlessApp<VideoEditor>,
    pick: impl Fn(&Node) -> bool,
) -> Option<(NodeId, Node)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| pick(node))
}

fn press(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let (node, _) =
        find(app, |n| n.label() == Some(name)).unwrap_or_else(|| panic!("no {name} in the tree"));
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

/// Select the clip placing `name` alone.
fn select(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let (clip, _) = find(app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some(name)
            && n.value().is_some_and(|v| v.contains(" on "))
    })
    .expect("the source is placed");
    app.harness()
        .perform_a11y_action(clip, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

fn keys(app: &mut HeadlessApp<VideoEditor>, modifiers: &[&str], key: &str) {
    let modifiers: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
    app.harness().set_modifiers(&modifiers);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

/// What the timeline tells a reader: the playhead, the selection and where
/// its view starts.
fn timeline(app: &mut HeadlessApp<VideoEditor>) -> String {
    find(app, |n| {
        n.role() == Role::Group && n.label() == Some("Timeline")
    })
    .and_then(|(_, n)| n.value().map(str::to_owned))
    .unwrap()
}

fn view_start(app: &mut HeadlessApp<VideoEditor>) -> String {
    let said = timeline(app);
    said.split("timeline view starts at ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .unwrap()
        .to_owned()
}

fn playhead(app: &mut HeadlessApp<VideoEditor>) -> String {
    let said = timeline(app);
    said.split("playhead at ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap()
        .to_owned()
}

/// Put the pointer over the preview (`over_preview`) or the timeline: the
/// two sides of the splitter between them.
fn point_at(app: &mut HeadlessApp<VideoEditor>, over_preview: bool) {
    let (_, splitter) = find(app, |n| n.role() == Role::Splitter).unwrap();
    let bounds = splitter.bounds().unwrap();
    let y = if over_preview {
        bounds.y0 / 2.0
    } else {
        bounds.y1 + 120.0
    };
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(bounds.x1 / 2.0, y));
}

fn wheel(app: &mut HeadlessApp<VideoEditor>, modifiers: &[&str], dx: f64, dy: f64, times: usize) {
    let modifiers: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
    app.harness().set_modifiers(&modifiers);
    for _ in 0..times {
        app.harness().scroll(dx, dy);
    }
    app.harness().set_modifiers(&[]);
    app.settle();
}

/// Page Up / Page Down, which the harness's key names do not cover.
fn page(app: &mut HeadlessApp<VideoEditor>, down: bool) {
    use winit::keyboard::{Key, NamedKey};
    let key = Key::Named(if down {
        NamedKey::PageDown
    } else {
        NamedKey::PageUp
    });
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        app.harness().send(&ui::synthetic_input::keyboard_input(
            ui::synthetic_input::key_event(key.clone(), None, state),
        ));
    }
    app.settle();
}

fn zoom_in(app: &mut HeadlessApp<VideoEditor>) {
    for _ in 0..8 {
        keys(app, &[], "+");
    }
}

fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

#[test]
fn shift_wheel_pans_a_zoomed_timeline_and_stops_at_both_ends() {
    let mut app = editor();
    zoom_in(&mut app);
    point_at(&mut app, false);
    assert_eq!(view_start(&mut app), "00:00:00");
    wheel(&mut app, &["shift"], 0.0, -3.0, 1);
    let panned = view_start(&mut app);
    assert_ne!(panned, "00:00:00", "shift+wheel panned right");
    wheel(&mut app, &["shift"], 0.0, -3.0, 400);
    let end = view_start(&mut app);
    wheel(&mut app, &["shift"], 0.0, -3.0, 5);
    assert_eq!(view_start(&mut app), end, "it stops at the content's end");
    wheel(&mut app, &["shift"], 0.0, 3.0, 400);
    assert_eq!(view_start(&mut app), "00:00:00", "and at its start");
}

#[test]
fn a_horizontal_wheel_pans_and_a_wheel_over_the_preview_leaves_the_timeline() {
    let mut app = editor();
    zoom_in(&mut app);
    point_at(&mut app, true);
    wheel(&mut app, &[], 0.0, -3.0, 3);
    wheel(&mut app, &["ctrl"], 0.0, 3.0, 3);
    assert_eq!(
        view_start(&mut app),
        "00:00:00",
        "the preview is not a timeline"
    );
    point_at(&mut app, false);
    wheel(&mut app, &[], -3.0, 0.0, 1);
    assert_ne!(
        view_start(&mut app),
        "00:00:00",
        "a horizontal wheel panned"
    );
}

#[test]
fn page_keys_scroll_a_zoomed_timeline_and_z_fits_the_project_back() {
    let mut app = editor();
    zoom_in(&mut app);
    assert!(timeline(&mut app).contains("Page Up and Page Down"));
    page(&mut app, true);
    assert_ne!(view_start(&mut app), "00:00:00", "Page Down pans later");
    page(&mut app, false);
    page(&mut app, true);
    keys(&mut app, &[], "z");
    assert_eq!(view_start(&mut app), "00:00:00", "Z fits from the start");
}

#[test]
fn ctrl_wheel_zooms_so_the_timeline_overflows() {
    let mut app = editor();
    point_at(&mut app, false);
    wheel(&mut app, &["ctrl"], 0.0, 3.0, 12);
    page(&mut app, true);
    assert_ne!(
        view_start(&mut app),
        "00:00:00",
        "zoomed in, the tail is off screen"
    );
}

#[test]
fn the_view_follows_the_playhead_off_its_edge() {
    let mut app = editor();
    zoom_in(&mut app);
    press(&mut app, "Jump to end");
    assert_ne!(
        view_start(&mut app),
        "00:00:00",
        "the view went with the playhead"
    );
}

#[test]
fn jkl_shuttles_and_k_stops() {
    let mut app = editor();
    keys(&mut app, &[], "l");
    assert!(
        find(&mut app, |n| n.label() == Some("Pause")).is_some(),
        "L plays forward"
    );
    keys(&mut app, &[], "k");
    assert!(
        find(&mut app, |n| n.label() == Some("Play")).is_some(),
        "K stops"
    );
    keys(&mut app, &[], "j");
    assert!(
        find(&mut app, |n| n.label() == Some("Pause")).is_some(),
        "J plays in reverse"
    );
}

#[test]
fn a_locked_track_refuses_split_delete_and_nudge_but_stays_selectable() {
    let mut app = editor();
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let v1 = app.state().clip(interview).unwrap().track;
    press(&mut app, "Lock V1");
    assert!(app.state().tracks.iter().any(|t| t.id == v1 && t.locked));
    select(&mut app, "Interview_A.mp4");
    assert!(timeline(&mut app).contains("1 of 7 clips selected"));
    let before = app.state();
    keys(&mut app, &[], "s");
    keys(&mut app, &[], "Delete");
    keys(&mut app, &["shift"], "ArrowRight");
    let after = app.state();
    assert_eq!(after.clips.len(), before.clips.len());
    assert!(
        (after.clip(interview).unwrap().start - before.clip(interview).unwrap().start).abs()
            < 1e-12,
        "the clip did not move"
    );
}

#[test]
fn pasting_onto_a_locked_track_lands_on_a_video_track_that_accepts_it() {
    let mut app = editor();
    let drone = clip_of(&app.state(), "Drone_Coastline.mov");
    let v1 = app.state().clip(drone).unwrap().track;
    select(&mut app, "Drone_Coastline.mov");
    keys(&mut app, &["ctrl"], "c");
    press(&mut app, "Lock V1");
    keys(&mut app, &["ctrl"], "v");
    let project = app.state();
    let media = project.clip(drone).unwrap().media;
    let pasted = project
        .clips
        .iter()
        .find(|c| c.media == media && c.id != drone)
        .expect("something was pasted");
    assert_ne!(pasted.track, v1, "the paste avoided the locked track");
    assert!(
        project
            .tracks
            .iter()
            .any(|t| t.id == pasted.track && t.kind == video_editor::model::TrackKind::Video)
    );
}

#[test]
fn ctrl_x_cuts_the_clip_and_ctrl_v_pastes_it_back_selected() {
    let mut app = editor();
    let drone = clip_of(&app.state(), "Drone_Coastline.mov");
    let media = app.state().clip(drone).unwrap().media;
    let clips = app.state().clips.len();
    select(&mut app, "Drone_Coastline.mov");
    keys(&mut app, &["ctrl"], "x");
    assert!(app.state().clip(drone).is_none());
    assert_eq!(app.state().clips.len(), clips - 1);
    keys(&mut app, &["ctrl"], "v");
    assert_eq!(app.state().clips.len(), clips);
    assert!(app.state().clips.iter().any(|c| c.media == media));
    assert!(timeline(&mut app).contains("1 of 7 clips selected"));
}

#[test]
fn the_whole_selection_deletes_and_duplicates_as_one_step_each() {
    let mut app = editor();
    let clips = app.state().clips.len();
    keys(&mut app, &["ctrl"], "a");
    let versions = app.versions().len();
    keys(&mut app, &["ctrl"], "d");
    assert_eq!(app.state().clips.len(), clips * 2, "every clip duplicated");
    assert_eq!(app.versions().len(), versions + 1, "in one step");
    let selected = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, n)| {
            n.value().is_some_and(|v| v.contains(" on ")) && n.is_selected() == Some(true)
        })
        .count();
    assert_eq!(selected, clips, "the copies are the selection");
    keys(&mut app, &[], "Delete");
    assert_eq!(app.state().clips.len(), clips, "the selection went at once");
    assert_eq!(app.versions().len(), versions + 2, "in one step");
}

/// Open the item `anchor` names, as choosing a search hit or an agent's
/// `open` does.
fn open(
    app: &mut HeadlessApp<VideoEditor>,
    anchor: &str,
) -> Result<(), ui::tilo_actions::DoorError> {
    app.call("open", ui::serde_json::json!({ "anchor": anchor }))?;
    app.settle();
    Ok(())
}

fn clip_anchor(clip: uuid::Uuid) -> String {
    video_editor::search::anchor("clips", clip)
}

#[test]
fn revealing_a_clip_selects_it_puts_the_playhead_on_it_and_stops_playback() {
    let mut app = editor();
    let market = clip_of(&app.state(), "Cutaway_Market.mp4");
    press(&mut app, "Play");
    open(&mut app, &clip_anchor(market)).unwrap();
    app.settle();
    assert_eq!(playhead(&mut app), "00:20:00");
    assert!(
        find(&mut app, |n| n.label() == Some("Play")).is_some(),
        "the seek stopped the transport"
    );
    let (_, node) = find(&mut app, |n| {
        n.label() == Some("Cutaway_Market.mp4") && n.value().is_some_and(|v| v.contains(" on "))
    })
    .unwrap();
    assert_eq!(node.is_selected(), Some(true));
}

#[test]
fn revealing_a_clip_marker_selects_its_clip_at_the_markers_current_place() {
    let mut project = sample_project();
    let market = clip_of(&project, "Cutaway_Market.mp4");
    let beat = Marker::new(2.0, "Exact beat", 1);
    let marker_id = beat.id;
    project.clip_mut(market).unwrap().markers.push(beat);
    let mut app = hosting(&project);
    open(
        &mut app,
        &format!("{}/markers/{marker_id}", clip_anchor(market)),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        playhead(&mut app),
        "00:22:00",
        "the clip's start plus the marker's offset"
    );
    let (_, node) = find(&mut app, |n| {
        n.label() == Some("Cutaway_Market.mp4") && n.value().is_some_and(|v| v.contains(" on "))
    })
    .unwrap();
    assert_eq!(node.is_selected(), Some(true));
}

#[test]
fn revealing_an_effect_that_is_gone_is_refused() {
    let mut project = sample_project();
    let market = clip_of(&project, "Cutaway_Market.mp4");
    let clip = project.clip_mut(market).unwrap();
    clip.add_effect(video_editor::model::EffectKind::Sepia);
    clip.add_effect(video_editor::model::EffectKind::Sepia);
    let effect = clip.effects[1].id;
    let mut app = hosting(&project);
    let row = format!("{}/effects/{effect}", clip_anchor(market));
    open(&mut app, &row).expect("a live effect resolves");
    let mut gone = project.clone();
    gone.clip_mut(market).unwrap().remove_effect_id(effect);
    app.invoke_confirmed(Invoke::edit(&load_project(&gone)).unwrap())
        .unwrap();
    assert!(
        open(&mut app, &row).is_err(),
        "a stale effect is not resolved to its twin by position"
    );
}
