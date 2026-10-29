//! Markers as a reader meets them: each one a row naming when it is and what
//! its note says, carrying the three controls the pointer reveals on hover,
//! and a place the playhead can be sent.

use crate::hosted_editor as hosted;

use hosted::{Editor, act, named, open, playhead, press, select_on, value};
use ui::a11y::Action;
use ui::accesskit::Role;
use video_editor::Project;
use video_editor::seed::sample_project;

/// The demo project with a marker named `name` at each `(t, name)`.
fn marked(marks: &[(f64, &str)]) -> Editor {
    let mut project = sample_project();
    for (t, name) in marks {
        project.add_marker(*t, *name, 0);
    }
    open(&project)
}

fn marker<'p>(project: &'p Project, name: &str) -> &'p video_editor::model::Marker {
    project.markers.iter().find(|m| m.name == name).unwrap()
}

/// The marker rows a reader is offered, in the order they are offered.
fn spoken_markers(app: &mut Editor) -> Vec<String> {
    let group = named(app, "Markers").expect("the markers section");
    let rows = app.harness().a11y_nodes();
    group
        .children()
        .iter()
        .filter_map(|id| rows.iter().find(|(n, _)| n == id))
        .filter(|(_, n)| n.role() == Role::ListItem)
        .map(|(_, n)| n.label().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_marker_row_speaks_its_time_and_note_and_offers_its_controls() {
    let mut app = marked(&[(12.0, "act two")]);
    act(
        &mut app,
        "Edit note on act two",
        &Action::SetValue("check the horizon line".into()),
    )
    .unwrap();
    let row = value(&mut app, "act two");
    assert!(
        row.contains("00:12:00"),
        "the row's timecode is spoken: {row}"
    );
    assert!(
        row.contains("check the horizon line"),
        "and its note: {row}"
    );
    for verb in ["Edit note on", "Rename", "Delete"] {
        let control = named(&mut app, &format!("{verb} act two"))
            .unwrap_or_else(|| panic!("the row offers {verb}"));
        assert_eq!(control.role(), Role::Button);
    }
    let project = app.state();
    let m = marker(&project, "act two");
    assert_eq!(
        m.note, "check the horizon line",
        "the note reached the document"
    );
}

#[test]
fn a_note_can_be_cleared_but_a_name_cannot() {
    let mut app = marked(&[(12.0, "act two")]);
    let note = "Edit note on act two";
    act(&mut app, note, &Action::SetValue("a note".into())).unwrap();
    act(&mut app, note, &Action::SetValue(String::new())).unwrap();
    assert_eq!(
        marker(&app.state(), "act two").note,
        "",
        "an empty note takes it off"
    );

    let _ = act(&mut app, "Rename act two", &Action::SetValue("   ".into()));
    assert_eq!(
        marker(&app.state(), "act two").name,
        "act two",
        "a nameless marker is a dot nobody can find"
    );
    act(
        &mut app,
        "Rename act two",
        &Action::SetValue("act three".into()),
    )
    .unwrap();
    assert!(app.state().markers.iter().any(|m| m.name == "act three"));
}

#[test]
fn delete_takes_the_marker_out_of_the_document_and_the_tree() {
    let mut app = marked(&[(12.0, "act two")]);
    press(&mut app, "Delete act two");
    assert!(app.state().markers.is_empty());
    assert!(named(&mut app, "act two").is_none());
}

#[test]
fn project_and_clip_markers_are_offered_in_time_order_with_the_add_button() {
    let mut project = sample_project();
    project.add_marker(12.0, "act two", 0);
    project.add_marker(4.0, "cold open", 0);
    let sunset = project
        .media
        .iter()
        .find(|m| m.name == "Sunset_Timelapse.mp4")
        .unwrap()
        .id;
    project
        .clips
        .iter_mut()
        .find(|c| c.media == sunset)
        .unwrap()
        .add_marker(23.5, "on the clip", 2);
    let mut app = open(&project);
    select_on(&mut app, "Sunset_Timelapse.mp4", "V2");
    assert_eq!(
        spoken_markers(&mut app),
        ["cold open", "act two", "on the clip"]
    );
    assert!(named(&mut app, "Add marker at playhead").is_some());
}

#[test]
fn activating_a_marker_seeks_there_and_stops_playback() {
    let mut app = marked(&[(12.0, "act two")]);
    press(&mut app, "Play");
    press(&mut app, "act two");
    assert_eq!(playhead(&mut app), "00:12:00");
    assert!(
        named(&mut app, "Play").is_some(),
        "seeking to a mark stops playback, as the click does"
    );
}

#[test]
fn a_held_marker_node_keeps_naming_its_marker_when_the_list_reorders() {
    let mut app = marked(&[(12.0, "act two")]);
    let held = app.harness().a11y_node_named("act two").unwrap();
    // An earlier marker pushes "act two" down the list.
    hosted::go_to(&mut app, "0:04");
    press(&mut app, "Add marker at playhead");
    app.harness().key("Escape");
    assert_eq!(spoken_markers(&mut app).len(), 2);
    assert_ne!(spoken_markers(&mut app)[0], "act two");
    hosted::go_to(&mut app, "0:00");
    app.harness()
        .perform_a11y_action(held, &Action::Activate)
        .unwrap();
    assert_eq!(
        playhead(&mut app),
        "00:12:00",
        "the held node still names act two"
    );
}

#[test]
fn enter_on_a_focused_marker_seeks_there() {
    let mut app = marked(&[(12.0, "act two")]);
    act(&mut app, "act two", &Action::Focus).unwrap();
    let (_, focused) = app.harness().a11y_focus().expect("a focused node");
    assert_eq!(
        focused.label(),
        Some("act two"),
        "the marker is a focus stop"
    );
    app.harness().key("Enter");
    assert_eq!(playhead(&mut app), "00:12:00");
}
