#![allow(clippy::unwrap_used, reason = "test assertions")]
#![cfg(feature = "gst")]

//! `render-timeline`: the timeline rendered on the device into content, through the
//! same door, permissions and job progress as any other action.

use ui::permissions::{KindSet, PermissionContext, TreePath};
use ui::prelude::*;
use video_editor::actions::VideoEditorEffect;
use video_editor::model::{MediaItem, MediaKind, TextSource, Track, TrackKind};
use video_editor::resource::load_project;
use video_editor::{Project, VideoEditor};

mod media_stack {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/media_stack.rs"
    ));
}

const AGENT: &str = "renderer";

/// One title held for two seconds.
fn titled() -> Project {
    let mut project = Project::new("Tiny");
    let media = uuid::Uuid::new_v4();
    project.media.push(MediaItem {
        id: media,
        name: "Title".into(),
        kind: MediaKind::Text,
        duration: 2.0,
        text: Some(TextSource::new("Hi")),
        ..MediaItem::default()
    });
    let track = uuid::Uuid::new_v4();
    project.tracks.push(Track {
        id: track,
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        ..Track::default()
    });
    let clip = project.add_clip(track, media, 0.0).unwrap();
    project.clip_mut(clip).unwrap().out_point = 2.0;
    project
}

fn render(start: f64, end: f64) -> VideoEditorEffect {
    VideoEditorEffect::RenderTimeline {
        start: Some(start),
        end: Some(end),
        format: Some("webm".into()),
        size: Some("720p".into()),
    }
}

fn agent(kinds: KindSet) -> PermissionContext {
    PermissionContext::agent_granted(AGENT, &TreePath::root().app(VideoEditor::ID), kinds)
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn an_agent_renders_a_range_into_content_with_progress() {
    if !media_stack::media_stack() {
        return;
    }
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&titled())).unwrap())
        .unwrap();
    let versions = app.versions().len();

    // Rendering reads: an agent allowed to read may, one allowed only to
    // change the project may not.
    app.set_permissions(agent(KindSet::UNDOABLE));
    assert!(app.effect(&render(0.5, 0.75)).is_err());

    app.set_permissions(agent(KindSet::READ));
    let output = app.effect(&render(0.5, 0.75)).unwrap();
    let value = output.value.unwrap();
    assert_eq!(value["start"], 0.5);
    assert_eq!(value["end"], 0.75);
    assert_eq!(value["format"], "webm");
    let address = value["content"].as_str().unwrap();
    assert_eq!(output.resources, [address.to_owned()]);
    let hash = ui::tilo_actions::content_hash(address).unwrap();
    let video = app.content().read(hash).unwrap();
    assert_eq!(value["bytes"], video.len());
    assert!(
        video.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "the content is a WebM file"
    );

    let progress: Vec<_> = app
        .effect_progress()
        .into_iter()
        .filter_map(|update| match update {
            tilo_actions::InvocationUpdate::Progress {
                completed, total, ..
            } => Some((completed, total)),
            _ => None,
        })
        .collect();
    // A quarter second at 30 frames a second, each frame reported.
    assert_eq!(progress.last(), Some(&(Some(8), Some(8))), "{progress:?}");
    assert_eq!(app.versions().len(), versions, "rendering changed nothing");
}

#[test]
fn a_render_outside_the_timeline_is_refused() {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    assert!(app.effect(&render(0.0, 1.0)).is_err(), "nothing to render");
    app.invoke_confirmed(Invoke::edit(&load_project(&titled())).unwrap())
        .unwrap();
    assert!(app.effect(&render(5.0, 6.0)).is_err());
    assert!(app.effect(&render(1.0, 0.5)).is_err());
}
