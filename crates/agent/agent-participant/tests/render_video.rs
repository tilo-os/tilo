//! An agent cuts two clips the room holds into a timeline and has the host
//! render it: an MP4 that is the cut, the same bytes when rendered again,
//! and stills that show each clip.
//!
//! The render runs in the host's render service with the real
//! `tilo-video-render`: build it first (`cargo build -p video-editor --bin
//! tilo-video-render`) or name it in `TILO_VIDEO_RENDER`. Behind the
//! `render-tests` feature because it needs the media stack (with
//! `openh264enc`), a GPU adapter and `gst-launch-1.0` for the fixtures; it
//! fails, never skips, when any of them is missing.
#![cfg(feature = "render-tests")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a test's assertions and fixed-layout pixel reads"
)]

mod common;

tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();

use std::path::Path;

use common::catalogue;
use tilo_actions::door::ActionDescriptor;
use tilo_agent_participant::render::{self, RenderService};
use tilo_agent_participant::{ConfinedReplica, Seed};
use tilo_content::ContentStore;

const VIDEO: &str = "so.tilo.video-editor";

/// A 2 s, 320×180, 30 fps MJPEG clip of one colour (`0xAARRGGBB`).
fn solid_clip(argb: u32) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mov");
    let status = std::process::Command::new("gst-launch-1.0")
        .args(["-q", "videotestsrc", "pattern=solid-color"])
        .arg(format!("foreground-color={argb}"))
        .args([
            "num-buffers=60",
            "!",
            "video/x-raw,width=320,height=180,framerate=30/1",
            "!",
            "jpegenc",
            "!",
            "qtmux",
            "!",
            "filesink",
        ])
        .arg(format!("location={}", path.display()))
        .status()
        .expect("gst-launch-1.0 makes the fixtures");
    assert!(status.success(), "the fixture was not encoded");
    std::fs::read(path).unwrap()
}

