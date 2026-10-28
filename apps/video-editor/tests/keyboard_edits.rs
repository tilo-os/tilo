#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Edit shortcuts, driven through the public surface: pick a clip on the
//! timeline as a screen reader does, move the playhead with the go-to field,
//! press the key, and read the project the host holds afterwards.

use ui::prelude::*;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

/// The demo project: on V1, `Drone_Coastline.mov` over 0–8 s,
/// `Interview_A.mp4` over 8–20 s and `Cutaway_Market.mp4` over 20–26 s.
fn editor() -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&sample_project())).unwrap())
        .unwrap();
    app
}

/// The id of the clip placing the source `name`.
fn clip_of(project: &Project, name: &str) -> uuid::Uuid {
    let media = project.media.iter().find(|m| m.name == name).unwrap().id;
    project.clips.iter().find(|c| c.media == media).unwrap().id
}

/// Select the clip placing `name` alone, by activating its node on the
/// timeline (a bin row of the same source is not a placed clip).
fn select(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    app.harness().click_named("video_editor_root").unwrap();
    let clip = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::ListItem
                && node.label() == Some(name)
                && node.value().is_some_and(|value| value.contains(" on V"))
        })
        .map(|(id, _)| id)
        .expect("the source is placed on a video track");
    app.harness()
        .perform_a11y_action(clip, &ui::a11y::Action::Activate)
        .unwrap();
}

/// Move the playhead to `timecode` through the go-to field.
fn go_to(app: &mut HeadlessApp<VideoEditor>, timecode: &str) {
    app.harness().key("g");
    app.harness().type_text(timecode);
    app.harness().key("Enter");
}

fn with_modifier(app: &mut HeadlessApp<VideoEditor>, modifier: &str, key: &str) {
    app.harness().set_modifiers(&[modifier.into()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
}

#[test]
fn s_splits_the_selected_clip_at_the_playhead_as_one_undo_step() {
    let mut app = editor();
    let clips = app.state().clips.len();
    select(&mut app, "Interview_A.mp4");
    go_to(&mut app, "0:12");
    let versions = app.versions().len();
    app.harness().key("s");
    assert_eq!(app.state().clips.len(), clips + 1);
    assert_eq!(app.versions().len(), versions + 1, "one action");
    assert!(app.undo());
    assert_eq!(app.state().clips.len(), clips);
}

#[test]
fn copy_and_paste_place_an_independent_clip_at_the_playhead() {
    let mut app = editor();
    let clips = app.state().clips.len();
    let drone = app
        .state()
        .media
        .iter()
        .find(|m| m.name == "Drone_Coastline.mov")
        .unwrap()
        .id;
    select(&mut app, "Drone_Coastline.mov");
    with_modifier(&mut app, "ctrl", "c");
    go_to(&mut app, "0:30");
    with_modifier(&mut app, "ctrl", "v");
    let project = app.state();
    assert_eq!(project.clips.len(), clips + 1);
    assert!(
        project
            .clips
            .iter()
            .any(|c| c.media == drone && (c.start - 30.0).abs() < 1e-9),
        "a second drone clip starts at the playhead"
    );
}

#[test]
fn o_trims_the_selected_clip_out_to_the_playhead() {
    let mut app = editor();
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    select(&mut app, "Interview_A.mp4");
    go_to(&mut app, "0:15");
    app.harness().key("o");
    let end = app.state().clip(interview).unwrap().end();
    assert!((end - 15.0).abs() < 1e-6, "ends at {end}");
}

#[test]
fn shift_delete_ripples_the_gap_closed() {
    let mut app = editor();
    let drone = clip_of(&app.state(), "Drone_Coastline.mov");
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let before = app.state().clip(interview).unwrap().start;
    select(&mut app, "Drone_Coastline.mov");
    with_modifier(&mut app, "shift", "Delete");
    let project = app.state();
    assert!(project.clip(drone).is_none(), "the clip is gone");
    assert!(
        project.clip(interview).unwrap().start < before,
        "the next clip moved left into the gap"
    );
}

#[test]
fn shift_right_nudges_the_selected_clip_a_second_later() {
    let mut app = editor();
    let interview = clip_of(&app.state(), "Interview_A.mp4");
    let before = app.state().clip(interview).unwrap().start;
    select(&mut app, "Interview_A.mp4");
    with_modifier(&mut app, "shift", "ArrowRight");
    let after = app.state().clip(interview).unwrap().start;
    assert!((after - before - 1.0).abs() < 1e-6, "{before} -> {after}");
}
