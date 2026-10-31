//! Recording the screen through the shared media stack: a recording is a
//! file that takes the frames as they come and is finished when it is
//! stopped, and a clip of what just happened is written from the replay
//! buffer without a second encode. Machines whose media stack cannot
//! encode here skip these with a note, as the share streams' tests do.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::Path;

use tilo_capture_studio::{Frame, Recorder, Settings, State};

/// A 64 x 48 frame whose bright band moves with `step`, so the encoder has
/// something to code between frames.
fn frame(step: u32) -> Frame {
    let (width, height) = (64_u32, 48_u32);
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let lit = x / 8 == step % 8;
            let at = ((y * width + x) * 4) as usize;
            rgba[at] = if lit { 240 } else { 20 };
            rgba[at + 1] = u8::try_from(y % 256).unwrap();
            rgba[at + 2] = 30;
            rgba[at + 3] = 255;
        }
    }
    Frame::new(width, height, rgba).expect("a frame of its own size")
}

/// A recorder here, or a note that this machine has no encoder and the test
/// is skipped.
fn recorder() -> Option<Recorder> {
    media::gst::init().ok()?;
    match Recorder::new(Settings::for_size(64, 48)) {
        Ok(recorder) => Some(recorder),
        Err(error) => {
            eprintln!("no encoder on this machine ({error}): skipped");
            None
        }
    }
}

/// How big the file at `path` is, asserting there is something in it.
fn size(path: &Path) -> u64 {
    let bytes = std::fs::metadata(path).expect("the file is there").len();
    assert!(bytes > 0, "the file has the recording in it");
    bytes
}

#[test]
fn a_recording_is_a_file_that_takes_the_frames_until_it_is_stopped() {
    let Some(mut recorder) = recorder() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recording.mp4");
    recorder.start(path.clone()).expect("a recording starts");
    assert_eq!(recorder.state(), State::Recording);
    for step in 0..90 {
        recorder.frame(&frame(step)).expect("a frame is encoded");
    }
    assert!(recorder.buffered() > 0, "the replay buffer fills too");

    recorder.pause();
    assert_eq!(recorder.state(), State::Paused, "a pause keeps the file");
    for step in 90..120 {
        recorder
            .frame(&frame(step))
            .expect("a paused frame encodes");
    }
    recorder.resume();
    assert_eq!(recorder.state(), State::Recording);
    for step in 120..180 {
        recorder.frame(&frame(step)).expect("a frame is encoded");
    }

    let finished = recorder.stop().expect("the file finishes");
    assert_eq!(finished, Some(path.clone()));
    assert_eq!(recorder.state(), State::Idle);
    size(&path);
}

#[test]
fn a_clip_of_what_just_happened_comes_out_of_the_replay_buffer() {
    let Some(mut recorder) = recorder() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    // Not recording: the replay buffer fills anyway.
    for step in 0..90 {
        recorder.frame(&frame(step)).expect("a frame is encoded");
    }
    let held = recorder.buffered();
    assert!(held > 0);
    let clip = directory.path().join("clip.mp4");
    let written = recorder
        .save_replay(clip.clone())
        .expect("the buffered past is a clip");
    assert!(written > 0 && written <= held);
    size(&clip);
}

#[test]
fn the_replay_buffer_keeps_only_what_it_may() {
    let mut settings = Settings::for_size(64, 48);
    settings.replay_frames = 30;
    settings.replay_bytes = 64 * 1024;
    let Ok(mut recorder) = Recorder::new(settings) else {
        eprintln!("no encoder on this machine: skipped");
        return;
    };
    for step in 0..300 {
        recorder.frame(&frame(step)).expect("a frame is encoded");
    }
    assert!(recorder.buffered() <= 30, "{} frames", recorder.buffered());
    assert!(
        recorder.buffered_bytes() <= 64 * 1024,
        "{} bytes",
        recorder.buffered_bytes()
    );
    assert!(recorder.buffered() > 0, "the recent past is kept");
}

#[test]
fn a_frame_must_be_the_size_it_says_it_is() {
    assert!(Frame::new(2, 2, vec![0; 16]).is_ok());
    assert!(Frame::new(2, 2, vec![0; 15]).is_err());
}
