//! `tilo-video-render` renders a timeline of room content, confined, to an
//! MP4 that is exactly the cut and byte-equal from one render to the next.
//!
//! These run the real helper binary this package builds, against two
//! generated clips, through the same client an agent's host uses. They need
//! the media stack, a GPU adapter and `openh264enc`; without any of those
//! they fail with the helper's reason rather than pass without looking.
#![cfg(all(feature = "gst", feature = "decode", unix))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "a test's assertions and fixed-size pixel arithmetic"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use video_editor::export::{AgentExportSettings, AgentFrameSettings};
use video_editor::model::{Clip, MediaItem, MediaKind, Project, Track, TrackKind};
use video_editor::render_helper::client::{self, JobDir};
use video_editor::render_helper::proto::{Reply, Request};

const HELPER: &str = env!("CARGO_BIN_EXE_tilo-video-render");

/// A 2 s, 320×180, 30 fps MJPEG `QuickTime` clip of one solid colour
/// (`0xAARRGGBB`), encoded in-process: the decode path a room's footage
/// takes, without depending on any H.264 encoder for the fixture.
fn solid_clip(path: &Path, argb: u32) {
    use gstreamer::prelude::*;
    gstreamer::init().unwrap();
    let pipeline = gstreamer::parse::launch(&format!(
        "videotestsrc pattern=solid-color foreground-color={argb} num-buffers=60 \
         ! video/x-raw,width=320,height=180,framerate=30/1 ! jpegenc ! qtmux \
         ! filesink location={}",
        path.display()
    ))
    .expect("the fixture graph builds (videotestsrc, jpegenc, qtmux)");
    pipeline.set_state(gstreamer::State::Playing).unwrap();
    let bus = pipeline.bus().unwrap();
    let message = bus
        .timed_pop_filtered(
            gstreamer::ClockTime::from_seconds(60),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        )
        .expect("the fixture finishes");
    pipeline.set_state(gstreamer::State::Null).unwrap();
    assert!(
        matches!(message.view(), gstreamer::MessageView::Eos(_)),
        "the fixture failed: {message:?}"
    );
}

/// A content hash for a fixture: any 64 hex digits name it in the job
/// directory, as the room's hash would.
fn hash_of(bytes: &[u8]) -> String {
    let mut digest = [0u8; 32];
    for (i, byte) in bytes.iter().enumerate() {
        digest[i % 32] = digest[i % 32].wrapping_mul(31).wrapping_add(*byte);
    }
    digest.iter().fold(String::new(), |mut hex, b| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// Two clips back to back on V1, red then blue, each 2 s, as content.
struct Fixture {
    project: Project,
    sources: Vec<(String, PathBuf)>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut project = Project::new("Two clips");
    project.width = 1280;
    project.height = 720;
    let track = Track {
        id: uuid::Uuid::new_v4(),
        name: "V1".into(),
        kind: TrackKind::Video,
        ..Default::default()
    };
    let mut sources = Vec::new();
    for (i, argb) in [0xffd0_2020_u32, 0xff20_40d0].into_iter().enumerate() {
        let path = dir.path().join(format!("clip{i}.mov"));
        solid_clip(&path, argb);
        let hash = hash_of(&std::fs::read(&path).unwrap());
        let media = MediaItem {
            id: uuid::Uuid::new_v4(),
            name: format!("clip{i}.mov"),
            kind: MediaKind::Video,
            duration: 2.0,
            width: 320,
            height: 180,
            path: video_editor::media::content_address(&hash),
            ..Default::default()
        };
        project.clips.push(Clip {
            id: uuid::Uuid::new_v4(),
            track: track.id,
            media: media.id,
            start: 2.0 * i as f64,
            in_point: 0.0,
            out_point: 2.0,
            ..Default::default()
        });
        project.media.push(media);
        sources.push((hash, path));
    }
    project.tracks.push(track);
    Fixture {
        project,
        sources,
        _dir: dir,
    }
}

/// Lay the fixture out and run the helper on it.
fn render(fixture: &Fixture, request: &Request) -> (Reply, Vec<u8>) {
    let mut doc = automerge::AutoCommit::new();
    video_editor::doc::VideoDoc::write_project(&mut doc, &fixture.project);
    let mut job = JobDir::create(&std::env::temp_dir(), 512 << 20).unwrap();
    job.put_snapshot(&doc.save()).unwrap();
    for (hash, path) in &fixture.sources {
        job.put_source(hash, &mut std::fs::File::open(path).unwrap())
            .unwrap();
    }
    let mut reports = 0;
    let finished = client::render(
        Path::new(HELPER),
        &job,
        request,
        Instant::now() + Duration::from_secs(180),
        &mut |_, _| {
            reports += 1;
            true
        },
    )
    .unwrap_or_else(|error| panic!("the helper did not render: {error}"));
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut &finished.output, &mut bytes).unwrap();
    (finished.reply, bytes)
}

#[test]
#[serial_test::serial(render_helper)]
fn two_clips_render_to_one_four_second_avc_file_byte_equal_each_time() {
    let fixture = fixture();
    let request = Request::Video {
        settings: AgentExportSettings::default(),
    };
    let (reply, first) = render(&fixture, &request);
    let Reply::Video(rendered) = reply else {
        panic!("the helper answered {reply:?}");
    };
    assert_eq!(rendered.frames, 120, "4 s at 30 fps");
    let probed = video_editor::probe::probe_bytes(&first, Path::new("render.mp4")).unwrap();
    assert!(
        (probed.duration - 4.0).abs() <= 1.0 / 30.0 + 1e-6,
        "duration {}",
        probed.duration
    );
    assert_eq!((probed.width, probed.height), (1280, 720));
    assert_eq!(probed.codec, "avc1");
    assert!((probed.fps - 30.0).abs() < 1e-6, "fps {}", probed.fps);
    assert!(!probed.has_audio, "silent footage writes no audio track");
    assert_eq!(video_editor::probe::video_sample_count(&first), Some(120));

    let (_, second) = render(&fixture, &request);
    assert!(
        first == second,
        "two renders of one timeline differ ({} and {} bytes)",
        first.len(),
        second.len()
    );
}

/// The average colour of a PNG's middle third.
fn middle_colour(png: &[u8]) -> (u32, u32, [u8; 3]) {
    let thumb = thumbnail::decode_png(png).expect("a PNG");
    let (w, h) = (thumb.width, thumb.height);
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for y in h / 3..2 * h / 3 {
        for x in w / 3..2 * w / 3 {
            let i = ((y * w + x) * 4) as usize;
            for (c, total) in sum.iter_mut().enumerate() {
                *total += u64::from(thumb.rgba[i + c]);
            }
            n += 1;
        }
    }
    let mean = sum.map(|total| u8::try_from(total / n.max(1)).unwrap_or(u8::MAX));
    (w, h, mean)
}

#[test]
#[serial_test::serial(render_helper)]
fn a_still_shows_the_clip_under_its_time() {
    let fixture = fixture();
    for (at, red) in [(0.5, true), (2.5, false)] {
        let (reply, png) = render(
            &fixture,
            &Request::Frame {
                frame: AgentFrameSettings {
                    at,
                    width: 320,
                    height: 180,
                },
            },
        );
        assert_eq!(reply, Reply::Frame);
        let (w, h, [r, _, b]) = middle_colour(&png);
        assert_eq!((w, h), (320, 180));
        if red {
            assert!(r > 150 && b < 90, "red at {at} s: {r} {b}");
        } else {
            assert!(b > 150 && r < 90, "blue at {at} s: {r} {b}");
        }
    }
}
