#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "headless integration fixture asserts setup failures directly"
)]

//! End-to-end action test without a driving window. A participant with no
//! window — an agent, a script — holds the editor's view of the project,
//! attached and never drawn, surveys what it may do, and edits it through the
//! same door the editor's own window uses; the editor shows the result once
//! the change reaches it.

use tilo_actions::door::{ActionAddress, DoorError};
use ui::collaboration::DocumentMutation;
use ui::framework::{AttachedView, definition};
use ui::permissions::PermissionContext;
use ui::prelude::*;
use ui::record::RecordCheckout;
use ui::tilo_resource::{FIELD_EDIT, Id, Resource};
use video_editor::VideoEditor;
use video_editor::model::{MediaItem, MediaKind, Project, Track, TrackKind};
use video_editor::resource::load_project;

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
    app.invoke_confirmed(Invoke::edit(&load_project(&project)).unwrap())
        .expect("the project loads");
    (app, clip_id)
}

/// An action reaches the document with no window, and the app itself shows
/// the result.
#[test]
fn an_action_invoked_with_no_window_reaches_the_app() {
    let (mut app, clip_id) = editor_with_one_clip();

    // The editor's view of the record, as a host with no screen holds it.
    let definition = definition::<VideoEditor>();
    let mut agent = AttachedView::open(
        definition,
        "test-workspace",
        RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
        app.read_document(|doc| doc.clone().save()),
        "headless-driver",
        PermissionContext::permissive(),
        false,
    )
    .expect("the view holds the project");

    // Surveyed, not guessed. Asserting the address is offered *before*
    // invoking it is what keeps the next assertion honest: invoking
    // something the tree never advertised would prove nothing about the
    // surface an agent actually sees.
    let operations = agent.operations();
    let address = operations
        .iter()
        .map(|operation| operation.address.clone())
        .find(|address| address.resource == Project::KIND && address.action == FIELD_EDIT)
        .unwrap_or_else(|| {
            panic!(
                "no field edit offered on the project; the tree offered: {:?}",
                operations
                    .iter()
                    .map(|operation| (&operation.address.resource, &operation.address.action))
                    .collect::<Vec<_>>()
            )
        });

    let fade = Project::paths()
        .clips
        .item(&Id::from_raw(clip_id.to_string()))
        .fade_in
        .set(1.5);
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        ui::tilo_resource::EDIT_PARAM.into(),
        serde_json::to_value(&fade).unwrap(),
    );
    agent
        .invoke(&address, &arguments, false)
        .expect("the surveyed action applies");
    let changes = agent.take_changes();
    assert!(
        changes.is_some(),
        "the action reported success without changing the document",
    );

    // A negative control in the same run: the door must refuse an address
    // the tree does not carry. Without this, the assertions above would pass
    // equally against a door that accepted anything.
    // As a caller sends it over the wire: an address it made up.
    let bogus = ActionAddress::parse(&format!(
        "{}{}nonexistent-verb",
        Project::KIND,
        tilo_actions::KEY_SEP
    ))
    .expect("a well-formed address");
    assert!(
        matches!(
            agent.invoke(&bogus, &arguments, false),
            Err(DoorError::Unknown(_))
        ),
        "an address the tree never published must be refused, not attempted",
    );

    // The change reaches the editor as a collaborator's does.
    let (DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) =
        changes.expect("the agent's change");
    let mut peer = app.peer_document();
    tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
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
