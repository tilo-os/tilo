#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The editor hosted by the app framework: its custom timeline keeps its own
//! rendering and input, and every edit it makes takes the host's path —
//! permission-checked, one undo step, attributed, replayable.

use ui::prelude::*;
use video_editor::actions::{VideoEditorAction, load_project_action};
use video_editor::doc::sample_project;
use video_editor::{VideoEditor, VideoProject};

fn with_sample() -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    // The demo project goes in through the generic typed `apply` action — the
    // same door a script or an agent would use to submit a whole edit.
    // Replacing the whole project is destructive, so it is confirmed.
    let invoke = Invoke::<VideoProject>::apply(&load_project_action(&sample_project())).unwrap();
    app.invoke_confirmed(invoke).unwrap();
    app
}

#[test]
fn a_typed_edit_through_the_door_reaches_the_timeline() {
    let app = with_sample();
    let stored = app.state();
    assert_eq!(stored.clips.len(), sample_project().clips.len());
    assert!(!stored.clips.is_empty());
}

fn walk(resource: &tilo_actions::Resource, ids: &mut Vec<String>) {
    ids.push(resource.id.0.clone());
    for child in &resource.children {
        walk(child, ids);
    }
}

#[test]
fn the_surface_keeps_publishing_its_own_resource_tree() {
    let mut app = with_sample();
    let resources = app.harness().resources();
    let mut ids = Vec::new();
    for resource in &resources {
        walk(resource, &mut ids);
    }
    assert!(ids.iter().any(|id| id == "video_editor.project"), "{ids:?}");
    assert!(
        ids.iter().any(|id| id.starts_with("video.clip:")),
        "{ids:?}"
    );
}

#[test]
fn a_keyboard_edit_on_the_custom_surface_is_one_undoable_host_action() {
    let mut app = with_sample();
    let markers = app.state().markers.len();
    app.harness().click_named("video_editor_root").unwrap();
    app.harness().key("m");
    assert_eq!(app.state().markers.len(), markers + 1);
    let last = app.versions().pop().unwrap();
    assert!(
        last.summary()
            .is_some_and(|summary| summary.contains("Project")),
        "{last:?}"
    );
    assert!(app.undo());
    assert_eq!(app.state().markers.len(), markers);
    assert!(app.redo());
    assert_eq!(app.state().markers.len(), markers + 1);
}

#[test]
fn named_declarations_are_offered_to_the_palette_and_agents() {
    let mut app = with_sample();
    app.invoke(VideoProject::ADD_MARKER.with((12.5, "Chorus".into())))
        .unwrap();
    assert!(
        app.state()
            .markers
            .iter()
            .any(|marker| marker.name == "Chorus")
    );
    let refused = app.invoke(VideoProject::ADD_MARKER.with((-1.0, "Before time".into())));
    assert!(refused.is_err());
}

#[test]
fn a_viewer_sees_the_timeline_but_cannot_change_it() {
    let mut app = HeadlessApp::<VideoEditor>::with_role(ui::Role::Viewer).unwrap();
    let invoke = Invoke::<VideoProject>::apply(&load_project_action(&sample_project())).unwrap();
    assert!(app.invoke_confirmed(invoke).is_err());
    app.harness().click_named("video_editor_root").unwrap();
    app.harness().key("m");
    assert!(app.state().markers.is_empty());
}

#[test]
fn existing_actions_replay_byte_identically_through_the_resource_contract() {
    let mut origin = Replica::<VideoProject>::new();
    let mut peer = origin.fork();
    origin
        .apply_action(&load_project_action(&sample_project()), "Open project")
        .unwrap();
    let first = origin.state().unwrap().clips.first().unwrap().id;
    origin
        .apply_action(
            &VideoEditorAction::DeleteClip {
                id: first.to_string(),
            },
            "Delete clip",
        )
        .unwrap();
    for envelope in origin.log().to_vec() {
        peer.replay(&envelope).unwrap();
    }
    assert_eq!(origin.heads(), peer.heads());
    assert_eq!(
        peer.state().unwrap().clips.len(),
        sample_project().clips.len() - 1
    );
}

fn typed(action: &VideoEditorAction) -> Invoke<VideoProject> {
    Invoke::<VideoProject>::apply(action).unwrap()
}

#[test]
fn deleting_a_clip_is_confirmed_and_trimming_one_is_not() {
    let mut app = with_sample();
    let clip = app.state().clips.first().cloned().unwrap();
    let clips = app.state().clips.len();

    // A trim — any edit that keeps the work — applies directly.
    let mut trimmed = clip.clone();
    trimmed.out_point = trimmed.in_point + (trimmed.out_point - trimmed.in_point) / 2.0;
    app.invoke(typed(&video_editor::actions::upsert_clip_action(&trimmed)))
        .unwrap();
    let stored = app.state().clip(clip.id).cloned().unwrap();
    assert!((stored.out_point - trimmed.out_point).abs() < 1e-9);

    // Deleting it is destructive: refused until confirmed, like a declared
    // destructive action.
    let delete = typed(&video_editor::actions::delete_clip_action(&clip.id));
    assert!(matches!(
        app.invoke(delete.clone()),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    assert_eq!(app.state().clips.len(), clips, "nothing was deleted");
    app.invoke_confirmed(delete).unwrap();
    assert_eq!(app.state().clips.len(), clips - 1);
}

#[test]
fn a_delete_key_on_the_surface_waits_for_confirmation() {
    let mut app = with_sample();
    let clips = app.state().clips.len();
    app.harness().click_named("video_editor_root").unwrap();
    app.harness().key("Delete");
    assert_eq!(
        app.state().clips.len(),
        clips,
        "the surface's delete is held for the host's confirmation"
    );
}