fn operation(editor: &ConfinedReplica, action: &str) -> ActionDescriptor {
    editor
        .survey_with_effects()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
        .unwrap_or_else(|| panic!("the record publishes {action}"))
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// Run a render operation as the participant routes it.
fn render_op(
    editor: &ConfinedReplica,
    room: &ContentStore,
    action: &str,
    value: serde_json::Value,
) -> tilo_actions::ActionOutput {
    let descriptor = operation(editor, action);
    assert!(
        render::answers(&descriptor),
        "{action} is the host's render service's"
    );
    RenderService::shared()
        .invoke(editor, room, &descriptor.address, &arguments(value))
        .unwrap_or_else(|error| panic!("{action}: {error:?}"))
}

/// The room's bytes of the one content address `output` names.
fn produced(room: &ContentStore, output: &tilo_actions::ActionOutput) -> (String, Vec<u8>) {
    if let Some(value) = &output.value {
        assert_ne!(value["state"], "failed", "the render failed: {value}");
    }
    let [address] = output.resources.as_slice() else {
        panic!("one content resource: {output:?}");
    };
    let hash = tilo_actions::content_hash(address).expect("a content address");
    (address.clone(), room.read(hash).expect("the room keeps it"))
}

/// Check a render the way the helper's own test does.
fn check_video(output: &tilo_actions::ActionOutput, bytes: &[u8]) {
    let value = output.value.as_ref().expect("the export answers");
    assert_eq!(value["state"], "done", "{value}");
    assert_eq!(value["frames"], 120, "{value}");
    assert_eq!(value["width"], 1280);
    assert_eq!(value["height"], 720);
    assert_eq!(value["fps"], 30);
    assert_eq!(value["bytes"], bytes.len() as u64);
    let probed = video_editor::probe::probe_bytes(bytes, Path::new("render.mp4")).unwrap();
    assert!(
        (probed.duration - 4.0).abs() <= 1.0 / 30.0 + 1e-6,
        "duration {}",
        probed.duration
    );
    assert_eq!((probed.width, probed.height), (1280, 720));
    assert_eq!(probed.codec, "avc1");
    assert!((probed.fps - 30.0).abs() < 1e-6, "fps {}", probed.fps);
    assert_eq!(video_editor::probe::video_sample_count(bytes), Some(120));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one cut, rendered and looked at step by step"
)]
fn an_agent_renders_its_cut_of_room_footage_to_video_and_stills() {
    assert!(
        render::available(),
        "no tilo-video-render: build it (cargo build -p video-editor --bin tilo-video-render) \
         or name it in TILO_VIDEO_RENDER"
    );
    let room = ContentStore::memory();
    let red = room.put(&solid_clip(0xffd0_2020)).unwrap();
    let blue = room.put(&solid_clip(0xff20_40d0)).unwrap();
    let editor =
        ConfinedReplica::open(&catalogue(), VIDEO, common::ROOM, "cutter", true, Seed::New)
            .unwrap()
            .with_content(room.clone());
    editor.take_mutation().unwrap();
    let run = |action: &str, value: serde_json::Value| {
        let descriptor = operation(&editor, action);
        let output = editor
            .invoke(&descriptor.address, &arguments(value), false)
            .unwrap_or_else(|error| panic!("{action}: {error:?}"));
        // The host admits each change, so its copy is the record as it now is.
        editor.take_mutation().unwrap();
        output
    };
    for (hash, name) in [(&red, "red.mov"), (&blue, "blue.mov")] {
        let imported = run(
            "import-content",
            serde_json::json!({
                "content": tilo_actions::content_resource(hash).unwrap(),
                "name": name,
            }),
        );
        let media = imported.value.expect("the import answers its source");
        assert_eq!(media["media"]["duration"], 2.0, "{media}");
    }
    run("add-clip", serde_json::json!({"media": "red.mov"}));
    let second = run("add-clip", serde_json::json!({"media": "blue.mov"}))
        .value
        .unwrap();
    assert_eq!(second["start"], 2.0, "appended: {second}");

    let first = render_op(
        &editor,
        &room,
        "export-video-content",
        serde_json::json!({"wait": 120}),
    );
    let (address, bytes) = produced(&room, &first);
    check_video(&first, &bytes);
    assert_eq!(first.value.as_ref().unwrap()["content"], address.as_str());

    // Asked again, the same render answers, without rendering twice.
    let again = render_op(
        &editor,
        &room,
        "export-video-content",
        serde_json::json!({"wait": 0}),
    );
    assert_eq!(again.resources, first.resources, "the render is kept");

    // A change that draws nothing (a marker) is a new render of the same
    // picture: the same bytes.
    run("add-marker", serde_json::json!({"at": 1.0, "name": "Cut"}));
    let rerendered = render_op(
        &editor,
        &room,
        "export-video-content",
        serde_json::json!({"wait": 120}),
    );
    let (_, bytes_again) = produced(&room, &rerendered);
    check_video(&rerendered, &bytes_again);
    assert!(bytes == bytes_again, "two renders of one picture differ");

    for (at, is_red) in [(0.5, true), (2.5, false)] {
        let still = render_op(
            &editor,
            &room,
            "view-frame-content",
            serde_json::json!({"at": at, "width": 320}),
        );
        let (_, png) = produced(&room, &still);
        let thumb = thumbnail::decode_png(&png).expect("a PNG");
        assert_eq!((thumb.width, thumb.height), (320, 180));
        let at_centre = |c: usize| thumb.rgba[((90 * 320 + 160) * 4) as usize + c];
        let (r, b) = (at_centre(0), at_centre(2));
        if is_red {
            assert!(r > 150 && b < 90, "red at {at} s: {r} {b}");
        } else {
            assert!(b > 150 && r < 90, "blue at {at} s: {r} {b}");
        }
    }
    assert!(
        editor.take_mutation().unwrap().is_none(),
        "rendering changes nothing in the record"
    );
}
