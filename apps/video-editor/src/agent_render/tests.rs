use serde_json::{Map, Value, json};

use super::*;
use crate::model::{Clip, MediaItem, MediaKind, Track, TrackKind};

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// A project placing one source at `path`, `seconds` long.
fn placing(path: &str, seconds: f64) -> Project {
    let mut project = Project::new("Cut");
    let track = Track {
        id: uuid::Uuid::from_u128(1),
        kind: TrackKind::Video,
        ..Track::default()
    };
    let media = MediaItem {
        id: uuid::Uuid::from_u128(2),
        name: "interview.mp4".into(),
        kind: MediaKind::Video,
        duration: seconds,
        path: path.into(),
        ..MediaItem::default()
    };
    project.clips.push(Clip {
        id: uuid::Uuid::from_u128(3),
        track: track.id,
        media: media.id,
        out_point: seconds,
        ..Clip::default()
    });
    project.tracks.push(track);
    project.media.push(media);
    project
}

fn content(hash_byte: &str) -> String {
    format!("tilo.content:{}", hash_byte.repeat(32))
}

#[test]
fn export_settings_default_to_720p30_and_follow_the_aspect_from_one_side() {
    let aspect = 16.0 / 9.0;
    assert_eq!(
        AgentExportSettings::from_args(&args(json!({})), aspect).unwrap(),
        AgentExportSettings::default()
    );
    let wide = AgentExportSettings::from_args(&args(json!({"width": 1920})), aspect).unwrap();
    assert_eq!((wide.width, wide.height), (1920, 1080));
    let tall = AgentExportSettings::from_args(&args(json!({"height": 1920})), 9.0 / 16.0).unwrap();
    assert_eq!((tall.width, tall.height), (1080, 1920));
    let rate = AgentExportSettings::from_args(&args(json!({"fps": 24.0})), aspect).unwrap();
    assert_eq!(rate.fps, 24);
}

#[test]
fn export_settings_refuse_odd_oversized_or_unknown_requests() {
    let aspect = 16.0 / 9.0;
    for bad in [
        json!({"width": 1281}),
        json!({"width": 3840, "height": 2160}),
        json!({"width": 8, "height": 8}),
        json!({"fps": 25}),
        json!({"format": "webm"}),
        json!({"width": "wide"}),
    ] {
        assert!(
            AgentExportSettings::from_args(&args(bad.clone()), aspect).is_err(),
            "{bad} is refused"
        );
    }
}

#[test]
fn the_editors_render_maps_onto_an_agent_render_at_its_height() {
    let aspect = 16.0 / 9.0;
    let hd = AgentExportSettings::from_render_args(&args(json!({"size": "720p"})), aspect).unwrap();
    assert_eq!((hd.width, hd.height, hd.fps), (1280, 720, 30));
    let full = AgentExportSettings::from_render_args(&args(json!({})), aspect).unwrap();
    assert_eq!((full.width, full.height), (1920, 1080));
    assert!(
        AgentExportSettings::from_render_args(&args(json!({"size": "2160p"})), aspect).is_err()
    );
    assert!(
        AgentExportSettings::from_render_args(&args(json!({"format": "mov"})), aspect).is_err()
    );
}

#[test]
fn the_bitrate_keeps_two_minutes_under_the_kept_size() {
    let biggest = AgentExportSettings {
        width: 1920,
        height: 1080,
        fps: 60,
    };
    let bytes = biggest.video_bitrate() / 8 * 120;
    assert!(bytes < 60 * 1024 * 1024, "{bytes} bytes");
    assert_eq!(AgentExportSettings::default().video_bitrate(), 2_500_000);
}

#[test]
fn a_wait_is_capped_and_defaults_to_a_minute() {
    assert_eq!(
        wait_from_args(&args(json!({}))).unwrap().as_secs(),
        AGENT_DEFAULT_WAIT
    );
    assert_eq!(
        wait_from_args(&args(json!({"wait": 999})))
            .unwrap()
            .as_secs(),
        AGENT_MAX_WAIT
    );
    assert!(wait_from_args(&args(json!({"wait": -1}))).is_err());
}

#[test]
fn a_still_is_on_the_timeline_at_an_even_width() {
    let frame =
        AgentFrameSettings::from_args(&args(json!({"at": 2.5, "width": 320})), 16.0 / 9.0, 10.0)
            .unwrap();
    assert_eq!((frame.width, frame.height), (320, 180));
    assert!(AgentFrameSettings::from_args(&args(json!({"at": 11.0})), 1.0, 10.0).is_err());
    assert!(AgentFrameSettings::from_args(&args(json!({})), 1.0, 10.0).is_err());
    assert!(
        AgentFrameSettings::from_args(&args(json!({"at": 1.0, "width": 321})), 1.0, 10.0).is_err()
    );
}

