#![allow(clippy::unwrap_used, reason = "test assertions")]

use super::*;
use crate::{AppCatalogue, Replica, ReplicaActor, Seed, component_apps};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// What an agent's video record lists before its host decides, held in
/// process as its worker holds it.
fn agents_video_survey() -> Vec<ActionDescriptor> {
    Replica::open(
        &AppCatalogue::new(component_apps()),
        VIDEO_EDITOR,
        ROOM,
        "editor",
        ReplicaActor::Agent,
        Seed::New,
    )
    .unwrap()
    .survey()
}

fn actions(descriptors: &[ActionDescriptor]) -> Vec<&str> {
    descriptors
        .iter()
        .map(|descriptor| descriptor.address.action.as_str())
        .collect()
}

#[test]
fn an_agents_worker_lists_the_renders_and_a_host_without_a_render_service_drops_them() {
    let listed = agents_video_survey();
    for render in [RENDER, EXPORT, STATUS, CANCEL, FRAME] {
        assert!(actions(&listed).contains(&render), "{render} is listed");
    }
    let without = offered_where(listed.clone(), false);
    for render in [RENDER, EXPORT, STATUS, CANCEL, FRAME] {
        assert!(!actions(&without).contains(&render), "{render} is dropped");
    }
    assert!(
        actions(&without).contains(&"export-timeline-content"),
        "what the worker answers itself stays"
    );
    assert_eq!(offered_where(listed.clone(), true).len(), listed.len());
}

#[test]
fn a_person_is_never_offered_the_agent_renders() {
    let catalogue = AppCatalogue::new(component_apps());
    let owner = Replica::open(
        &catalogue,
        VIDEO_EDITOR,
        ROOM,
        "ada",
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let listed = owner.survey();
    assert!(!actions(&listed).contains(&EXPORT));
    assert!(!actions(&listed).contains(&FRAME));
}

fn snapshot(heads: &[&str]) -> Snapshot {
    Snapshot {
        record: "record-1".into(),
        heads: heads.iter().map(|head| (*head).to_owned()).collect(),
        bytes: Vec::new(),
        project: Project::new("Cut"),
        range: (None, None),
    }
}

#[test]
fn a_render_is_identified_by_record_heads_settings_and_range() {
    let settings = AgentExportSettings::default();
    let one = snapshot(&["a", "b"]).job_id(&settings);
    assert_eq!(one, snapshot(&["a", "b"]).job_id(&settings));
    assert!(one.starts_with("render-"));
    assert_ne!(one, snapshot(&["a", "c"]).job_id(&settings));
    let smaller = AgentExportSettings {
        width: 640,
        height: 360,
        fps: 30,
    };
    assert_ne!(one, snapshot(&["a", "b"]).job_id(&smaller));
    let mut ranged = snapshot(&["a", "b"]);
    ranged.range = (Some(1.0), None);
    assert_ne!(one, ranged.job_id(&settings));
}

#[test]
fn a_render_larger_than_its_limit_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out");
    std::fs::write(&path, vec![1_u8; 2048]).unwrap();
    let content = ContentStore::memory();
    let error = keep(std::fs::File::open(&path).unwrap(), 1024, &content).unwrap_err();
    assert!(error.contains("larger"), "{error}");
    let (address, bytes) = keep(std::fs::File::open(&path).unwrap(), 4096, &content).unwrap();
    assert_eq!(bytes, 2048);
    let hash = tilo_actions::content_hash(&address).unwrap();
    assert_eq!(content.read(hash).unwrap().len(), 2048);
}

#[test]
fn a_timeline_placing_content_the_room_lacks_is_refused() {
    let mut project = Project::new("Cut");
    let track = video_editor::model::Track {
        kind: video_editor::model::TrackKind::Video,
        ..Default::default()
    };
    let media = video_editor::model::MediaItem {
        name: "interview.mp4".into(),
        kind: video_editor::model::MediaKind::Video,
        duration: 4.0,
        path: format!("tilo.content:{}", "ab".repeat(32)),
        ..Default::default()
    };
    let (track_id, media_id) = (track.id, media.id);
    project.tracks.push(track);
    project.media.push(media);
    project.add_clip(track_id, media_id, 0.0).unwrap();
    let mut held = snapshot(&[]);
    held.project = project;
    let error = held.check_sources(&ContentStore::memory()).unwrap_err();
    assert!(error.contains("does not hold"), "{error}");
}
