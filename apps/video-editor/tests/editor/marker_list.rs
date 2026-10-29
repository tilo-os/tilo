//! The inspector's marker list: markers are added, named, stepped through,
//! visited and deleted from one place, whether they sit on the timeline or
//! travel with a clip, and whether or not anything is selected.

use crate::hosted;

use hosted::*;
use ui::accesskit::Role;
use video_editor::Project;

/// The marker rows a reader is offered, as `(name, value)`.
fn marker_rows(app: &mut Editor) -> Vec<(String, String)> {
    nodes(app, |n| {
        n.role() == Role::ListItem && n.description() == Some("Activate to move the playhead here")
    })
    .into_iter()
    .map(|(_, n)| {
        (
            n.label().unwrap_or_default().to_string(),
            n.value().unwrap_or_default().to_string(),
        )
    })
    .collect()
}

/// Add a marker at `timecode` and type its name, as M then typing does.
fn mark(app: &mut Editor, timecode: &str, name: &str) {
    go_to(app, timecode);
    app.harness().key("m");
    app.harness().type_text(name);
    app.harness().key("Enter");
}

#[test]
fn m_adds_a_named_marker_at_the_playhead() {
    let mut app = sample_editor();
    mark(&mut app, "0:08", "act two");
    let markers = app.state().markers;
    assert_eq!(markers.len(), 1);
    let marker = markers.iter().next().unwrap();
    assert_eq!(marker.name, "act two", "M opens the new marker's name");
    assert!((marker.t - 8.0).abs() < 1e-6);
    assert_eq!(marker_rows(&mut app).len(), 1, "and the list shows it");
}

#[test]
fn comma_and_period_step_between_markers_and_stop_at_the_ends() {
    let mut app = sample_editor();
    mark(&mut app, "0:08", "act two");
    mark(&mut app, "0:20", "sunset");
    go_to(&mut app, "0:14");
    app.harness().key(",");
    assert_eq!(playhead(&mut app), "00:08:00", "stepped back to the first");
    app.harness().key(".");
    assert_eq!(playhead(&mut app), "00:20:00", "and forward to the second");
    app.harness().key(".");
    assert_eq!(playhead(&mut app), "00:20:00", "stops rather than wrapping");
}

#[test]
fn a_row_takes_the_playhead_there_and_its_delete_removes_only_it() {
    let mut app = sample_editor();
    mark(&mut app, "0:08", "act two");
    mark(&mut app, "0:20", "sunset");
    press(&mut app, Role::ListItem, "act two");
    assert_eq!(playhead(&mut app), "00:08:00");
    press(&mut app, Role::Button, "Delete act two");
    let markers = app.state().markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(markers.iter().next().unwrap().name, "sunset");
}

#[test]
fn a_clip_marker_is_listed_with_the_timeline_ones_and_deletes_from_its_clip() {
    let mut project = video_editor::seed::sample_project();
    let cutaway = clip_of(&project, "Cutaway_Market.mp4");
    let start = project.clip(cutaway).unwrap().start;
    project
        .clip_mut(cutaway)
        .unwrap()
        .add_marker(start + 1.0, "beat", 2)
        .unwrap();
    let mut app = editor_with(&project);
    mark(&mut app, "0:04", "intro");

    let rows = marker_rows(&mut app);
    assert_eq!(rows.len(), 2, "both kinds are listed: {rows:?}");
    // Stored clip-local, the clip's marker still says where it is on the
    // timeline.
    press(&mut app, Role::ListItem, "beat");
    assert_eq!(playhead(&mut app), "00:21:00");

    press(&mut app, Role::Button, "Delete beat");
    let project = app.state();
    assert!(project.clip(cutaway).unwrap().markers.is_empty());
    assert_eq!(project.markers.len(), 1, "the timeline marker is untouched");
}

#[test]
fn markers_are_listed_and_added_with_nothing_selected_even_in_an_empty_project() {
    let mut app = editor_with(&Project::new("Blank"));
    assert!(
        node(
            &mut app,
            Role::Label,
            "No clip selected. Click a clip in the timeline to edit it."
        )
        .is_some(),
        "the empty state is still spoken"
    );
    press(&mut app, Role::Button, "Add marker at playhead");
    assert_eq!(app.state().markers.len(), 1, "the button adds one");
    assert_eq!(marker_rows(&mut app).len(), 1, "and the list shows it");
}
