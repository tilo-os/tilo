//! End-to-end acceptance test for the export: what lands in the file has
//! to be the user's footage.
//!
//! The bug this exists to catch produced a perfectly valid MP4 whose every
//! frame was a synthesized two-band gradient built from a palette table —
//! so a container check (`ftyp` magic, non-zero length) passed on it. This
//! test decodes the exported file back and compares *pixels* with the
//! source, which is the only check that can tell the two apart.
//!
//! It lives in `tests/` rather than beside the export so it exercises the
//! crate exactly as a caller would, through the public API.
#![cfg(feature = "gst")]

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use video_editor::export::{
    Container, ExportSettings, ExportStatus, FrameRate, Resolution, begin_export, output_path,
};
use video_editor::model::{
    MediaItem, MediaKind, Project, TextAlign, TextSource, Track, TrackKind, Transform,
};
use video_editor::param::{Interp, Param};

const GRID_W: usize = 160;
const GRID_H: usize = 90;

/// The bundled demo clip. Absence is a failure, not a skip: this test
/// cannot verify anything without real footage, and a check that quietly
/// passes when it could not look is how the original bug survived.
fn demo_clip() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("QOS_VE_TEST_CLIP") {
        let p = std::path::PathBuf::from(p);
        assert!(p.exists(), "$QOS_VE_TEST_CLIP does not exist: {}", p.display());
        return p;
    }
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..5 {
        let candidate = dir.join(".claude/demo-artifacts/qos-demo-57s.mp4");
        if candidate.exists() {
            return candidate;
        }
        if !dir.pop() {
            break;
        }
    }
    panic!(
        "the export fidelity test needs real footage at \
         .claude/demo-artifacts/qos-demo-57s.mp4, or $QOS_VE_TEST_CLIP pointing at a clip",
    );
}

/// A project holding `path` untrimmed at t=0 for `len` seconds, at the
/// source's own frame size.
///
/// `name` decides the output filename, so each test writes somewhere of
/// its own and the two can run concurrently without an env-var race.
fn project_over(path: &std::path::Path, len: f64, name: &str) -> Project {
    let mut p = Project::new(name);
    p.width = 1920;
    p.height = 1080;
    let mid = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id: mid,
        name: "demo".into(),
        kind: MediaKind::Video,
        duration: 57.0,
        width: 1920,
        height: 1080,
        fps: 30.0,
        palette: 3,
        path: path.to_string_lossy().into_owned(),
        hydration: video_editor::media::Hydration::Local,
        ..Default::default()
    });
    let tid = uuid::Uuid::new_v4();
    p.tracks.push(Track {
        id: tid,
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        muted: false,
        locked: false,
        solo: false,
    });
    let cid = p.add_clip(tid, mid, 0.0).unwrap();
    p.clip_mut(cid).unwrap().in_point = 0.0;
    p.clip_mut(cid).unwrap().out_point = len;
    p
}

