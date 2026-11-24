//! A video a tilolang view declares decodes a frame.
//!
//! `ui.VideoPlayer` is the one surface every tilolang video shows, Tube's watch page among them:
//! the window process builds the engine its view's props ask for and plays what the view named.
//! This builds that declared player over a short generated clip and holds it until it has a
//! picture, so a player that draws only its controls and never decodes fails here. The process
//! runs with the decoder ([`ui`'s `decode`]); a build without it would show a synthesized frame
//! instead, which is why this lives in the runtime bundle's own crate. The clip is made at test
//! time (`GStreamer`, or ffmpeg): no binary media is committed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// The tools these tests start inherit the test's environment: they run one at a time.
use tilo_test_env::SpawnUnderEnvLock as _;

use ui::widget::Widget as _;
use ui::widget::video_player::{DeclaredVideo, VideoPlayerProps};

/// A short VP9/AAC clip at `path`, made with whichever tool the test image carries.
fn make_clip(path: &Path) -> Result<(), String> {
    let gst = Command::new("gst-launch-1.0")
        .args([
            "-e",
            "-q",
            "mp4mux",
            "name=mux",
            "!",
            "filesink",
            &format!("location={}", path.display()),
            "videotestsrc",
            "num-buffers=50",
            "pattern=ball",
            "!",
            "video/x-raw,width=160,height=120,framerate=25/1",
            "!",
            "videoconvert",
            "!",
            "vp9enc",
            "!",
            "queue",
            "!",
            "mux.video_0",
            "audiotestsrc",
            "num-buffers=94",
            "samplesperbuffer=1024",
            "wave=silence",
            "!",
            "audio/x-raw,rate=48000,channels=1",
            "!",
            "audioconvert",
            "!",
            "avenc_aac",
            "!",
            "aacparse",
            "!",
            "queue",
            "!",
            "mux.audio_0",
        ])
        .status_locked();
    if gst.is_ok_and(|status| status.success()) && path.is_file() {
        return Ok(());
    }
    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=mono:sample_rate=48000",
            "-shortest",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "64k",
            "-c:a",
            "aac",
            &path.display().to_string(),
        ])
        .status_locked();
    if ffmpeg.is_ok_and(|status| status.success()) && path.is_file() {
        return Ok(());
    }
    Err("neither gst-launch-1.0 nor ffmpeg made the test's clip".to_owned())
}

#[test]
fn a_declared_player_decodes_the_clip_it_names() {
    let clip: PathBuf =
        std::env::temp_dir().join(format!("tilo-video-decode-{}.mp4", std::process::id()));
    make_clip(&clip).expect("a tool to make the clip");
    let source = media::Source::for_path(&clip)
        .to_uri()
        .expect("the clip's address");
    let mut video = DeclaredVideo::new(VideoPlayerProps {
        title: Some("generated clip".to_owned()),
        source: Some(source),
        playing: true,
        ..VideoPlayerProps::default()
    });
    // Playback runs on the engine's own thread: pump frames until it has a picture, or fail.
    let began = Instant::now();
    loop {
        let (state, path) = ((), ());
        video.sync(&state, &path);
        if video.player().latest_frame().is_some() {
            break;
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the declared player decoded no frame from the clip it named"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = std::fs::remove_file(&clip);
}

/// An HLS stream in `dir`, returning its playlist: segments and a playlist made at test time
/// with whichever tool the test image carries. Nothing binary is committed.
fn make_hls(dir: &Path) -> Result<PathBuf, String> {
    let playlist = dir.join("index.m3u8");
    let segments = format!("location={}/segment%05d.ts", dir.display());
    let gst = Command::new("gst-launch-1.0")
        .args([
            "-e",
            "-q",
            "videotestsrc",
            "num-buffers=50",
            "pattern=ball",
            "!",
            "video/x-raw,width=160,height=120,framerate=25/1",
            "!",
            "videoconvert",
            "!",
            "openh264enc",
            "!",
            "h264parse",
            "config-interval=-1",
            "!",
            "mpegtsmux",
            "!",
            "hlssink",
            &segments,
            &format!("playlist-location={}", playlist.display()),
            "target-duration=1",
        ])
        .status_locked();
    if gst.is_ok_and(|status| status.success()) && playlist.is_file() {
        return Ok(playlist);
    }
    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=25:duration=2",
            "-c:v",
            "libopenh264",
            "-b:v",
            "500k",
            "-g",
            "25",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "0",
            &playlist.display().to_string(),
        ])
        .status_locked();
    if ffmpeg.is_ok_and(|status| status.success()) && playlist.is_file() {
        return Ok(playlist);
    }
    Err("neither gst-launch-1.0 nor ffmpeg made the test's HLS stream".to_owned())
}

#[test]
fn a_declared_player_plays_a_live_hls_stream() {
    let dir = std::env::temp_dir().join(format!("tilo-live-hls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the stream");
    let playlist = make_hls(&dir).expect("a tool to make the stream");
    let source = media::Source::for_path(&playlist)
        .to_uri()
        .expect("the playlist's address");
    let mut video = DeclaredVideo::new(VideoPlayerProps {
        title: Some("generated live stream".to_owned()),
        source: Some(source),
        playing: true,
        live: true,
        ..VideoPlayerProps::default()
    });
    assert!(
        video.player().is_live(),
        "a live source is held as live by the player"
    );
    // Playback runs on the engine's own thread: pump frames until the manifest's segments
    // decode a picture, or fail.
    let began = Instant::now();
    loop {
        let (state, path) = ((), ());
        video.sync(&state, &path);
        if video.player().latest_frame().is_some() {
            break;
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the declared player decoded no frame from the HLS stream it named"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