#[test]
fn an_agent_renders_only_short_timelines_of_room_content() {
    assert!(renderable(&placing(&content("ab"), 10.0)).is_ok());
    let file = renderable(&placing("/Users/you/footage.mp4", 10.0)).unwrap_err();
    assert!(file.contains("import-content"), "{file}");
    let long = renderable(&placing(&content("ab"), 300.0)).unwrap_err();
    assert!(long.contains("at most 120"), "{long}");
    assert!(renderable(&Project::new("Empty")).is_err());
}

#[test]
fn placed_content_names_each_source_once_and_skips_unplaced_ones() {
    let mut project = placing(&content("AB"), 5.0);
    project.media.push(MediaItem {
        id: uuid::Uuid::from_u128(9),
        name: "unused".into(),
        kind: MediaKind::Video,
        path: content("cd"),
        ..MediaItem::default()
    });
    assert_eq!(placed_content(&project), vec!["ab".repeat(32)]);
}

#[test]
fn a_range_narrows_the_render_to_the_timeline() {
    let project = placing(&content("ab"), 10.0);
    let part = ranged(&project, Some(2.0), Some(4.0)).unwrap();
    assert_eq!(part.export_range(), (2.0, 4.0));
    assert!(ranged(&project, Some(12.0), None).is_err());
    assert!(ranged(&project, Some(3.0), Some(1.0)).is_err());
    assert_eq!(ranged(&project, None, None).unwrap(), project);
}

#[test]
fn the_agent_pipeline_encodes_with_openh264_single_threaded_into_mp4() {
    let desc = pipeline_desc(&AgentExportSettings::default(), "fdsink fd=1", false);
    assert!(desc.contains("openh264enc"), "{desc}");
    assert!(desc.contains("multi-thread=1"), "{desc}");
    assert!(
        desc.contains("mp4mux name=tilo_mux faststart=false ! fdsink fd=1"),
        "{desc}"
    );
    assert!(
        desc.contains("width=1280,height=720,framerate=30/1"),
        "{desc}"
    );
}

/// An ISO-BMFF box of `kind` around `body`.
fn mp4_box(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(body);
    out
}

/// A full box header body: version, flags, then two times of the version's
/// width and some trailing bytes.
fn header(version: u8, time: u8) -> Vec<u8> {
    let width = if version == 0 { 4 } else { 8 };
    let mut body = vec![version, 0, 0, 0];
    body.extend(std::iter::repeat_n(time, 2 * width));
    body.extend_from_slice(&[7; 12]);
    body
}

#[test]
fn mp4_clock_times_are_zeroed_in_movie_track_and_media_headers() {
    let mdia = mp4_box(*b"mdia", &mp4_box(*b"mdhd", &header(1, 0xEE)));
    let trak = mp4_box(
        *b"trak",
        &[mp4_box(*b"tkhd", &header(0, 0xDD)), mdia].concat(),
    );
    let moov = mp4_box(
        *b"moov",
        &[mp4_box(*b"mvhd", &header(0, 0xCC)), trak].concat(),
    );
    let mut file = [mp4_box(*b"ftyp", b"isom\0\0\0\0"), moov].concat();
    let before = file.clone();
    assert_eq!(zero_mp4_times(&mut file), 3, "mvhd, tkhd and mdhd");
    assert!(!file.contains(&0xCC) && !file.contains(&0xDD) && !file.contains(&0xEE));
    assert_eq!(file.len(), before.len());
    // The trailing fields are untouched.
    let kept = file.iter().filter(|b| **b == 7).fold(0, |n, _| n + 1);
    assert_eq!(kept, 36);
    assert_eq!(zero_mp4_times(&mut b"no boxes".to_vec()), 0);
}

#[test]
#[serial_test::serial(video_editor_media_env)]
fn content_addressed_sources_resolve_under_the_content_root() {
    let dir = tempfile::tempdir().unwrap();
    let hash = "ab".repeat(32);
    std::fs::write(dir.path().join(&hash), b"bytes").unwrap();
    crate::media::set_content_root(Some(dir.path().to_path_buf()));
    let found = crate::media::resolve(&format!("tilo.content:{}", hash.to_ascii_uppercase()));
    let missing = crate::media::resolve(&content("cd"));
    crate::media::set_content_root(None);
    assert_eq!(found.abs_path, dir.path().join(&hash));
    assert!(found.is_decodable());
    assert!(!missing.is_decodable());
    let unrooted = crate::media::resolve(&content("ab"));
    assert!(!unrooted.is_decodable(), "no root, no bytes");
}
