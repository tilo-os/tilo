#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test assertions over checked fixtures, and the shared footage helper"
)]
#![cfg(all(feature = "helper", unix))]

//! `tilo-video-render` end to end: the host lays out a job directory, the
//! helper confines itself and renders into the file it was handed, and the
//! same timeline renders to the same bytes.

use std::io::Read as _;
use std::time::{Duration, Instant};

use tilo_video_render::client::{self, Finished, JobDir};
use tilo_video_render::proto::{Reply, Request};
use video_editor::Project;
use video_editor::agent_render::{AGENT_VIDEO_ENCODER, AgentExportSettings, AgentFrameSettings};
use video_editor::model::{MediaItem, MediaKind, TextSource, Track, TrackKind};

#[allow(
    dead_code,
    reason = "the shared footage helper carries the media-stack check, which these tests replace \
              with the one encoder an agent's render needs"
)]
mod generated_clip {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/video-editor/tests/support/generated_clip.rs"
    ));
}

const HELPER: &str = env!("CARGO_BIN_EXE_tilo-video-render");

/// Whether this machine can encode an agent's video; a test without it says
/// so and returns, unless `TILO_REQUIRE_MEDIA=1` demands the stack.
fn encoder_present() -> bool {
    if gstreamer::init().is_ok() && gstreamer::ElementFactory::find(AGENT_VIDEO_ENCODER).is_some() {
        return true;
    }
    tilo_test_env::capability_unavailable(
        tilo_test_env::Capability::Media,
        format_args!("no {AGENT_VIDEO_ENCODER} (set GST_PLUGIN_PATH to its plugin directory)"),
    );
    false
}

/// A title held for `seconds` on V1, and a source at `source` under it when
/// given.
fn timeline(seconds: f64, source: Option<&str>) -> Project {
    let mut project = Project::new("Agent cut");
    project.set_frame_size(320, 180);
    let track = uuid::Uuid::from_u128(10);
    project.tracks.push(Track {
        id: track,
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        ..Track::default()
    });
    if let Some(path) = source {
        let lower = uuid::Uuid::from_u128(11);
        project.tracks.push(Track {
            id: lower,
            name: "V0".into(),
            kind: TrackKind::Video,
            index: 0,
            ..Track::default()
        });
        let media = uuid::Uuid::from_u128(21);
        project.media.push(MediaItem {
            id: media,
            name: "footage.mp4".into(),
            kind: MediaKind::Video,
            duration: generated_clip::DURATION_SECS,
            width: generated_clip::WIDTH,
            height: generated_clip::HEIGHT,
            fps: f64::from(generated_clip::FPS),
            path: path.into(),
            ..MediaItem::default()
        });
        let clip = project.add_clip(lower, media, 0.0).unwrap();
        project.clip_mut(clip).unwrap().out_point = seconds;
    }
    let title = uuid::Uuid::from_u128(20);
    project.media.push(MediaItem {
        id: title,
        name: "Title".into(),
        kind: MediaKind::Text,
        duration: seconds,
        text: Some(TextSource::new("Agent")),
        ..MediaItem::default()
    });
    let clip = project.add_clip(track, title, 0.0).unwrap();
    project.clip_mut(clip).unwrap().out_point = seconds;
    project
}

/// Run the helper on `project` (with `sources` laid out by hash) for
/// `request`.
fn run(project: &Project, sources: &[(&str, &[u8])], request: &Request) -> Finished {
    let parent = tempfile::tempdir().unwrap();
    let mut job = JobDir::create(parent.path(), 64 << 20).unwrap();
    job.put_snapshot(&video_editor::seed::document_of(project).save())
        .unwrap();
    for (hash, bytes) in sources {
        job.put_source(hash, &mut &bytes[..]).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    client::render(
        std::path::Path::new(HELPER),
        &job,
        request,
        deadline,
        &mut |_, _| true,
    )
    .unwrap_or_else(|error| panic!("the helper rendered nothing: {error}"))
}

fn bytes(mut finished: Finished) -> (Reply, Vec<u8>) {
    let mut out = Vec::new();
    finished.output.read_to_end(&mut out).unwrap();
    (finished.reply, out)
}

const SETTINGS: AgentExportSettings = AgentExportSettings {
    width: 320,
    height: 180,
    fps: 24,
};

#[test]
#[serial_test::serial(video_encode)]
fn a_confined_render_of_a_title_is_an_mp4_and_the_same_bytes_twice() {
    if !encoder_present() {
        return;
    }
    let project = timeline(1.0, None);
    let request = Request::Video { settings: SETTINGS };
    let (reply, first) = bytes(run(&project, &[], &request));
    let Reply::Video(rendered) = reply else {
        panic!("answered {reply:?}");
    };
    assert_eq!(rendered.frames, 24);
    assert_eq!(first.get(4..8), Some(&b"ftyp"[..]), "an ISO-BMFF file");
    let mut zeroed = first.clone();
    video_editor::agent_render::zero_mp4_times(&mut zeroed);
    assert_eq!(zeroed, first, "the clock times are already zero");
    let (_, second) = bytes(run(&project, &[], &request));
    assert_eq!(first, second, "one timeline, one render");
}

#[test]
#[serial_test::serial(video_encode)]
fn a_confined_render_reads_room_content_laid_out_by_hash() {
    if !encoder_present() {
        return;
    }
    let footage = std::fs::read(generated_clip::encoded_video()).unwrap();
    let hash = ui::framework::content::ContentStore::memory()
        .put(&footage)
        .unwrap();
    let project = timeline(1.0, Some(&format!("tilo.content:{hash}")));
    let request = Request::Video { settings: SETTINGS };
    let (reply, out) = bytes(run(&project, &[(&hash, &footage)], &request));
    assert!(
        matches!(reply, Reply::Video(r) if r.frames == 24),
        "{reply:?}"
    );
    assert!(out.len() > 1000);
}

#[test]
#[serial_test::serial(video_encode)]
fn a_render_whose_source_is_missing_says_so() {
    if !encoder_present() {
        return;
    }
    let project = timeline(1.0, Some(&format!("tilo.content:{}", "ab".repeat(32))));
    let parent = tempfile::tempdir().unwrap();
    let job = JobDir::create(parent.path(), 1 << 20).unwrap();
    job.put_snapshot(&video_editor::seed::document_of(&project).save())
        .unwrap();
    let outcome = client::render(
        std::path::Path::new(HELPER),
        &job,
        &Request::Video { settings: SETTINGS },
        Instant::now() + Duration::from_secs(60),
        &mut |_, _| true,
    );
    let error = outcome.err().unwrap();
    assert!(error.contains("footage.mp4"), "{error}");
}

#[test]
#[serial_test::serial(video_encode)]
fn a_confined_still_is_a_png_of_the_asked_size() {
    if !encoder_present() {
        return;
    }
    let project = timeline(1.0, None);
    let frame = AgentFrameSettings {
        at: 0.5,
        width: 160,
        height: 90,
    };
    let (reply, png) = bytes(run(&project, &[], &Request::Frame { frame }));
    assert_eq!(reply, Reply::Frame);
    assert_eq!(png.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (160, 90));
}
