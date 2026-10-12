#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The editor hosted by the app framework: its custom timeline keeps its own
//! rendering and input, and every edit it makes takes the host's path —
//! permission-checked, one undo step, attributed, replayable.

use ui::prelude::*;
use ui::tilo_resource::Id;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

fn with_sample() -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    // The demo project goes in as one field edit of the whole project — the
    // same door a script or an agent would use to submit a whole edit.
    app.invoke_confirmed(Invoke::edit(&load_project(&sample_project())).unwrap())
        .unwrap();
    app
}

#[test]
fn host_door_edits_publish_resources_and_undo_as_one_action() {
    // Case `a_typed_edit_through_the_door_reaches_the_timeline`.
    {
        let app = with_sample();
        let stored = app.state();
        assert_eq!(stored.clips.len(), sample_project().clips.len());
        assert!(!stored.clips.is_empty());
    }
    // Case `the_surface_keeps_publishing_its_own_resource_tree`.
    {
        let mut app = with_sample();
        let resources = app.harness().resources();
        let mut ids = Vec::new();
        for resource in &resources {
            walk(resource, &mut ids);
        }
        assert!(ids.iter().any(|id| id == "video-editor.project"), "{ids:?}");
        assert!(
            ids.iter()
                .any(|id| id.starts_with("video-editor.project/clips/")),
            "{ids:?}"
        );
    }
    // Case `a_keyboard_edit_on_the_custom_surface_is_one_undoable_host_action`.
    {
        let mut app = with_sample();
        let markers = app.state().markers.len();
        app.harness().click_named("video_editor_root").unwrap();
        app.harness().key("m");
        assert_eq!(app.state().markers.len(), markers + 1);
        let last = app.versions().pop().unwrap();
        assert!(
            last.context
                .as_ref()
                .and_then(|context| context.what.as_ref())
                .is_some_and(|what| matches!(what, ui::tilo_actions::Did::Edit(_))),
            "{last:?}"
        );
        assert!(app.undo());
        assert_eq!(app.state().markers.len(), markers);
        assert!(app.redo());
        assert_eq!(app.state().markers.len(), markers + 1);
    }
    // Case `a_gesture_of_several_edits_is_one_undo_step`.
    {
        let mut app = with_sample();
        let clips = app.state().clips.len();
        assert!(clips > 1, "the sample has several clips to paste");
        let versions = app.versions().len();
        app.harness().click_named("video_editor_root").unwrap();
        // Select everything, copy it and paste it at the playhead: one new clip
        // per copied clip, one gesture.
        app.harness().set_modifiers(&["ctrl".into()]);
        for key in ["a", "c", "v"] {
            app.harness().key(key);
        }
        app.harness().set_modifiers(&[]);
        assert_eq!(app.state().clips.len(), clips * 2);
        assert_eq!(
            app.versions().len(),
            versions + 1,
            "the whole paste is one action"
        );
        assert!(app.undo());
        assert_eq!(app.state().clips.len(), clips, "one undo takes it all back");
        assert!(app.redo());
        assert_eq!(app.state().clips.len(), clips * 2);
    }
}

fn walk(resource: &tilo_actions::Resource, ids: &mut Vec<String>) {
    ids.push(resource.id.0.clone());
    for child in &resource.children {
        walk(child, ids);
    }
}

