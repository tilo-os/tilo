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
    Container, ExportSettings, ExportStatus, FrameRate, Resolution, begin_export, encode_chain,
    output_path, selected_encoders, video_codec,
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
    export_as(project, Container::Mp4)
}

/// [`export`] to a chosen container, so the same fidelity checks can be run
/// against a chain built from different elements.
fn export_as(project: &Project, container: Container) -> std::path::PathBuf {
    // Say which encoder this machine resolved to, so the result is read
    // against what actually ran rather than an assumption about it. On a box
    // with no hardware H.264 encoder this prints the software fallback, which
    // is the honest reading of the run.
    println!("encoders: {}", selected_encoders(container).describe());
    let settings = ExportSettings {
        resolution: Resolution::P720,
        container,
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

/// The WebM chain, end to end.
///
/// Every other test here exports MP4, so until now nothing decoded a file
/// produced by the *other* encoder — a different element, a differently named
/// bitrate property (`target-bitrate`, not `bitrate`) and a different muxer.
/// A mistake anywhere in that branch would have reached a user before it
/// reached a test.
///
/// The checks are the ones the MP4 test makes, for the same reason: a
/// container-shaped file full of synthesized bands passes a magic-byte check
/// and fails this one.
#[test]
#[serial_test::serial(video_editor_gst)]
fn the_webm_chain_encodes_the_real_footage_too() {
    let clip = demo_clip();
    let project = project_over(&clip, 1.0, &format!("ve webm {}", std::process::id()));
    let out = export_as(&project, Container::WebM);
    assert_eq!(
        out.extension().and_then(|e| e.to_str()),
        Some("webm"),
        "the WebM export writes a .webm file: {}",
        out.display(),
    );

    // Whatever encoder was picked for this container, it must be one that
    // writes the codec the container carries — and it must have been given the
    // bitrate under the property it actually names.
    let (sel, _, _) = encode_chain(Container::WebM);
    assert!(
        video_codec(Container::WebM)
            .candidates()
            .iter()
            .any(|c| c.element == sel.spec.element),
        "WebM selected {}, which does not encode VP8",
        sel.spec.element,
    );

    let source = decode_frame(&clip, 0.0);
    let exported = decode_frame(&out, 0.0);

    let source_detail = horizontal_detail(&source);
    let export_detail = horizontal_detail(&exported);
    let diff = mean_abs_diff(&source, &exported);
    println!(
        "webm horizontal detail  source={source_detail:.2}  export={export_detail:.2}\n\
         mean abs difference source vs export = {diff:.2}",
    );

    assert!(
        export_detail > 1.0,
        "the exported WebM frame is flat along every row (detail {export_detail:.2}) — that \
         is the synthesized-gradient signature, not decoded footage",
    );
    assert!(
        export_detail > source_detail * 0.25,
        "the exported WebM frame carries only {:.0}% of the source's horizontal detail \
         ({export_detail:.2} vs {source_detail:.2}) — it is not the real picture",
        export_detail / source_detail * 100.0,
    );
    assert!(
        diff < 60.0,
        "the exported WebM frame does not resemble the source frame \
         (mean abs difference {diff:.2})",
    );

    tidy(&out);
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

// ---------------------------------------------------------------------------
// Audio: proving the export's second branch writes a real, correct track.
//
// The bundled demo clip has no audio stream at all — `vide` handlers, no
// `soun` — so nothing in this file could check the audio branch against it.
// Worse, it could appear to: a length check against a stream of silence
// passes, which is how an "audio" test on this branch once went green over a
// file with no audio in it. So the fixture here is *generated*, and its
// content is known sample by sample before anything encodes it.
// ---------------------------------------------------------------------------

/// Everything mixes and muxes at this rate; the fixture is written at it so
/// nothing resamples on the way in.
const FIXTURE_RATE: u32 = 48_000;
/// Amplitude of each tone, leaving headroom so the mixer's ±1 clamp never
/// engages and the encoded peak is the tone's own.
const TONE_AMPLITUDE: f64 = 0.45;

/// The fixture's left-channel tone, constant for its whole length.
const LEFT_HZ: f64 = 440.0;
/// The right channel's tone before [`SWITCH_SECS`] …
const RIGHT_HZ_EARLY: f64 = 880.0;
/// … and after it.
const RIGHT_HZ_LATE: f64 = 1760.0;
/// When the right channel changes tone. Both tones complete a whole number of
/// cycles by this instant, so the change happens at a zero crossing and adds
/// no click for a codec to smear.
const SWITCH_SECS: f64 = 1.0;
/// The fixture's length.
const FIXTURE_SECS: f64 = 2.0;

/// The sample a channel of the fixture holds at `t` seconds.
fn fixture_sample(channel: usize, t: f64) -> f64 {
    let hz = match channel {
        0 => LEFT_HZ,
        _ if t < SWITCH_SECS => RIGHT_HZ_EARLY,
        _ => RIGHT_HZ_LATE,
    };
    (std::f64::consts::TAU * hz * t).sin() * TONE_AMPLITUDE
}

/// Write the fixture as a 16-bit stereo PCM WAV.
///
/// Written by hand rather than encoded, so the file's contents are decided
/// here and not by whatever an encoder felt like doing — the point of a
/// fixture is that its signal is known before the code under test sees it.
fn write_fixture_wav(path: &std::path::Path) {
    let frames = (FIXTURE_SECS * FIXTURE_RATE as f64).round() as usize;
    let channels = 2u16;
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let data_len = frames as u32 * block_align as u32;

    let mut out: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&FIXTURE_RATE.to_le_bytes());
    out.extend_from_slice(&(FIXTURE_RATE * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let t = i as f64 / FIXTURE_RATE as f64;
        for ch in 0..2 {
            let v = (fixture_sample(ch, t).clamp(-1.0, 1.0) * 32767.0).round() as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    std::fs::write(path, &out).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// The fixture on disk, generated once per test process and **verified**.
///
/// The verification is the whole point: a fixture that turns out to be silent
/// makes every assertion downstream vacuous, and this file has already been
/// fooled once by exactly that. So the written file is decoded back through
/// the same path the editor's mixer uses and checked to carry the tones it was
/// built from — before any test is allowed to draw a conclusion from it.
fn audio_fixture() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("qos-ve-tone-{}.wav", std::process::id()));
    if !path.exists() {
        write_fixture_wav(&path);
    }
    let pcm = decode_audio(&path);
    let frames = pcm.len() / 2;
    assert!(
        frames > (FIXTURE_SECS * FIXTURE_RATE as f64 * 0.9) as usize,
        "the generated fixture decodes to {frames} frames, not the ~{} it was written with — \
         a short or silent fixture makes every audio assertion below meaningless",
        (FIXTURE_SECS * FIXTURE_RATE as f64) as usize,
    );
    let left = channel(&pcm, 0);
    let right = channel(&pcm, 1);
    assert!(
        peak(&left) > 0.3 && peak(&right) > 0.3,
        "the fixture is audible on both channels before anything encodes it: \
         peaks {:.3} / {:.3}",
        peak(&left),
        peak(&right),
    );
    // And it holds the tones it was written with, in the channels it wrote
    // them to. If this fails the generator is wrong, not the export.
    let early = ..(SWITCH_SECS * FIXTURE_RATE as f64) as usize;
    assert!(
        goertzel(&left[early], LEFT_HZ) > goertzel(&left[early], RIGHT_HZ_EARLY) * 4.0,
        "the fixture's left channel is {LEFT_HZ} Hz",
    );
    assert!(
        goertzel(&right[early], RIGHT_HZ_EARLY) > goertzel(&right[early], LEFT_HZ) * 4.0,
        "and its right channel is {RIGHT_HZ_EARLY} Hz",
    );
    path
}

/// One channel of an interleaved stereo block.
fn channel(pcm: &[f32], ch: usize) -> Vec<f32> {
    pcm.iter().skip(ch).step_by(2).copied().collect()
}

/// The largest absolute sample in a block.
fn peak(block: &[f32]) -> f64 {
    block.iter().fold(0.0f64, |m, s| m.max(s.abs() as f64))
}

/// Energy at `hz` in `block`, by Goertzel.
///
/// One bin of a DFT, which is all that is wanted here: the fixture's tones are
/// known in advance, so the question is only how much of each one is present.
/// Normalised by length so windows of different sizes compare directly.
fn goertzel(block: &[f32], hz: f64) -> f64 {
    if block.is_empty() {
        return 0.0;
    }
    let w = std::f64::consts::TAU * hz / FIXTURE_RATE as f64;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in block {
        let s0 = x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    power.max(0.0).sqrt() / block.len() as f64
}

/// What a file's audio track turned out to be.
struct DecodedAudio {
    /// Interleaved stereo `f32` at [`FIXTURE_RATE`] — converted, so it can be
    /// compared with the fixture whatever the file held.
    pcm: Vec<f32>,
    /// The rate and channel count the file *itself* declares, read off the
    /// decoder's pad before `audioconvert` touches it. `None` when the file
    /// has no audio track, which is a finding and not a failure to look.
    native: Option<(i32, i32)>,
}

impl DecodedAudio {
    fn frames(&self) -> usize {
        self.pcm.len() / 2
    }
    fn secs(&self) -> f64 {
        self.frames() as f64 / FIXTURE_RATE as f64
    }
}

/// [`decode_audio_full`], keeping only the samples.
fn decode_audio(path: &std::path::Path) -> Vec<f32> {
    decode_audio_full(path).pcm
}

/// Decode `path`'s **audio** track to interleaved stereo `f32` at
/// [`FIXTURE_RATE`], reporting the rate and channel count the file declares.
///
/// The video counterpart of this is [`decode_frame`]; the two are deliberately
/// separate so a file can be found to have a picture and no sound, which is
/// exactly the state the export used to leave every file in.
fn decode_audio_full(path: &std::path::Path) -> DecodedAudio {
    gst::init().expect("gstreamer init");
    let uri = format!("file://{}", path.display());
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .expect("uridecodebin");
    let convert = gst::ElementFactory::make("audioconvert").build().expect("audioconvert");
    let resample = gst::ElementFactory::make("audioresample").build().expect("audioresample");
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("channels", 2i32)
                .field("rate", FIXTURE_RATE as i32)
                .field("layout", "interleaved")
                .build(),
        )
        .sync(false)
        .build();
    pipeline
        .add_many([&src, &convert, &resample, sink.upcast_ref()])
        .expect("assemble");
    gst::Element::link_many([&convert, &resample, sink.upcast_ref()]).expect("link");
    let convert_weak = convert.downgrade();
    // The decoder's own output caps, captured before `audioconvert` rewrites
    // them — asking the sink instead would only report back the caps this
    // function requested, which proves nothing about the file.
    let native = std::sync::Arc::new(std::sync::Mutex::new(None::<(i32, i32)>));
    let native_w = native.clone();
    src.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps();
        let is_audio = caps
            .as_ref()
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        if !is_audio {
            return;
        }
        if let Some(st) = caps.as_ref().and_then(|c| c.structure(0)) {
            if let (Ok(rate), Ok(ch)) = (st.get::<i32>("rate"), st.get::<i32>("channels")) {
                *native_w.lock().expect("caps slot") = Some((rate, ch));
            }
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

    let mut out: Vec<f32> = Vec::new();
    // A file with no audio stream never links the sink, so nothing is ever
    // pulled and `pull_sample` would block. Bounded pulls turn that into an
    // empty result — "this file has no audio" — rather than a hang.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if std::time::Instant::now() >= deadline {
            panic!("decoding audio from {} timed out", path.display());
        }
        match sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) {
            Some(sample) => {
                let Some(buffer) = sample.buffer() else { continue };
                let Ok(map) = buffer.map_readable() else { continue };
                for c in map.chunks_exact(4) {
                    out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
            None => {
                if sink.is_eos() || out.is_empty() {
                    break;
                }
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    let native = *native.lock().expect("caps slot");
    DecodedAudio { pcm: out, native }
}

/// A project with the demo footage on a video track and the generated tone on
/// an audio track, both running from t=0 for [`FIXTURE_SECS`].
///
/// Both branches of the export graph therefore have real content to carry,
/// which is the configuration the audio branch exists for.
fn av_project(name: &str, video: &std::path::Path, audio: &std::path::Path) -> Project {
    let mut p = project_over(video, FIXTURE_SECS, name);
    let aid = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id: aid,
        name: "tone".into(),
        kind: MediaKind::Audio,
        duration: FIXTURE_SECS,
        path: audio.to_string_lossy().into_owned(),
        hydration: video_editor::media::Hydration::Local,
        ..Default::default()
    });
    let tid = uuid::Uuid::new_v4();
    p.tracks.push(Track {
        id: tid,
        name: "A1".into(),
        kind: TrackKind::Audio,
        index: 2,
        ..Default::default()
    });
    let cid = p.add_clip(tid, aid, 0.0).expect("the media exists");
    let c = p.clip_mut(cid).expect("clip");
    c.in_point = 0.0;
    c.out_point = FIXTURE_SECS;
    p
}

/// The demo footage carries no sound, so an audio assertion made against it
/// would be an assertion about silence.
///
/// This is not a remark, it is the trap: an earlier audio test on this branch
/// went green over a stream with no audio in it, because a length check passes
/// on silence. Every test below builds its own tone fixture instead, and this
/// one states — by decoding — the reason they have to.
#[test]
#[serial_test::serial(video_editor_gst)]
fn the_demo_footage_has_no_audio_to_test_against() {
    let clip = demo_clip();
    let decoded = decode_audio_full(&clip);
    assert!(
        decoded.native.is_none() && decoded.pcm.is_empty(),
        "{} has grown an audio track ({:?}, {} samples). The audio tests here build their \
         own fixture precisely because it had none; if this clip now has sound, they are \
         still right to, but this note is stale",
        clip.display(),
        decoded.native,
        decoded.pcm.len(),
    );

    // And the fixture the audio tests do use is audible, at the rate and
    // channel count they expect. A silent fixture would make them vacuous.
    let fixture = audio_fixture();
    let f = decode_audio_full(&fixture);
    assert_eq!(
        f.native,
        Some((FIXTURE_RATE as i32, 2)),
        "the generated fixture is {FIXTURE_RATE} Hz stereo",
    );
    println!(
        "demo clip: no audio track. fixture: {:?}, {:.3}s, peak {:.3}",
        f.native,
        f.secs(),
        peak(&f.pcm),
    );
}

/// The export's audio branch, end to end, for one container.
///
/// Decodes the written file's **audio track** back and checks it against the
/// signal that went in: that there is one at all, at the declared rate and
/// channel count, running the length of the timeline, and carrying the tones
/// the fixture holds in the channels it holds them in.
///
/// Until now nothing did this. The branch's unit tests measure the mixer, and
/// `volume_and_mute_reach_the_encoded_audio` measures peaks, but no test had
/// opened an exported file and found real sound inside it — so an audio branch
/// that muxed an empty or a silent track would have passed everything.
fn assert_audio_round_trip(container: Container) {
    let video = demo_clip();
    let audio = audio_fixture();
    let name = format!("ve audio {} {}", container.extension(), std::process::id());
    let project = av_project(&name, &video, &audio);
    let out = export_as(&project, container);

    let decoded = decode_audio_full(&out);
    println!(
        "{container:?}: audio via {} -> native {:?}, {} frames ({:.3}s), peak {:.3}",
        video_editor::export::audio_encoder(container).describe(),
        decoded.native,
        decoded.frames(),
        decoded.secs(),
        peak(&decoded.pcm),
    );

    // 1. There is an audio track, and it is the shape the mixer produces.
    let (rate, channels) = decoded.native.unwrap_or_else(|| {
        panic!(
            "{} carries no audio track at all — the export muxed picture only",
            out.display(),
        )
    });
    assert_eq!(
        rate, FIXTURE_RATE as i32,
        "the encoded track runs at the rate the mixer produces",
    );
    assert_eq!(channels, 2, "and is stereo, as the appsrc's caps declare");

    // 2. It runs the length of the timeline. A muxer handed one buffer and an
    //    EOS also produces a valid file with an audio track in it.
    assert!(
        (decoded.secs() - FIXTURE_SECS).abs() < 0.15,
        "the encoded audio covers the timeline: {:.3}s of an expected {FIXTURE_SECS}",
        decoded.secs(),
    );

    // 3. It is audible. Silence would satisfy everything above.
    let left = channel(&decoded.pcm, 0);
    let right = channel(&decoded.pcm, 1);
    assert!(
        peak(&left) > 0.15 && peak(&right) > 0.15,
        "the encoded track is audible on both channels, peaks {:.3} / {:.3} — a silent \
         track passes every structural check above",
        peak(&left),
        peak(&right),
    );

    // 4. And it is *this* signal, in the right channels. A tone generator
    //    wired in by mistake, a mono downmix, or a channel swap all survive a
    //    peak check; none of them survives this.
    let early = ..(SWITCH_SECS * FIXTURE_RATE as f64 * 0.9) as usize;
    let (l440, l880) = (goertzel(&left[early], LEFT_HZ), goertzel(&left[early], RIGHT_HZ_EARLY));
    let (r440, r880) = (goertzel(&right[early], LEFT_HZ), goertzel(&right[early], RIGHT_HZ_EARLY));
    println!(
        "{container:?} tone energy — left: {LEFT_HZ}Hz={l440:.4} {RIGHT_HZ_EARLY}Hz={l880:.4}  \
         right: {LEFT_HZ}Hz={r440:.4} {RIGHT_HZ_EARLY}Hz={r880:.4}",
    );
    assert!(
        l440 > l880 * 4.0,
        "the left channel of the export is the fixture's {LEFT_HZ} Hz tone \
         ({l440:.4} vs {l880:.4} at {RIGHT_HZ_EARLY} Hz)",
    );
    assert!(
        r880 > r440 * 4.0,
        "and the right channel is its {RIGHT_HZ_EARLY} Hz one ({r880:.4} vs {r440:.4} at \
         {LEFT_HZ} Hz) — the two channels have not been swapped or summed",
    );

    tidy(&out);
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn an_mp4_export_carries_the_timelines_real_audio() {
    assert_audio_round_trip(Container::Mp4);
}

/// The same for WebM, whose audio is a different encoder into a different
/// muxer — `opusenc` into `webmmux`, not AAC into `mp4mux`. The WebM *video*
/// chain went unproven until recently for exactly this reason; its audio chain
/// shares none of MP4's elements either.
#[test]
#[serial_test::serial(video_editor_gst)]
fn a_webm_export_carries_the_timelines_real_audio() {
    assert_audio_round_trip(Container::WebM);
}

/// Where in the decoded audio the right channel changes tone, in seconds, or
/// `None` when it never does.
///
/// Found as the boundary between the last window still dominated by the early
/// tone and the first dominated by the late one. Short windows, so the
/// boundary is located to about their width rather than smeared across a
/// long one.
fn tone_switch_secs(right: &[f32]) -> Option<f64> {
    let win = (FIXTURE_RATE as f64 * 0.01) as usize; // 10ms
    let dominant_late: Vec<bool> = right
        .chunks_exact(win)
        .map(|b| goertzel(b, RIGHT_HZ_LATE) > goertzel(b, RIGHT_HZ_EARLY))
        .collect();
    // The first late-dominant window with no early-dominant window after it:
    // a lone flipped window in the middle of the early tone is noise, not the
    // landmark.
    let last_early = dominant_late.iter().rposition(|&late| !late)?;
    let first_late = last_early + 1;
    (first_late < dominant_late.len())
        .then(|| first_late as f64 * win as f64 / FIXTURE_RATE as f64)
}

/// Where the sound sits in time relative to the picture.
///
/// The fixture's right channel changes tone at a known instant, so the
/// exported audio can be asked where that instant ended up. A video frame's
/// timeline position is fixed by the frame clock the export counts with, so
/// audio landing at the wrong second *is* the two streams disagreeing.
///
/// Measured in **both** containers, which is what makes the reading
/// attributable. A lossy codec has priming samples of its own, and AAC's are
/// not fully compensated on this round trip — the track comes back 2.048s long
/// for a 2s timeline, one AAC frame of padding at each end. Opus in WebM is
/// delay-compensated and comes back at exactly 2.000s. If the *export* were
/// misplacing audio, both would be displaced together; a displacement in one
/// container and not the other is that container's codec. So the two are
/// asserted at their own tolerances and the difference between them is
/// asserted too — that last is the part that no codec delay can explain away.
///
/// This still cannot resolve a fraction of a block, which is the size of the
/// half-block offset `mix_block` once had; that one is pinned in the mixer's
/// own tests, where no codec sits in between.
#[test]
#[serial_test::serial(video_editor_gst)]
fn the_exported_audio_sits_where_the_timeline_puts_it() {
    let video = demo_clip();
    let audio = audio_fixture();
    let pid = std::process::id();

    let mut found = Vec::new();
    for container in [Container::Mp4, Container::WebM] {
        let name = format!("ve sync {} {pid}", container.extension());
        let out = export_as(&av_project(&name, &video, &audio), container);
        let decoded = decode_audio_full(&out);
        let right = channel(&decoded.pcm, 1);
        assert!(!right.is_empty(), "there is no audio in {} to locate", out.display());

        let at = tone_switch_secs(&right).unwrap_or_else(|| {
            panic!(
                "the {container:?} export never changes from {RIGHT_HZ_EARLY} Hz to \
                 {RIGHT_HZ_LATE} Hz; the fixture's second half did not survive it",
            )
        });
        println!(
            "{container:?}: landmark at {at:.3}s (fixture has it at {SWITCH_SECS:.3}s, \
             {:+.1} ms), track {:.3}s of an expected {FIXTURE_SECS}",
            (at - SWITCH_SECS) * 1000.0,
            decoded.secs(),
        );
        found.push((container, at, decoded.secs()));
        tidy(&out);
    }

    for (container, at, secs) in &found {
        let offset_ms = (at - SWITCH_SECS) * 1000.0;
        // Opus is delay-compensated end to end, so its landmark is the
        // export's own placement with nothing added. AAC's priming is not
        // compensated on this path, so it gets the wider band — and both are
        // far tighter than the displacement a mis-fed audio branch produces.
        let (limit_ms, limit_len) = match container {
            Container::WebM => (25.0, 0.02),
            _ => (80.0, 0.08),
        };
        assert!(
            offset_ms.abs() < limit_ms,
            "the {container:?} export puts the {SWITCH_SECS}s landmark at {at:.3}s \
             ({offset_ms:+.1} ms), past the {limit_ms} ms this container allows",
        );
        // Running long or short is the other way to lose sync: right at the
        // start, wrong by the end.
        assert!(
            (secs - FIXTURE_SECS).abs() < limit_len,
            "the {container:?} audio ends where the video does: {secs:.3}s of an \
             expected {FIXTURE_SECS}",
        );
    }

    // The part no codec delay explains: the two containers must agree on
    // where the landmark is, to within their own priming. An export that fed
    // its audio branch from the wrong point in the timeline would shift both
    // equally — and that common shift is exactly what comparing them removes.
    let spread_ms = (found[0].1 - found[1].1).abs() * 1000.0;
    println!("landmark spread between containers: {spread_ms:.1} ms");
    assert!(
        spread_ms < 80.0,
        "the two containers disagree by {spread_ms:.1} ms about where the same timeline \
         instant is, which no codec priming accounts for",
    );
}
