//! An agent's video renders are answered by its host's render service, from
//! the record the host holds, in the confined `tilo-video-render` helper:
//! never in the record's app worker, and offered only where the helper is.
//!
//! The helper is found beside the test's build (`cargo build -p
//! tilo-video-render --bin tilo-video-render`) or at `$TILO_VIDEO_RENDER`; its
//! encoder through `GST_PLUGIN_PATH` where it is not a system plugin.
//! Without either, the rendering tests say so and return, unless
//! `TILO_REQUIRE_MEDIA=1` demands them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use common::catalogue;
use tilo_actions::door::{ActionDescriptor, DoorError};
use tilo_agent_participant::{ConfinedReplica, ReplicaActor, Seed, render};
use tilo_content::ContentStore;
use video_editor::Project;
use video_editor::model::{MediaItem, MediaKind, TextSource, Track, TrackKind};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const VIDEO: &str = "so.tilo.video-editor";

/// Whether this host renders; a test that needs it says so otherwise.
fn renders() -> bool {
    if render::available() {
        return true;
    }
    assert!(
        std::env::var_os("TILO_REQUIRE_MEDIA").is_none(),
        "the render helper is required: build tilo-video-render"
    );
    eprintln!("skipped: no tilo-video-render beside the test or at $TILO_VIDEO_RENDER");
    false
}

/// A one-second title, and a source at `source` under it when given.
fn timeline(source: Option<&str>) -> Project {
    let mut project = Project::new("Agent cut");
    project.set_frame_size(320, 180);
    let track = Track {
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        ..Track::default()
    };
    let title = MediaItem {
        name: "Title".into(),
        kind: MediaKind::Text,
        duration: 1.0,
        text: Some(TextSource::new("Agent")),
        ..MediaItem::default()
    };
    let (track_id, title_id) = (track.id, title.id);
    project.tracks.push(track);
    project.media.push(title);
    let clip = project.add_clip(track_id, title_id, 0.0).unwrap();
    project.clip_mut(clip).unwrap().out_point = 1.0;
    if let Some(path) = source {
        let media = MediaItem {
            id: video_editor::agent_timeline::content_media_id(&"ab".repeat(32)),
            name: "interview.mp4".into(),
            kind: MediaKind::Video,
            duration: 1.0,
            path: path.into(),
            ..MediaItem::default()
        };
        let media_id = media.id;
        project.media.push(media);
        project.add_clip(track_id, media_id, 1.0).unwrap();
    }
    project
}

/// `project` held for an agent, lent `room`: a new record the agent loads
/// the project into through the door, as opening a project file does.
fn agents_record(project: &Project, room: &ContentStore) -> ConfinedReplica {
    let agent = ConfinedReplica::open(
        &catalogue(),
        VIDEO,
        ROOM,
        "editor",
        &ReplicaActor::Agent,
        Seed::New,
    )
    .unwrap()
    .lending(room.clone());
    let load = offered(&agent, "field-edit").expect("field edits are offered");
    let edit = serde_json::json!({ "edit": video_editor::resource::load_project(project) });
    agent
        .invoke(&load.address, &arguments(edit), true)
        .unwrap_or_else(|error| panic!("the project loads: {error}"));
    agent.take_mutation().unwrap();
    agent
}

fn offered(agent: &ConfinedReplica, action: &str) -> Option<ActionDescriptor> {
    agent
        .survey()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

#[test]
fn an_agent_is_offered_the_renders_exactly_where_its_host_renders() {
    let agent = agents_record(&timeline(None), &ContentStore::memory());
    for action in [
        "export-video-content",
        "view-frame-content",
        "render-timeline",
    ] {
        assert_eq!(
            offered(&agent, action).is_some(),
            render::available(),
            "{action} is offered only with a render service"
        );
    }
    assert!(
        offered(&agent, "export-timeline-content").is_some(),
        "what the worker answers itself is always offered"
    );
}

#[test]
fn an_export_is_rendered_on_the_host_kept_as_content_and_answered_again_unchanged() {
    if !renders() {
        return;
    }
    let room = ContentStore::memory();
    let agent = agents_record(&timeline(None), &room);
    let export = offered(&agent, "export-video-content").unwrap().address;
    let ask = arguments(serde_json::json!({ "width": 320, "height": 180, "fps": 24 }));
    let first = agent
        .invoke(&export, &ask, true)
        .unwrap_or_else(|error| panic!("the host renders: {error}"));
    let answer = first.value.clone().unwrap();
    assert_eq!(answer["state"], "done", "{answer}");
    assert_eq!(answer["frames"], 24);
    let address = answer["content"].as_str().unwrap();
    assert_eq!(first.resources, vec![address.to_owned()]);
    let hash = tilo_actions::content_hash(address).unwrap();
    let video = room.read(hash).expect("the room keeps the video");
    assert_eq!(video.get(4..8), Some(&b"ftyp"[..]));
    let again = agent.invoke(&export, &ask, true).unwrap();
    assert_eq!(
        again.value.unwrap()["content"],
        address,
        "one cut, one render"
    );
}

#[test]
fn a_still_is_drawn_on_the_host_as_a_png_kept_as_content() {
    if !renders() {
        return;
    }
    let room = ContentStore::memory();
    let agent = agents_record(&timeline(None), &room);
    let frame = offered(&agent, "view-frame-content").unwrap().address;
    let output = agent
        .invoke(
            &frame,
            &arguments(serde_json::json!({ "at": 0.5, "width": 160 })),
            true,
        )
        .unwrap_or_else(|error| panic!("the host draws: {error}"));
    let answer = output.value.unwrap();
    assert_eq!(
        (answer["width"].as_u64(), answer["height"].as_u64()),
        (Some(160), Some(90))
    );
    let hash = tilo_actions::content_hash(answer["content"].as_str().unwrap()).unwrap();
    let png = room.read(hash).unwrap();
    assert_eq!(png.get(..4), Some(&b"\x89PNG"[..]));
}

#[test]
fn a_render_of_content_the_room_lacks_or_a_size_too_large_is_refused_before_rendering() {
    if !renders() {
        return;
    }
    let room = ContentStore::memory();
    let missing = format!("tilo.content:{}", "ab".repeat(32));
    let agent = agents_record(&timeline(Some(&missing)), &room);
    let export = offered(&agent, "export-video-content").unwrap().address;
    match agent.invoke(&export, &serde_json::Map::new(), true) {
        Err(DoorError::Unavailable { code, reason }) => {
            assert_eq!(code, "render_refused");
            assert!(reason.contains("does not hold"), "{reason}");
        }
        other => panic!("a source the room lacks is refused: {other:?}"),
    }
    let plain = agents_record(&timeline(None), &room);
    let too_big = arguments(serde_json::json!({ "width": 3840, "height": 2160 }));
    assert!(matches!(
        plain.invoke(&export, &too_big, true),
        Err(DoorError::InvalidArguments(_))
    ));
}