/// Decode `path` and return its frame at `at_secs`, scaled to a fixed
/// `GRID_W × GRID_H` RGB grid so two files of different resolutions are
/// directly comparable.
fn decode_frame(path: &std::path::Path, at_secs: f64) -> Vec<[u8; 3]> {
    gst::init().expect("gstreamer init");
    let uri = format!("file://{}", path.display());
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .expect("uridecodebin");
    let convert = gst::ElementFactory::make("videoconvert").build().expect("videoconvert");
    let scale = gst::ElementFactory::make("videoscale").build().expect("videoscale");
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", GRID_W as i32)
                .field("height", GRID_H as i32)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        )
        .sync(false)
        .build();
    pipeline
        .add_many([&src, &convert, &scale, sink.upcast_ref()])
        .expect("assemble");
    gst::Element::link_many([&convert, &scale, sink.upcast_ref()]).expect("link");
    let convert_weak = convert.downgrade();
    src.connect_pad_added(move |_, pad| {
        let is_video = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("video/")))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        if let Some(c) = convert_weak.upgrade() {
            if let Some(target) = c.static_pad("sink") {
                if !target.is_linked() {
                    let _ = pad.link(&target);
                }
            }
        }
    });
    pipeline.set_state(gst::State::Playing).expect("play");

    let target_ms = (at_secs.max(0.0) * 1000.0) as u64;
    let mut out: Option<Vec<[u8; 3]>> = None;
    loop {
        let Ok(sample) = sink.pull_sample() else { break };
        let Some(buffer) = sample.buffer() else { continue };
        let pts = buffer.pts().map(|p| p.mseconds()).unwrap_or(0);
        let Ok(map) = buffer.map_readable() else { continue };
        let want = GRID_W * GRID_H * 4;
        if map.len() >= want {
            out = Some(
                map[..want]
                    .chunks(4)
                    .map(|p| [p[0], p[1], p[2]])
                    .collect(),
            );
        }
        if pts >= target_ms {
            break;
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    out.unwrap_or_else(|| panic!("decoded no frame from {} at {at_secs}s", path.display()))
}

/// Mean horizontal neighbour variation.
///
/// The synthesized fill the export used to encode is built from vertical
/// bands, so it is constant along every row and this collapses toward
/// zero. Real footage never does.
fn horizontal_detail(px: &[[u8; 3]]) -> f64 {
    let mut total = 0.0;
    for y in 0..GRID_H {
        for x in 1..GRID_W {
            let a = px[y * GRID_W + x - 1];
            let b = px[y * GRID_W + x];
            total += (a[0].abs_diff(b[0]) as f64)
                + (a[1].abs_diff(b[1]) as f64)
                + (a[2].abs_diff(b[2]) as f64);
        }
    }
    total / (GRID_W * GRID_H) as f64
}

/// Mean absolute per-channel difference between two frames, 0 = identical.
fn mean_abs_diff(a: &[[u8; 3]], b: &[[u8; 3]]) -> f64 {
    let total: f64 = a
        .iter()
        .zip(b)
        .map(|(p, q)| {
            (p[0].abs_diff(q[0]) as f64) + (p[1].abs_diff(q[1]) as f64) + (p[2].abs_diff(q[2]) as f64)
        })
        .sum();
    total / (a.len() * 3) as f64
}

/// Run a real export and return the file it wrote. The destination comes
/// from the project's own name, so no process-wide state is touched.
fn export(project: &Project) -> std::path::PathBuf {
    let settings = ExportSettings {
        resolution: Resolution::P720,
        container: Container::Mp4,
        frame_rate: FrameRate::F30,
    };
    let expected = output_path(&settings, project);
    match begin_export(project, &settings) {
        ExportStatus::Done { path } => assert_eq!(path, expected.to_string_lossy()),
        other => panic!("export must write a file, got {other:?}"),
    }
    let bytes = std::fs::metadata(&expected).expect("export file exists").len();
    assert!(bytes > 512, "a real encoded file, got {bytes} bytes");
    println!("wrote {} ({bytes} bytes)", expected.display());
    expected
}

/// Clean up an exported artifact, unless the operator asked for the files
/// somewhere specific — then they are there to be looked at, which is the
/// point of running this by hand.
fn tidy(path: &std::path::Path) {
    if std::env::var_os("QOS_VE_EXPORT_DIR").is_none() {
        std::fs::remove_file(path).ok();
    }
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn the_exported_file_carries_the_source_footage() {
    let clip = demo_clip();
    let out = export(&project_over(
        &clip,
        1.0,
        &format!("ve fidelity {}", std::process::id()),
    ));

    let source = decode_frame(&clip, 0.0);
    let exported = decode_frame(&out, 0.0);

    let source_detail = horizontal_detail(&source);
    let export_detail = horizontal_detail(&exported);
    let diff = mean_abs_diff(&source, &exported);
    println!(
        "horizontal detail  source={source_detail:.2}  export={export_detail:.2}\n\
         mean abs difference source vs export = {diff:.2}",
    );

    assert!(
        export_detail > 1.0,
        "the exported frame is flat along every row (detail {export_detail:.2}) — that is \
         the synthesized-gradient signature, not decoded footage",
    );
    assert!(
        export_detail > source_detail * 0.25,
        "the exported frame carries only {:.0}% of the source's horizontal detail \
         ({export_detail:.2} vs {source_detail:.2}) — it is not the real picture",
        export_detail / source_detail * 100.0,
    );
    assert!(
        diff < 60.0,
        "the exported frame does not resemble the source frame (mean abs difference {diff:.2})",
    );

    tidy(&out);
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn the_colour_grade_reaches_the_encoded_pixels() {
    // The grade used to be computed on the clip's palette colour, so it
    // never touched a decoded pixel. Prove it changes the encoded file.
    let clip = demo_clip();
    let pid = std::process::id();
    let neutral = project_over(&clip, 0.6, &format!("ve grade neutral {pid}"));
    let plain = export(&neutral);

    let mut graded = project_over(&clip, 0.6, &format!("ve grade lifted {pid}"));
    let cid = graded.clips[0].id;
    graded.clip_mut(cid).unwrap().filters = video_editor::model::Filters {
        brightness: 0.85,
        contrast: 0.5,
        saturation: 0.5,
    };
    let lifted = export(&graded);

    let mean = |px: &[[u8; 3]]| {
        px.iter()
            .map(|p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64)
            .sum::<f64>()
            / px.len() as f64
    };
    let a = decode_frame(&plain, 0.0);
    let b = decode_frame(&lifted, 0.0);
    let (la, lb) = (mean(&a), mean(&b));
    println!("encoded luma: neutral={la:.1}  brightened={lb:.1}");
    assert!(
        lb > la + 10.0,
        "raising brightness must brighten the ENCODED footage: {la:.1} → {lb:.1}",
    );
    // Still the same picture, just graded — not a different frame.
    assert!(
        horizontal_detail(&b) > horizontal_detail(&a) * 0.5,
        "the brightened export is the same footage with a grade on it",
    );

    tidy(&plain);
    tidy(&lifted);
}

/// A wipe has to reach the encoded file as *geometry*: halfway across,
/// one side of the frame is the incoming clip and the other is still the
/// outgoing one.
///
/// The transition this replaced was a flat full-frame flash that rendered
/// identically for a dissolve, a fade and a wipe. Comparing whole-frame
/// averages cannot tell those apart, so this asserts on the split between
/// the two halves — which is the thing a wipe has and a dissolve does not.
#[test]
#[serial_test::serial(video_editor_gst)]
fn a_wipe_reaches_the_encoded_file_as_geometry() {
    use video_editor::model::{ClipEffect, EffectKind, Transition};

    let clip = demo_clip();
    let pid = std::process::id();
    // Both clips come off one source. The bundled demo is a screen
    // recording that barely moves, so two instants of it are the same
    // picture — invert the incoming clip and the two sides of the wipe
    // become unmistakable.
    let build = |kind: Transition, name: String| {
        let mut p = project_over(&clip, 1.0, &name);
        let track = p.tracks[0].id;
        let media = p.media[0].id;
        let second = p.add_clip(track, media, 1.0).unwrap();
        let c = p.clip_mut(second).unwrap();
        c.in_point = 20.0;
        c.out_point = 21.0;
        c.transition = kind;
        c.effects = vec![ClipEffect { kind: EffectKind::Invert, amount: 1.0 }];
        p
    };

    let wipe = export(&build(Transition::Wipe, format!("ve wipe {pid}")));
    let dissolve = export(&build(
        Transition::CrossDissolve,
        format!("ve dissolve {pid}"),
    ));

    // Halfway through the transition on the incoming clip, which starts at
    // 1.0s on the timeline.
    let at = 1.0 + video_editor::preview::TRANSITION_SECS * 0.5;
    let luma = |p: [u8; 3]| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
    let half_split = |px: &[[u8; 3]]| {
        let mean_of = |cols: std::ops::Range<usize>| {
            let mut sum = 0.0;
            let mut n = 0.0;
            for row in 0..GRID_H {
                for col in cols.clone() {
                    sum += luma(px[row * GRID_W + col]);
                    n += 1.0;
                }
            }
            sum / n
        };
        // Skip the feathered middle so the measurement is of the two sides.
        (mean_of(0..GRID_W / 3), mean_of(GRID_W * 2 / 3..GRID_W))
    };

    let (wl, wr) = half_split(&decode_frame(&wipe, at));
    let (dl, dr) = half_split(&decode_frame(&dissolve, at));
    println!("wipe halves L={wl:.1} R={wr:.1}   dissolve halves L={dl:.1} R={dr:.1}");

    assert!(
        (wl - wr).abs() > (dl - dr).abs() + 10.0,
        "the encoded wipe must split the frame left/right ({:.1}) far more than an even \
         dissolve does ({:.1})",
        (wl - wr).abs(),
        (dl - dr).abs(),
    );

    tidy(&wipe);
    tidy(&dissolve);
}


// ---------------------------------------------------------------------------
// Geometry, titles and time-remap in the encoded file.
//
// These need no source footage: a title's picture is generated from the
// document, so the whole pipeline — compose, encode, mux, decode back — can
// be exercised against a picture whose every pixel is known in advance.
// That is a stronger check than comparing against a decoded source, because
// there is nothing to blame a mismatch on but the export.
// ---------------------------------------------------------------------------

/// A project holding one title over `len` seconds, and the ids to edit it.
fn title_project(name: &str, words: &str) -> (Project, uuid::Uuid, uuid::Uuid) {
    let mut p = Project::new(name);
    p.width = 1280;
    p.height = 720;
    let mid = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id: mid,
        name: "Title".into(),
        kind: MediaKind::Text,
        duration: 4.0,
        text: Some(TextSource {
            content: words.into(),
            // Big enough that a 160x90 decode grid still resolves it.
            size: 0.55,
            color: [1.0, 1.0, 1.0, 1.0],
            align: TextAlign::Center,
            bold: true,
            ..TextSource::default()
        }),
        ..Default::default()
    });
    let tid = uuid::Uuid::new_v4();
    p.tracks.push(Track {
        id: tid,
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        ..Default::default()
    });
    let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
    p.clip_mut(cid).expect("clip").out_point = 4.0;
    (p, mid, cid)
}

/// How much of the decoded grid is bright, as a fraction. A white title on
/// black is exactly this measurement.
fn bright_fraction(px: &[[u8; 3]]) -> f64 {
    let lit = px.iter().filter(|q| q[0] > 170 && q[1] > 170 && q[2] > 170).count();
    lit as f64 / px.len() as f64
}

/// The centre of mass of the bright pixels, in grid coordinates, or `None`
/// when nothing is lit.
fn bright_centre(px: &[[u8; 3]]) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for (i, q) in px.iter().enumerate() {
        if q[0] > 170 && q[1] > 170 && q[2] > 170 {
            sx += (i % GRID_W) as f64;
            sy += (i / GRID_W) as f64;
            n += 1.0;
        }
    }
    (n > 0.0).then(|| (sx / n, sy / n))
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn a_title_reaches_the_encoded_file_as_real_glyphs() {
    let (p, _, _) = title_project("title_export", "HELLO");
    let path = export(&p);
    let frame = decode_frame(&path, 1.0);

    let lit = bright_fraction(&frame);
    assert!(
        lit > 0.01,
        "the encoded file carries the title's glyphs, not black: {:.4} of the frame is lit",
        lit,
    );
    assert!(
        lit < 0.5,
        "and it is glyphs rather than a white card: {:.4} lit",
        lit,
    );
    // Centred: the bright mass sits near the middle of the frame.
    let (cx, cy) = bright_centre(&frame).expect("something is lit");
    assert!(
        (cx - GRID_W as f64 / 2.0).abs() < GRID_W as f64 * 0.15,
        "a centred title encodes centred horizontally, got x={cx:.1}",
    );
    assert!(
        (cy - GRID_H as f64 / 2.0).abs() < GRID_H as f64 * 0.15,
        "and vertically, got y={cy:.1}",
    );
    tidy(&path);
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn a_transform_moves_the_picture_in_the_encoded_file() {
    // The same title, pushed into the lower-right quadrant. If the export
    // ignored geometry the two files would be identical.
    let (mut p, _, cid) = title_project("title_moved_export", "HELLO");
    p.clip_mut(cid).expect("clip").transform = Transform {
        scale_x: 0.45,
        scale_y: 0.45,
        pos_x: 0.25,
        pos_y: 0.25,
        ..Transform::default()
    };
    let path = export(&p);
    let frame = decode_frame(&path, 1.0);
    let (cx, cy) = bright_centre(&frame).expect("the title is still on screen");
    assert!(
        cx > GRID_W as f64 * 0.6,
        "the transform moved the title right in the encoded file, got x={cx:.1}",
    );
    assert!(
        cy > GRID_H as f64 * 0.6,
        "and down, got y={cy:.1}",
    );
    tidy(&path);
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn a_keyframed_move_is_at_a_different_place_in_each_encoded_second() {
    // An animated parameter has to be sampled per frame during the encode,
    // not once for the whole file. Two instants of the same clip must
    // therefore differ — and differ in the direction the curve runs.
    let (mut p, _, cid) = title_project("title_animated_export", "HELLO");
    {
        let c = p.clip_mut(cid).expect("clip");
        c.transform = Transform { scale_x: 0.4, scale_y: 0.4, ..Transform::default() };
        c.set_key_at(Param::PosY, 0.0, -0.3, Interp::Linear);
        c.set_key_at(Param::PosY, 3.0, 0.3, Interp::Linear);
    }
    let path = export(&p);
    let (_, early) = {
        let f = decode_frame(&path, 0.2);
        (bright_fraction(&f), bright_centre(&f).expect("lit at the head"))
    };
    let (_, late) = {
        let f = decode_frame(&path, 2.8);
        (bright_fraction(&f), bright_centre(&f).expect("lit at the tail"))
    };
    assert!(
        late.1 > early.1 + GRID_H as f64 * 0.2,
        "the keyframed move travels down the encoded file: y {:.1} → {:.1}",
        early.1,
        late.1,
    );
    tidy(&path);
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn speed_shortens_the_encoded_file() {
    // Timeline length is the source window divided by the rate, and the
    // export encodes the timeline — so a sped-up clip must produce a
    // shorter file, not the same file played faster.
    let (mut p, _, cid) = title_project("title_speed_export", "HELLO");
    let normal = p.duration();
    p.clip_mut(cid).expect("clip").set_speed(2.0, false);
    assert!(
        (p.duration() - normal / 2.0).abs() < 1e-9,
        "the timeline halves: {normal} → {}",
        p.duration(),
    );
    let path = export(&p);
    // Past the end of the sped-up timeline there is nothing to decode.
    let frame = decode_frame(&path, 0.5);
    assert!(
        bright_fraction(&frame) > 0.005,
        "the sped-up file still carries the title inside its own length",
    );
    tidy(&path);
}