#[test]
fn host_declarations_roles_and_replay_are_honoured() {
    // Case `named_declarations_are_offered_to_the_palette_and_agents`.
    {
        let mut app = with_sample();
        app.invoke(Project::PLACE_MARKER.with((12.5, "Chorus".into())))
            .unwrap();
        assert!(
            app.state()
                .markers
                .iter()
                .any(|marker| marker.name == "Chorus")
        );
        let refused = app.invoke(Project::PLACE_MARKER.with((-1.0, "Before time".into())));
        assert!(refused.is_err());
    }
    // Case `a_viewer_sees_the_timeline_but_cannot_change_it`.
    {
        let mut app = HeadlessApp::<VideoEditor>::with_role(ui::Role::Viewer).unwrap();
        let invoke = Invoke::edit(&load_project(&sample_project())).unwrap();
        assert!(app.invoke_confirmed(invoke).is_err());
        app.harness().click_named("video_editor_root").unwrap();
        app.harness().key("m");
        assert!(app.state().markers.is_empty());
    }
    // Case `existing_actions_replay_byte_identically_through_the_resource_contract`.
    {
        let mut origin = Replica::<Project>::new();
        let mut peer = origin.fork();
        origin
            .apply(Invoke::edit(&load_project(&sample_project())).unwrap())
            .unwrap();
        // A clip with no linked clips, so the delete takes exactly one.
        let state = origin.state().unwrap();
        let lone = state
            .clips
            .iter()
            .find(|clip| state.link_partners(clip.id).is_empty())
            .unwrap()
            .id;
        origin
            .apply_confirmed(Project::REMOVE_CLIP.with(Id::from_raw(lone.to_string())))
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
}

#[test]
fn destructive_host_edits_wait_for_one_confirmation() {
    // Case `deleting_a_clip_is_confirmed_and_trimming_one_is_not`.
    {
        let mut app = with_sample();
        let clip = app.state().clips.first().cloned().unwrap();
        let clips = app.state().clips.len();

        // A trim — any edit that keeps the work — applies directly.
        let mut trimmed = clip.clone();
        trimmed.out_point = trimmed.in_point + (trimmed.out_point - trimmed.in_point) / 2.0;
        let clip_key = Id::from_raw(clip.id.to_string());
        app.invoke(
            Invoke::edit(
                &Project::paths()
                    .clips
                    .item(&clip_key)
                    .out_point
                    .set(trimmed.out_point),
            )
            .unwrap(),
        )
        .unwrap();
        let stored = app.state().clip(clip.id).cloned().unwrap();
        assert!((stored.out_point - trimmed.out_point).abs() < 1e-9);

        // Deleting it is destructive: refused until confirmed, like a declared
        // destructive action.
        let delete = Project::REMOVE_CLIP.with(clip_key);
        assert!(matches!(
            app.invoke(delete.clone()),
            Err(tilo_actions::DoorError::ConfirmationRequired)
        ));
        assert_eq!(app.state().clips.len(), clips, "nothing was deleted");
        let partners = app.state().link_partners(clip.id).len();
        app.invoke_confirmed(delete).unwrap();
        assert_eq!(
            app.state().clips.len(),
            clips - 1 - partners,
            "the clip goes with the clips linked to it"
        );
    }
    // Case `a_delete_key_on_the_surface_waits_for_confirmation`.
    {
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
    // Case `a_destructive_gesture_asks_once_and_changes_nothing_until_confirmed`.
    {
        let mut app = with_sample();
        let clips = app.state().clips.len();
        app.harness().click_named("video_editor_root").unwrap();
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("a");
        app.harness().set_modifiers(&[]);
        // One deletion per clip, submitted as one destructive group: nothing is
        // deleted before the host's confirmation, not even the first clip.
        app.harness().key("Delete");
        assert_eq!(app.state().clips.len(), clips);
    }
}

#[test]
fn edits_bound_to_the_playhead_reach_the_door() {
    let mut app = with_sample();
    let clip = app
        .state()
        .clips
        .iter()
        .max_by(|a, b| a.duration().total_cmp(&b.duration()))
        .cloned()
        .expect("the sample has clips");
    let at = clip.start + clip.duration() / 2.0;
    let clips = app.state().clips.len();

    // An agent names the clip and the instant; the editor's view supplies
    // the rest of the project the cut is made in.
    app.call(
        "split-clip",
        serde_json::json!({ "clip": clip.id.to_string(), "at": at }),
    )
    .expect("the split is offered on the project");
    assert!(app.state().clips.len() > clips, "the clip was cut");
    assert!(app.undo(), "the cut is one undo step");
    assert_eq!(app.state().clips.len(), clips);

    // A mark sets the render range and nothing else.
    let name = app.state().name;
    app.call("mark-in", serde_json::json!({ "at": at }))
        .expect("marking is offered on the project");
    let marked = app.state();
    assert_eq!(marked.mark_in, Some(at));
    assert_eq!(marked.name, name);

    // A time the clip does not cover is refused, not applied somewhere else.
    assert!(
        app.call(
            "split-clip",
            serde_json::json!({ "clip": clip.id.to_string(), "at": clip.end() + 1000.0 }),
        )
        .is_err()
    );
}
