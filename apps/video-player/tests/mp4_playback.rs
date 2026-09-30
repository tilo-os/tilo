//! Real H.264/AAC playback through the app's semantic controls and media engine.

#![cfg(feature = "decode")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test prerequisites and production playback failures must fail explicitly"
)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use ui::time::Duration;

use media::gst::prelude::*;
use ui::widget::Widget;
use video_player::PlayerApp;
use video_player::actions::{OPERATION_TYPE, PlayerOperation};

const CHILD_ROOT: &str = "TILO_VIDEO_MP4_TEST_ROOT";

fn frame_pts(app: &PlayerApp) -> Option<u64> {
    app.player()
        .engine()
        .and_then(media::MediaEngine::latest_frame)
        .map(|frame| frame.pts_ms)
}

fn invoke(app: &mut PlayerApp, doc: &mut automerge::AutoCommit, operation: &PlayerOperation) {
    assert!(matches!(
        app.start_operation(
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                OPERATION_TYPE,
                &serde_json::to_vec(operation).unwrap()
            ),
            doc,
            &automerge::ROOT
        )
        .expect("production playback action"),
        Some(ui::widget::ResourceOperationStart::Completed(_))
    ));
}

fn wait_for(
    app: &mut PlayerApp,
    doc: &mut automerge::AutoCommit,
    description: &str,
    ready: impl Fn(&PlayerApp) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.sync(doc, &automerge::ROOT);
        if ready(app) {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "timed out waiting for {description}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn h264_aac_mp4_plays_pauses_seeks_and_resumes_through_app_actions() {
    // Settings/recent-history writes and environment overrides belong only to
    // the child. Parallel tests and the user's saved player state stay isolated.
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        exercise_app(Path::new(&root));
        return;
    }
    let nonce = ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("tilo MP4 日本語 #%-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "h264_aac_mp4_plays_pauses_seeks_and_resumes_through_app_actions",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let result = command.output().expect("run isolated MP4 playback");
    std::fs::remove_dir_all(&root).expect("release the MP4 and isolated state");
    assert!(
        result.status.success(),
        "MP4 playback child failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    // libtest exits 0 when `--exact` selects nothing; require the child ran.
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("test result: ok. 1 passed"),
        "the child ran no test: {}",
        String::from_utf8_lossy(&result.stdout)
    );
}

fn exercise_app(root: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/h264-aac.mp4");
    let sample = root.join("Moving picture and silent audio.mp4");
    std::fs::copy(fixture, &sample).expect("copy the H.264/AAC fixture");
    let mut app = PlayerApp::new().with_open_path(&sample);
    let engine = app.player().engine().expect("production engine");
    assert!(
        engine.is_real_decode(),
        "GStreamer is required; no synthesized fallback"
    );

    // Observe actual decoded PCM entering the production audio sink. Track
    // metadata alone would still pass if AAC were unsupported and dropped.
    // The fixture encodes silence, so this never plays an audible test tone.
    let sink = engine.audio_sink().expect("production audio output");
    let pad = sink.static_pad("sink").expect("audio sink input");
    let audio_buffers = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&audio_buffers);
    let probe = pad
        .add_probe(media::gst::PadProbeType::BUFFER, move |pad, info| {
            if info.buffer().is_some_and(|buffer| buffer.size() > 0)
                && pad.current_caps().is_some_and(|caps| {
                    caps.structure(0)
                        .is_some_and(|structure| structure.name() == "audio/x-raw")
                })
            {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            media::gst::PadProbeReturn::Ok
        })
        .expect("observe decoded AAC buffers");

    let mut doc = automerge::AutoCommit::new();
    wait_for(
        &mut app,
        &mut doc,
        "H.264 pictures and decoded AAC audio",
        |app| {
            frame_pts(app).is_some_and(|pts| pts >= 160)
                && audio_buffers.load(Ordering::Relaxed) >= 3
                && app.player().duration_ms() >= 3_900
        },
    );
    let frame = app.player().latest_frame().unwrap();
    assert_eq!((frame.width, frame.height), (160, 120));
    assert_eq!(
        app.player()
            .engine()
            .unwrap()
            .latest_frame()
            .unwrap()
            .rgba
            .len(),
        160 * 120 * 4
    );
    assert_eq!(app.player().engine().unwrap().audio_tracks().len(), 1);
    assert!(app.player().is_playing());

    invoke(&mut app, &mut doc, &PlayerOperation::TogglePlayback);
    assert!(!app.player().is_playing());
    invoke(&mut app, &mut doc, &PlayerOperation::SeekSeconds(2.0));
    wait_for(
        &mut app,
        &mut doc,
        "paused frame at the requested seek position",
        |app| frame_pts(app).is_some_and(|pts| (1_960..=2_040).contains(&pts)),
    );
    let paused_pts = frame_pts(&app).unwrap();
    let hold_until = ui::time::now()
        .checked_add(Duration::from_millis(200))
        .expect("hold deadline is representable");
    while ui::time::now() < hold_until {
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(frame_pts(&app).unwrap(), paused_pts);
        assert!(!app.player().is_playing());
        std::thread::sleep(Duration::from_millis(10));
    }

    let audio_before_resume = audio_buffers.load(Ordering::Relaxed);
    invoke(&mut app, &mut doc, &PlayerOperation::TogglePlayback);
    wait_for(
        &mut app,
        &mut doc,
        "resumed picture and audio progression",
        |app| {
            app.player().is_playing()
                && frame_pts(app).is_some_and(|pts| pts >= paused_pts.saturating_add(160))
                && audio_buffers.load(Ordering::Relaxed) > audio_before_resume
        },
    );
    pad.remove_probe(probe);
    drop(pad);
    drop(sink);
    drop(app);
}
