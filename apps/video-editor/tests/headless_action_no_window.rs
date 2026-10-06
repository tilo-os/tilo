#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "headless integration fixture asserts setup failures directly"
)]

//! End-to-end action test without a driving window. A participant with no
//! window — an agent, a script — holds a replica of the project, surveys what
//! it may do, and edits it through the same door the editor's own window
//! uses; the editor shows the result once the change reaches it.

use tilo_actions::door::{ActionAddress, DoorError};
use ui::framework::{Participant, resource_kind};
use ui::permissions::PermissionContext;
use ui::prelude::*;
use video_editor::actions::load_project_action;
use video_editor::model::{MediaItem, MediaKind, Project, Track, TrackKind};
use video_editor::{VideoEditor, VideoProject};

/// A project holding one clip, opened in the editor as its owner would.
fn editor_with_one_clip() -> (TestApp<VideoEditor>, uuid::Uuid) {
    let mut project = Project::new("headless");
    let media_id = uuid::Uuid::new_v4();
    project.media.push(MediaItem {
        id: media_id,
        name: "take-1.mp4".into(),
        kind: MediaKind::Video,
        duration: 20.0,
        width: 1920,
        height: 1080,
        fps: 30.0,
        ..Default::default()
    });
    let track_id = uuid::Uuid::new_v4();
    project.tracks.push(Track {
        id: track_id,
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        ..Default::default()
    });
    let clip_id = project
        .add_clip(track_id, media_id, 0.0)
        .expect("add a clip");
    project.clip_mut(clip_id).expect("the clip").out_point = 20.0;

    let mut app = TestApp::<VideoEditor>::new().expect("the editor opens");
    let load = Invoke::<VideoProject>::apply(&load_project_action(&project)).unwrap();
    app.invoke_confirmed(load).expect("the project loads");
    (app, clip_id)
}

/// An action reaches the document with no window, and the app itself shows
/// the result.
#[test]
fn an_action_invoked_with_no_window_reaches_the_app() {
    let (mut app, clip_id) = editor_with_one_clip();

    // A replica of the record, as a participant joining it receives one.
    let snapshot = app.peer_document().save();
    let mut agent = Participant::from_snapshot(
        resource_kind::<VideoEditor>(),
        &snapshot,
        "headless-driver",
        PermissionContext::permissive(),
    )
    .expect("the replica holds the project");

    // Surveyed, not guessed. Asserting the address is offered *before*
    // invoking it is what keeps the next assertion honest: invoking
    // something the tree never advertised would prove nothing about the
    // surface an agent actually sees.
    let operations = agent.operations();
    let address = operations
        .iter()
        .map(|operation| operation.address.clone())
        .find(|address| {
            address.resource == format!("video.clip:{clip_id}") && address.action == "set-fade-in"
        })
        .unwrap_or_else(|| {
            panic!(
                "no `set-fade-in` offered on the clip; the tree offered: {:?}",
                operations
                    .iter()
                    .map(|operation| (&operation.address.resource, &operation.address.action))
                    .collect::<Vec<_>>()
            )
        });

    let mut arguments = serde_json::Map::new();
    arguments.insert("seconds".into(), serde_json::json!(1.5));
    let changes = agent
        .invoke(&address, &arguments)
        .expect("the surveyed action applies");
    assert!(
        changes.is_some(),
        "the action reported success without changing the document",
    );

    // A negative control in the same run: the door must refuse an address
    // the tree does not carry. Without this, the assertions above would pass
    // equally against a door that accepted anything.
    let bogus = ActionAddress::new(format!("video.clip:{clip_id}"), "nonexistent-verb");
    assert!(
        matches!(agent.invoke(&bogus, &arguments), Err(DoorError::Unknown(_))),
        "an address the tree never published must be refused, not attempted",
    );

    // The change reaches the editor as a collaborator's does.
    let mut peer = ui::tilo_resource::Doc::load(&agent.save()).expect("the agent's replica");
    app.merge_peer(&mut peer)
        .expect("the agent's change merges");
    let clip = app
        .state()
        .clips
        .into_iter()
        .find(|clip| clip.id == clip_id)
        .expect("the clip is still there");
    assert!(
        (clip.fade_in - 1.5).abs() < 1e-9,
        "the fade did not survive to the app: invoking `{}` on `{}` with no window \
         left the clip at {}s",
        address.action,
        address.resource,
        clip.fade_in,
    );

    // ...and the app *presents* the result rather than merely storing it. A
    // clip the accessibility tree cannot name is a clip a screen reader, a
    // script and an agent cannot reach.
    app.harness().relayout();
    let exported = app.harness().a11y_nodes().len();
    assert!(
        exported > 1,
        "the editor exported no accessible tree ({exported} nodes): the edit is in \
         the document but nothing presents it",
    );
}
