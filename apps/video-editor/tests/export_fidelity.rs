#![allow(
    clippy::arithmetic_side_effects,
    clippy::assigning_clones,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cloned_instead_of_copied,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::explicit_iter_loop,
    clippy::field_reassign_with_default,
    clippy::float_cmp,
    clippy::format_push_string,
    clippy::uninlined_format_args,
    clippy::implicit_clone,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::manual_contains,
    clippy::map_unwrap_or,
    clippy::manual_assert,
    clippy::needless_borrow,
    clippy::panic,
    clippy::redundant_clone,
    clippy::redundant_closure_for_method_calls,
    clippy::single_match,
    clippy::unnecessary_trailing_comma,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::unwrap_used,
    reason = "pixel-fidelity fixture uses checked test data and direct assertion failures"
)]

//! End-to-end acceptance test for the export: what lands in the file has
//! to be the user's footage.
//!
//! The test decodes the exported file and compares pixels with the source;
//! container checks alone cannot verify footage fidelity.
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

mod generated_clip {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/generated_clip.rs"
    ));
}

/// A deterministic, genuinely encoded moving-picture source. Every test here
/// first asks `generated_clip::media_stack()`: without the encoders a test
/// reports that it did not run, and with `TILO_REQUIRE_MEDIA=1` (CI) it
/// fails, so export fidelity is never claimed without inspecting footage.
fn demo_clip() -> std::path::PathBuf {
    generated_clip::encoded_video()
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
        duration: generated_clip::DURATION_SECS,
        width: generated_clip::WIDTH,
        height: generated_clip::HEIGHT,
        fps: f64::from(generated_clip::FPS),
        palette: 3,
        path: path.to_string_lossy().into_owned(),
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
        gain_db: 0.0,
        pan: 0.0,
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
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .expect("videoconvert");
    let scale = gst::ElementFactory::make("videoscale")
        .build()
        .expect("videoscale");
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
        let Ok(sample) = sink.pull_sample() else {
            break;
        };
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        let pts = buffer.pts().map(|p| p.mseconds()).unwrap_or(0);
        let Ok(map) = buffer.map_readable() else {
            continue;
        };
        let want = GRID_W * GRID_H * 4;
        if map.len() >= want {
            out = Some(map[..want].chunks(4).map(|p| [p[0], p[1], p[2]]).collect());
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
            (p[0].abs_diff(q[0]) as f64)
                + (p[1].abs_diff(q[1]) as f64)
                + (p[2].abs_diff(q[2]) as f64)
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
    let bytes = std::fs::metadata(&expected)
        .expect("export file exists")
        .len();
    assert!(bytes > 512, "a real encoded file, got {bytes} bytes");
    println!("wrote {} ({bytes} bytes)", expected.display());
    expected
}

/// Clean up an exported artifact, unless the operator asked for the files
/// somewhere specific — then they are there to be looked at, which is the
/// point of running this by hand.
fn tidy(path: &std::path::Path) {
    if std::env::var_os("TILO_VE_EXPORT_DIR").is_none() {
        std::fs::remove_file(path).ok();
    }
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn exported_files_carry_the_real_source_footage_and_grade() {
    // Case `the_webm_chain_encodes_the_real_footage_too`:
    // The WebM chain, end to end.
    //
    // Every other test here exports MP4, so until now nothing decoded a file
    // produced by the *other* encoder — a different element, a differently named
    // bitrate property (`target-bitrate`, not `bitrate`) and a different muxer.
    // A mistake anywhere in that branch would have reached a user before it
    // reached a test.
    //
    // The checks are the ones the MP4 test makes, for the same reason: a
    // container-shaped file full of synthesized bands passes a magic-byte check
    // and fails this one.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
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
    // Case `the_exported_file_carries_the_source_footage`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
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
    // Case `the_colour_grade_reaches_the_encoded_pixels`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
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
    // Case `a_relative_media_path_is_resolved_or_refused_never_stood_in_for`:
    // A project whose media path is relative — what a hand-built project or a
    // `.qvp` copied next to its footage looks like — must either find the
    // footage or refuse. What it must never do is encode the compositor's
    // stand-in fill and report `Done`: that produced a valid MP4 of a flat
    // gradient, 14 KB for sixty frames of 1080p, indistinguishable from a
    // successful render to everything except a decoder.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::export::{begin_export, output_path};

        let clip = demo_clip();
        let dir = clip
            .parent()
            .expect("the clip has a directory")
            .to_path_buf();
        let file = clip
            .file_name()
            .expect("the clip has a name")
            .to_string_lossy()
            .into_owned();

        // 1. No anchor, and nothing of that name in the working directory: the
        //    source cannot be read, so the export refuses by name.
        video_editor::media::set_project_anchor(None);
        let pid = std::process::id();
        let mut lost = project_over(&clip, 0.5, &format!("ve relative lost {pid}"));
        lost.media[0].path = format!("tilo-not-here-{pid}/{file}");
        let settings = ExportSettings {
            resolution: Resolution::P720,
            container: Container::Mp4,
            frame_rate: FrameRate::F30,
        };
        let lost_out = output_path(&settings, &lost);
        std::fs::remove_file(&lost_out).ok();
        match begin_export(&lost, &settings) {
            ExportStatus::Failed { reason } => {
                println!("refused: {reason}");
                assert!(
                    reason.contains("demo"),
                    "the refusal names the source: {reason:?}"
                );
            }
            other => panic!("an unresolvable source must refuse, got {other:?}"),
        }
        assert!(
            !lost_out.exists(),
            "and leaves no file behind that looks like a render: {}",
            lost_out.display(),
        );

        // 2. Anchored at the directory the project file would live in, the same
        //    relative path finds the footage — and the file carries it.
        video_editor::media::set_project_anchor(Some(dir));
        let mut found = project_over(&clip, 0.5, &format!("ve relative found {pid}"));
        found.media[0].path = file.clone();
        let out = export(&found);
        let exported = decode_frame(&out, 0.2);
        let source = decode_frame(&clip, 0.2);
        let detail = horizontal_detail(&exported);
        let source_detail = horizontal_detail(&source);
        let diff = mean_abs_diff(&exported, &source);
        println!(
            "relative-path export: detail {detail:.2} (source {source_detail:.2}), difference {diff:.2}"
        );
        assert!(
            detail > source_detail * 0.4,
            "the encoded frame has the source's structure, not a flat fill \
         (detail {detail:.2} vs source {source_detail:.2})",
        );
        assert!(
            diff < 30.0,
            "and is the same picture: mean difference {diff:.2}"
        );

        video_editor::media::set_project_anchor(None);
        tidy(&out);
    }
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn exported_geometry_titles_transforms_and_speed_reach_the_file() {
    // Case `a_wipe_reaches_the_encoded_file_as_geometry`:
    // A wipe has to reach the encoded file as *geometry*: halfway across,
    // one side of the frame is the incoming clip and the other is still the
    // outgoing one.
    //
    // The transition this replaced was a flat full-frame flash that rendered
    // identically for a dissolve, a fade and a wipe. Comparing whole-frame
    // averages cannot tell those apart, so this asserts on the split between
    // the two halves — which is the thing a wipe has and a dissolve does not.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::model::{ClipEffect, EffectKind, Transition};

        let clip = demo_clip();
        let pid = std::process::id();
        // Both clips come off one source. Invert the incoming clip so the two
        // sides of the wipe remain unmistakable regardless of source motion.
        let build = |kind: Transition, name: String| {
            let mut p = project_over(&clip, 1.0, &name);
            let track = p.tracks[0].id;
            let media = p.media[0].id;
            let second = p.add_clip(track, media, 1.0).unwrap();
            let c = p.clip_mut(second).unwrap();
            c.in_point = 3.0;
            c.out_point = 4.0;
            c.transition = kind;
            c.effects = vec![ClipEffect {
                id: uuid::Uuid::new_v4(),
                kind: EffectKind::Invert,
                amount: 1.0,
            }]
            .into();
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
    // Case `a_title_reaches_the_encoded_file_as_real_glyphs`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
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
    // Case `a_transform_moves_the_picture_in_the_encoded_file`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
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
        assert!(cy > GRID_H as f64 * 0.6, "and down, got y={cy:.1}",);
        tidy(&path);
    }
    // Case `a_keyframed_move_is_at_a_different_place_in_each_encoded_second`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        // An animated parameter has to be sampled per frame during the encode,
        // not once for the whole file. Two instants of the same clip must
        // therefore differ — and differ in the direction the curve runs.
        let (mut p, _, cid) = title_project("title_animated_export", "HELLO");
        {
            let c = p.clip_mut(cid).expect("clip");
            c.transform = Transform {
                scale_x: 0.4,
                scale_y: 0.4,
                ..Transform::default()
            };
            c.set_key_at(Param::PosY, 0.0, -0.3, Interp::Linear);
            c.set_key_at(Param::PosY, 3.0, 0.3, Interp::Linear);
        }
        let path = export(&p);
        let (_, early) = {
            let f = decode_frame(&path, 0.2);
            (
                bright_fraction(&f),
                bright_centre(&f).expect("lit at the head"),
            )
        };
        let (_, late) = {
            let f = decode_frame(&path, 2.8);
            (
                bright_fraction(&f),
                bright_centre(&f).expect("lit at the tail"),
            )
        };
        assert!(
            late.1 > early.1 + GRID_H as f64 * 0.2,
            "the keyframed move travels down the encoded file: y {:.1} → {:.1}",
            early.1,
            late.1,
        );
        tidy(&path);
    }
    // Case `speed_shortens_the_encoded_file`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
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
    let lit = px
        .iter()
        .filter(|q| q[0] > 170 && q[1] > 170 && q[2] > 170)
        .count();
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

// ---------------------------------------------------------------------------
// Audio: proving the export's second branch writes a real, correct track.
//
// The encoded source has its own verified AAC track, but the mixer needs an
// input whose exact samples are known before anything encodes them. A length
// check against silence can pass, which is how an "audio" test once went
// green over a file with no audio in it. So this second fixture is generated
// as PCM and known sample by sample.
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
    let path = std::env::temp_dir().join(format!("tilo-ve-tone-{}.wav", std::process::id()));
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
    let convert = gst::ElementFactory::make("audioconvert")
        .build()
        .expect("audioconvert");
    let resample = gst::ElementFactory::make("audioresample")
        .build()
        .expect("audioresample");
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
    //
    // But an empty pull is only evidence of that *after* the decoder has
    // finished finding streams. `uridecodebin` has to typefind the container
    // and autoplug a decoder first, and on a loaded machine that takes longer
    // than one 500ms pull — so treating the first empty pull as proof of
    // silence reported "no audio" for files that had plenty. It made this
    // test pass on an idle box and fail beside a build, which is the worst
    // way for a test to be wrong: it looks like the code broke.
    //
    // `no-more-pads` is the decoder saying it has finished. Until it fires,
    // "the sink produced nothing" is a statement about the clock, not about
    // the file. The production decoder learned this already; this is its
    // copy of the same loop.
    let settled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let settled_in = settled.clone();
    src.connect_no_more_pads(move |_| {
        settled_in.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let bus = pipeline.bus();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if ui::time::now() >= deadline {
            panic!("decoding audio from {} timed out", path.display());
        }
        // A pipeline that has failed will never produce a sample, so stop
        // waiting on one rather than sitting out the whole deadline.
        let mut failed = None;
        if let Some(bus) = bus.as_ref() {
            while let Some(msg) = bus.pop() {
                if let gst::MessageView::Error(e) = msg.view() {
                    failed = Some(e.error().to_string());
                }
            }
        }
        if let Some(reason) = failed {
            panic!("decoding audio from {} failed: {reason}", path.display());
        }
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) {
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let Ok(map) = buffer.map_readable() else {
                continue;
            };
            for c in map.chunks_exact(4) {
                out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        } else {
            if sink.is_eos() {
                break;
            }
            // Nothing yet, and the decoder has finished finding streams:
            // there is genuinely no audio here.
            if out.is_empty() && settled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    let native = *native.lock().expect("caps slot");
    DecodedAudio { pcm: out, native }
}

/// A project with generated footage on a video track and the generated tone on
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

#[test]
#[serial_test::serial(video_editor_gst)]
fn exported_audio_is_real_audible_and_placed() {
    // Case `the_generated_footage_and_mixer_fixture_are_audible`:
    // The generated source itself is a genuine audiovisual container, and the
    // separate WAV used by mixer tests is independently verified too. This keeps
    // source decoding and known-signal mixer assertions from depending on private
    // footage or accidentally testing silence.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        let clip = demo_clip();
        let decoded = decode_audio_full(&clip);
        let (rate, channels) = decoded.native.unwrap_or_else(|| {
            panic!("{} has no audio track", clip.display());
        });
        assert!(
            rate > 0 && channels == 2 && peak(&decoded.pcm) > 0.1,
            "the generated source has an audible stereo track: {:?}, peak {:.3}",
            decoded.native,
            peak(&decoded.pcm),
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
            "generated clip: {:?}, {:.3}s, peak {:.3}; mixer fixture: {:?}, {:.3}s, peak {:.3}",
            decoded.native,
            decoded.secs(),
            peak(&decoded.pcm),
            f.native,
            f.secs(),
            peak(&f.pcm),
        );
    }
    // Case `an_mp4_export_carries_the_timelines_real_audio`.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        assert_audio_round_trip(Container::Mp4);
    }
    // Case `a_webm_export_carries_the_timelines_real_audio`:
    // The same for WebM, whose audio is a different encoder into a different
    // muxer — `opusenc` into `webmmux`, not AAC into `mp4mux`. The WebM *video*
    // chain went unproven until recently for exactly this reason; its audio chain
    // shares none of MP4's elements either.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        assert_audio_round_trip(Container::WebM);
    }
    // Case `the_encoded_sources_audio_survives_mp4_and_webm_export`:
    // The source fixture's AAC track must survive the same import/mix/export
    // path as user footage, not merely prove decodable in isolation. Exercise
    // both output muxers because their timestamp and encoder-delay handling are
    // independent.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        let source = demo_clip();
        let pid = std::process::id();
        for container in [Container::Mp4, Container::WebM] {
            let name = format!("ve source audio {} {pid}", container.extension());
            let output = export_as(&av_project(&name, &source, &source), container);
            let decoded = decode_audio_full(&output);
            let (rate, channels) = decoded.native.unwrap_or_else(|| {
                panic!(
                    "the {container:?} export dropped the encoded source's AAC track: {}",
                    output.display()
                );
            });
            assert_eq!(rate, FIXTURE_RATE.cast_signed());
            assert_eq!(channels, 2);
            assert!(
                (decoded.secs() - FIXTURE_SECS).abs() < 0.15,
                "{container:?} retained {:.3}s of the source's {FIXTURE_SECS}s audio cut",
                decoded.secs(),
            );
            let left = channel(&decoded.pcm, 0);
            assert!(peak(&left) > 0.1, "the propagated source audio is audible");
            assert!(
                goertzel(&left, 440.0) > 0.1,
                "the propagated track retains the source's 440 Hz signal",
            );
            tidy(&output);
        }
    }
    // Case `the_exported_audio_sits_where_the_timeline_puts_it`:
    // Where the sound sits in time relative to the picture.
    //
    // The fixture's right channel changes tone at a known instant, so the
    // exported audio can be asked where that instant ended up. A video frame's
    // timeline position is fixed by the frame clock the export counts with, so
    // audio landing at the wrong second *is* the two streams disagreeing.
    //
    // Measured in **both** containers, because they can see different things.
    //
    // Opus in WebM is delay-compensated end to end: the landmark comes back at
    // exactly the second the fixture put it, and the track at exactly 2.000s, so
    // its reading is the export's own placement with nothing added. That is the
    // instrument here, and it is sharp — reinstating the half-block read offset
    // `mix_block` once had moves it from +0 ms to -15 ms, which this catches.
    //
    // AAC in MP4 cannot be read that way. Its priming is not compensated on this
    // round trip: the track comes back 2.048s long for a 2s timeline and the
    // landmark lands 45 ms late with everything working correctly. So it gets a
    // band wide enough for that, and catches gross displacement only.
    //
    // The spread between the two is asserted as well. It says nothing about a
    // shift they share — that is what the Opus reading is for — but a container
    // whose audio is placed differently from the other's, when both are fed by
    // one mixer through one set of timestamps, has something wrong with it that
    // neither absolute reading need show.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        let video = demo_clip();
        let audio = audio_fixture();
        let pid = std::process::id();

        let mut found = Vec::new();
        for container in [Container::Mp4, Container::WebM] {
            let name = format!("ve sync {} {pid}", container.extension());
            let out = export_as(&av_project(&name, &video, &audio), container);
            let decoded = decode_audio_full(&out);
            let right = channel(&decoded.pcm, 1);
            assert!(
                !right.is_empty(),
                "there is no audio in {} to locate",
                out.display()
            );

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
            // Opus adds no delay of its own, so its band is tight: measured at
            // +0 ms working and -15 ms with the mixer's old half-block read
            // offset put back, and 10 ms sits between those with two of the 5 ms
            // detection windows to spare. AAC's own priming puts its landmark
            // 45 ms late when everything is right, so its band has to cover that
            // and it catches only gross displacement.
            let (limit_ms, limit_len) = match container {
                Container::WebM => (10.0, 0.02),
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

        // Both containers are fed by one mixer through one set of timestamps, so
        // beyond their own priming they must agree about where a timeline instant
        // is. Measured at 45 ms apart, which is AAC's priming; a container-
        // specific fault would widen it.
        let spread_ms = (found[0].1 - found[1].1).abs() * 1000.0;
        println!("landmark spread between containers: {spread_ms:.1} ms");
        assert!(
            spread_ms < 80.0,
            "the two containers disagree by {spread_ms:.1} ms about where the same timeline \
         instant is, more than the priming of either accounts for",
        );
    }
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
    let (l440, l880) = (
        goertzel(&left[early], LEFT_HZ),
        goertzel(&left[early], RIGHT_HZ_EARLY),
    );
    let (r440, r880) = (
        goertzel(&right[early], LEFT_HZ),
        goertzel(&right[early], RIGHT_HZ_EARLY),
    );
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

/// Where in the decoded audio the right channel changes tone, in seconds, or
/// `None` when it never does.
///
/// The window that best splits the track into "early tone before, late tone
/// after" — the boundary maximising how many windows fall on the side they
/// should. Taking the last early-dominant window instead would be at the
/// mercy of a single flipped window in a codec's padding at the tail, which
/// is not a hypothetical: AAC's trailing frames read as late-dominant and
/// put the landmark a whole second out.
fn tone_switch_secs(right: &[f32]) -> Option<f64> {
    let win = (FIXTURE_RATE as f64 * 0.005) as usize; // 5ms
    let late: Vec<bool> = right
        .chunks_exact(win)
        .map(|b| goertzel(b, RIGHT_HZ_LATE) > goertzel(b, RIGHT_HZ_EARLY))
        .collect();
    if late.len() < 4 {
        return None;
    }
    let total_late = late.iter().filter(|&&l| l).count();
    // Both tones have to actually be present, or there is no step to find and
    // the best boundary would be a meaningless end of the track.
    if total_late == 0 || total_late == late.len() {
        return None;
    }
    // score(k) = early-dominant windows before k + late-dominant from k on,
    // rolled forward rather than recomputed.
    let mut score = total_late as i64;
    let (mut best, mut best_k) = (score, 0usize);
    for (k, &l) in late.iter().enumerate() {
        score += if l { -1 } else { 1 };
        if score > best {
            best = score;
            best_k = k + 1;
        }
    }
    Some(best_k as f64 * win as f64 / FIXTURE_RATE as f64)
}

// ---------------------------------------------------------------------------
// Chroma key and shape mask in the encoded file.
//
// Both are compositing features, so what has to reach the file is not a
// colour shift but the *layer underneath* becoming visible where the top one
// was cut. Every test here stacks two clips of a generated plate — the lower
// one inverted, so the two sides are unmistakable — and reads the decoded
// export back.
//
// This purpose-built plate exists because the general footage has no green
// screen in it, and a key asserted against footage that was never keyed
// proves nothing. It is verified before it is used, for the same reason the
// audio fixture is: a fixture that turns out to be the wrong colour makes
// every assertion below a statement about nothing.
// ---------------------------------------------------------------------------

/// The plate colour, matching the editor's own "Green" key preset.
const PLATE_RGB: [u8; 3] = [15, 184, 51];
/// The subject standing on it: a horizontal orange band across the middle
/// third, nothing like the plate in hue.
const SUBJECT_RGB: [u8; 3] = [235, 130, 30];
const PLATE_W: u32 = 320;
const PLATE_H: u32 = 180;
const PLATE_SECS: f64 = 1.2;
const PLATE_FPS: u32 = 30;

/// Rows `[0.4, 0.6)` of the frame carry the subject; the rest is plate.
fn plate_pixel(v: f64) -> [u8; 3] {
    if (0.4..0.6).contains(&v) {
        SUBJECT_RGB
    } else {
        PLATE_RGB
    }
}

/// Encode the green-screen plate to `path`.
///
/// Written frame by frame from [`plate_pixel`], so the file's contents are
/// decided here and not by whatever a test-pattern generator felt like
/// producing. The encoder is the one this machine actually resolves to, so
/// the fixture builds wherever the export itself runs.
fn write_plate(path: &std::path::Path) {
    gst::init().expect("gstreamer init");
    let (enc, parse, mux) = encode_chain(Container::Mp4);
    let bitrate = enc.spec.bitrate_arg(6_000_000);
    let element = enc.spec.element;
    let parse = parse.map(|p| format!("{p} ! ")).unwrap_or_default();
    let desc = format!(
        "appsrc name=plate is-live=false format=time \
         caps=video/x-raw,format=RGBA,width={PLATE_W},height={PLATE_H},framerate={PLATE_FPS}/1 \
         ! videoconvert ! {element} {bitrate} ! {parse}{mux} ! filesink location={}",
        path.display()
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap_or_else(|e| panic!("build the plate pipeline ({desc}): {e}"))
        .downcast::<gst::Pipeline>()
        .expect("a pipeline");
    let src = pipeline
        .by_name("plate")
        .expect("appsrc")
        .downcast::<gst_app::AppSrc>()
        .expect("an appsrc");
    src.set_format(gst::Format::Time);
    pipeline
        .set_state(gst::State::Playing)
        .expect("start the plate pipeline");

    let mut rgba = vec![255u8; (PLATE_W * PLATE_H * 4) as usize];
    for y in 0..PLATE_H {
        let v = (y as f64 + 0.5) / PLATE_H as f64;
        let c = plate_pixel(v);
        for x in 0..PLATE_W {
            let i = ((y * PLATE_W + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&c);
        }
    }
    let frames = (PLATE_SECS * PLATE_FPS as f64).round() as u64;
    let ns = 1_000_000_000 / PLATE_FPS as u64;
    for i in 0..frames {
        let mut buffer = gst::Buffer::with_size(rgba.len()).expect("allocate");
        {
            let b = buffer.get_mut().expect("fresh buffer is uniquely owned");
            b.set_pts(gst::ClockTime::from_nseconds(ns * i));
            b.set_duration(gst::ClockTime::from_nseconds(ns));
            b.map_writable().expect("map").copy_from_slice(&rgba);
        }
        src.push_buffer(buffer)
            .unwrap_or_else(|e| panic!("push plate frame {i}: {e:?}"));
    }
    src.end_of_stream().expect("end the plate stream");
    let bus = pipeline.bus().expect("bus");
    for msg in bus.iter_timed(gst::ClockTime::from_seconds(60)) {
        match msg.view() {
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => {
                let _ = pipeline.set_state(gst::State::Null);
                panic!("plate encode failed: {e:?}");
            }
            _ => {}
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
}

/// The plate on disk, generated once per test process and **verified**.
///
/// The verification is the point: if the generated file is not actually a
/// green plate with an orange band on it, every key assertion below becomes
/// a statement about nothing.
fn green_plate() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("tilo-ve-plate-{}.mp4", std::process::id()));
    if !path.exists() {
        write_plate(&path);
    }
    let bytes = std::fs::metadata(&path).expect("the plate exists").len();
    assert!(
        bytes > 512,
        "the plate is a real encoded file, got {bytes} bytes"
    );
    let frame = decode_frame(&path, 0.4);
    let at = |u: f64, v: f64| {
        let x = ((u * GRID_W as f64) as usize).min(GRID_W - 1);
        let y = ((v * GRID_H as f64) as usize).min(GRID_H - 1);
        frame[y * GRID_W + x]
    };
    // The plate reads as green above and below the band, and as the subject
    // across it — before the editor has touched it.
    for v in [0.15, 0.85] {
        let q = at(0.5, v);
        assert!(
            q[1] > 120 && q[1] as i32 > q[0] as i32 + 60 && q[1] as i32 > q[2] as i32 + 60,
            "the generated plate is green at v={v}, got {q:?} — a fixture that is not a \
             green screen makes every key assertion below meaningless",
        );
    }
    let band = at(0.5, 0.5);
    assert!(
        band[0] > 150 && band[0] as i32 > band[2] as i32 + 60,
        "and carries the orange subject across its middle, got {band:?}",
    );
    path
}

/// A two-layer project over the generated plate: V1 inverted (so it reads
/// magenta wherever the plate is green) and V2 the plate itself. Returns the
/// project and the id of the upper clip.
fn plate_stack(name: &str) -> (Project, uuid::Uuid) {
    use video_editor::model::{ClipEffect, EffectKind};

    let plate = green_plate();
    let mut p = Project::new(name);
    p.width = PLATE_W;
    p.height = PLATE_H;
    let media = |p: &mut Project| {
        let id = uuid::Uuid::new_v4();
        p.media.push(MediaItem {
            id,
            name: "plate".into(),
            kind: MediaKind::Video,
            duration: PLATE_SECS,
            width: PLATE_W,
            height: PLATE_H,
            fps: PLATE_FPS as f64,
            palette: 3,
            path: plate.to_string_lossy().into_owned(),
            ..Default::default()
        });
        id
    };
    let under_m = media(&mut p);
    let over_m = media(&mut p);
    let track = |p: &mut Project, index: u32| {
        let id = uuid::Uuid::new_v4();
        p.tracks.push(Track {
            id,
            name: format!("V{index}"),
            kind: TrackKind::Video,
            index,
            ..Default::default()
        });
        id
    };
    let v1 = track(&mut p, 1);
    let v2 = track(&mut p, 2);
    let under = p.add_clip(v1, under_m, 0.0).expect("media exists");
    let over = p.add_clip(v2, over_m, 0.0).expect("media exists");
    for id in [under, over] {
        let c = p.clip_mut(id).expect("clip");
        c.in_point = 0.1;
        c.out_point = 1.0;
    }
    // Inverting the lower layer is what makes "the layer beneath showed
    // through" a visible fact rather than an inference: green inverts to
    // magenta, which no part of the upper layer is.
    p.clip_mut(under).expect("clip").effects = vec![ClipEffect {
        id: uuid::Uuid::new_v4(),
        kind: EffectKind::Invert,
        amount: 1.0,
    }]
    .into();
    (p, over)
}

/// Sample the decoded grid at a fractional position.
fn sample(px: &[[u8; 3]], u: f64, v: f64) -> [u8; 3] {
    let x = ((u * GRID_W as f64) as usize).min(GRID_W - 1);
    let y = ((v * GRID_H as f64) as usize).min(GRID_H - 1);
    px[y * GRID_W + x]
}

fn is_greenish(q: [u8; 3]) -> bool {
    q[1] > 90 && q[1] as i32 > q[0] as i32 + 40 && q[1] as i32 > q[2] as i32 + 40
}
/// Inverted green: strong red and blue, weak green.
fn is_magentaish(q: [u8; 3]) -> bool {
    q[0] as i32 > q[1] as i32 + 40 && q[2] as i32 > q[1] as i32 + 40
}

#[test]
#[serial_test::serial(video_editor_gst)]
fn exported_keys_masks_and_curves_reach_the_encoded_file() {
    // Case `a_chroma_key_reaches_the_encoded_file_as_compositing`:
    // The key has to reach the encoded file as *compositing*: where the plate
    // was, the layer underneath is now visible, and the subject standing on the
    // plate is still there.
    //
    // Read against the same stack exported with the key off, so what the
    // difference proves is the key and not the fixture.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::model::ChromaKey;

        let pid = std::process::id();
        let (plain_p, _) = plate_stack(&format!("ve key off {pid}"));
        let plain = export(&plain_p);

        let (mut keyed_p, over) = plate_stack(&format!("ve key on {pid}"));
        keyed_p.clip_mut(over).expect("clip").key = ChromaKey {
            enabled: true,
            color: [
                PLATE_RGB[0] as f64 / 255.0,
                PLATE_RGB[1] as f64 / 255.0,
                PLATE_RGB[2] as f64 / 255.0,
            ],
            tolerance: 0.25,
            softness: 0.1,
            spill: 0.0,
        };
        let keyed = export(&keyed_p);

        let a = decode_frame(&plain, 0.4);
        let b = decode_frame(&keyed, 0.4);
        // Well clear of the band, and off the frame edges where the encoder's
        // chroma subsampling smears neighbouring regions together.
        for v in [0.2, 0.82] {
            let off = sample(&a, 0.5, v);
            let on = sample(&b, 0.5, v);
            println!("v={v}: key off {off:?}  key on {on:?}");
            assert!(
                is_greenish(off),
                "unkeyed, the upper layer covers the frame at v={v}: {off:?}",
            );
            assert!(
                is_magentaish(on),
                "keyed, the ENCODED file shows the layer beneath at v={v}: {on:?}",
            );
        }
        // The subject survives — a key that ate it would pass every check above
        // and still be useless.
        let subject = sample(&b, 0.5, 0.5);
        assert!(
            subject[0] > 130 && subject[0] as i32 > subject[2] as i32 + 40,
            "the subject standing on the plate is still in the encoded file: {subject:?}",
        );

        tidy(&plain);
        tidy(&keyed);
    }
    // Case `a_mask_reaches_the_encoded_file_as_geometry`:
    // A mask has to reach the encoded file as geometry: inside the shape the
    // upper layer, outside it the layer beneath.
    //
    // Read against the same stack exported with no mask, so a frame that simply
    // failed to composite cannot pass.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::model::{Mask, MaskShape};

        let pid = std::process::id();
        let (plain_p, _) = plate_stack(&format!("ve mask off {pid}"));
        let plain = export(&plain_p);

        let (mut masked_p, over) = plate_stack(&format!("ve mask on {pid}"));
        masked_p.clip_mut(over).expect("clip").mask = Mask {
            shape: MaskShape::Ellipse,
            w: 0.45,
            h: 0.45,
            feather: 0.05,
            ..Mask::default()
        };
        let masked = export(&masked_p);

        let a = decode_frame(&plain, 0.4);
        let b = decode_frame(&masked, 0.4);
        // Sampled off the middle band so what is being cut is the plate, not the
        // subject. Inside the disc but above the band; outside it near the edge.
        // v=0.35 is two thirds of the way out along the disc's vertical radius
        // — clear of both the feathered rim and the subject band below it.
        let inside = (0.5, 0.35);
        let outside = (0.06, 0.12);
        println!(
            "unmasked edge {:?}   masked inside {:?}   masked edge {:?}",
            sample(&a, outside.0, outside.1),
            sample(&b, inside.0, inside.1),
            sample(&b, outside.0, outside.1),
        );
        assert!(
            is_greenish(sample(&a, outside.0, outside.1)),
            "unmasked, the upper layer reaches the frame edge: {:?}",
            sample(&a, outside.0, outside.1),
        );
        assert!(
            is_greenish(sample(&b, inside.0, inside.1)),
            "masked, the upper layer is still inside the disc: {:?}",
            sample(&b, inside.0, inside.1),
        );
        assert!(
            is_magentaish(sample(&b, outside.0, outside.1)),
            "and the layer beneath shows outside it in the ENCODED file: {:?}",
            sample(&b, outside.0, outside.1),
        );

        tidy(&plain);
        tidy(&masked);
    }
    // Case `a_keyframed_mask_moves_across_the_encoded_file`:
    // A keyframed mask has to be in a different place in each encoded second.
    // A mask baked once at the head of the export would pass the geometry test
    // above and still be motionless in the file.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::model::{Mask, MaskShape};

        let (mut p, over) = plate_stack(&format!("ve mask move {}", std::process::id()));
        {
            let c = p.clip_mut(over).expect("clip");
            c.mask = Mask {
                shape: MaskShape::Ellipse,
                w: 0.4,
                h: 0.9,
                feather: 0.05,
                ..Mask::default()
            };
            c.set_key_at(Param::MaskX, 0.0, -0.3, Interp::Linear);
            c.set_key_at(Param::MaskX, 0.85, 0.3, Interp::Linear);
        }
        let path = export(&p);
        let early = decode_frame(&path, 0.05);
        let late = decode_frame(&path, 0.8);
        // Left of centre at the head, right of centre by the tail. Sampled above
        // the subject band, so what moves is the mask and not the picture.
        let (left, right) = ((0.22, 0.2), (0.78, 0.2));
        println!(
            "early L={:?} R={:?}   late L={:?} R={:?}",
            sample(&early, left.0, left.1),
            sample(&early, right.0, right.1),
            sample(&late, left.0, left.1),
            sample(&late, right.0, right.1),
        );
        assert!(
            is_greenish(sample(&early, left.0, left.1)),
            "the disc starts on the left of the encoded frame",
        );
        assert!(
            is_magentaish(sample(&early, right.0, right.1)),
            "and has not reached the right yet",
        );
        assert!(
            is_magentaish(sample(&late, left.0, left.1)),
            "by the end it has left the left",
        );
        assert!(
            is_greenish(sample(&late, right.0, right.1)),
            "and arrived on the right",
        );
        tidy(&path);
    }
    // Case `a_tone_curve_reaches_the_encoded_file_as_a_table`:
    // The curve has to reach the encoded file **as a table**: every band of the
    // wedge lands where `Curve::apply` says it should, and the two ends — which
    // the curve pins — do not move.
    //
    // That last part is what tells a curve from a brightness slider. A slider
    // lifting the midtones lifts the shadows and the highlights with them; the
    // whole reason to have curves is that this one does not.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::curve::Curve;

        let pid = std::process::id();
        let (plain_project, _) = wedge_project(&format!("ve curve plain {pid}"));
        let plain = export(&plain_project);

        let (mut graded_project, cid) = wedge_project(&format!("ve curve shaped {pid}"));
        // An S: crush the low midtones, lift the high ones, both ends pinned.
        let mut curve = Curve::default();
        curve.master.set_point(0.3, 0.14);
        curve.master.set_point(0.7, 0.88);
        graded_project.clip_mut(cid).unwrap().curves = curve.clone();
        let shaped = export(&graded_project);

        let before = decode_frame(&plain, 0.3);
        let after = decode_frame(&shaped, 0.3);

        // H.264 at this bitrate holds a flat band to within a few code values;
        // the curve moves the midtones by 40+, so the margin is comfortable.
        const TOL: f64 = 16.0;
        let mut moved = 0;
        for i in 0..WEDGE.len() {
            let a = sample(&before, band_u(i), 0.5);
            let b = sample(&after, band_u(i), 0.5);
            let input = a[1] as f64 / 255.0;
            let want = curve.apply(input, input, input).1 * 255.0;
            println!("band {i}: encoded plain {a:?} -> shaped {b:?}, table says {want:.1}");
            assert!(
                (b[1] as f64 - want).abs() < TOL,
                "band {i}: the encoded tone {} should be the table's {want:.1} for input {}",
                b[1],
                a[1],
            );
            if (b[1] as f64 - a[1] as f64).abs() > TOL {
                moved += 1;
            }
        }
        assert!(
            moved >= 3,
            "the curve really reshaped the picture, {moved} bands moved"
        );

        // And the shape is a *shape*: the two ends the curve pins barely move
        // while the midtones move several times as far. A brightness or contrast
        // slider cannot do that — it moves the whole wedge together — so this is
        // the assertion that tells a curve from the sliders it sits beside.
        let shift = |i: usize| {
            (sample(&after, band_u(i), 0.5)[1] as f64 - sample(&before, band_u(i), 0.5)[1] as f64)
                .abs()
        };
        let ends = shift(0).max(shift(WEDGE.len() - 1));
        let mids = (1..WEDGE.len() - 1).map(shift).fold(0.0_f64, f64::max);
        println!("ends moved at most {ends:.1}, midtones up to {mids:.1}");
        assert!(
            ends < 20.0,
            "the pinned ends stay near where they were: {ends:.1}"
        );
        assert!(
            mids > ends * 3.0,
            "and the midtones move several times as far: {mids:.1} against {ends:.1}",
        );

        tidy(&plain);
        tidy(&shaped);
    }
    // Case `a_per_channel_curve_reaches_the_encoded_file`:
    // A per-channel curve reaches the file as a colour cast on a neutral wedge —
    // something the three grade axes cannot produce at all.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::curve::Curve;

        let pid = std::process::id();
        let (mut project, cid) = wedge_project(&format!("ve curve channel {pid}"));
        let mut curve = Curve::default();
        curve.red.set_point(0.5, 0.9);
        project.clip_mut(cid).unwrap().curves = curve;
        let path = export(&project);

        let frame = decode_frame(&path, 0.3);
        // Band 3 is 112/255 ≈ 0.44 in, well inside the lifted region.
        let q = sample(&frame, band_u(3), 0.5);
        println!("mid band with red lifted: {q:?}");
        assert!(
            q[0] as i32 > q[1] as i32 + 40 && q[0] as i32 > q[2] as i32 + 40,
            "only red was lifted, so the neutral band comes back red-cast: {q:?}",
        );
        // The far ends are pinned on every channel, so the wedge's dark end is
        // still neutral — the cast is the curve's shape, not a tint.
        let dark = sample(&frame, band_u(0), 0.5);
        assert!(
            dark[0].abs_diff(dark[1]) < 20,
            "the pinned dark end stays neutral: {dark:?}",
        );

        tidy(&path);
    }
    // Case `an_animated_curve_amount_grades_each_encoded_second_differently`:
    // An animated curve amount reaches the file as a grade that ramps: the same
    // footage is graded differently in different seconds.
    'case: {
        if !generated_clip::media_stack() {
            break 'case;
        }
        use video_editor::curve::Curve;

        let pid = std::process::id();
        let (mut project, cid) = wedge_project(&format!("ve curve anim {pid}"));
        let mut curve = Curve::default();
        curve.master.set_point(0.5, 0.95);
        {
            let c = project.clip_mut(cid).unwrap();
            c.in_point = 0.0;
            c.out_point = WEDGE_SECS;
            c.curves = curve;
            c.set_key_at(Param::CurveMix, 0.0, 0.0, Interp::Linear);
            c.set_key_at(Param::CurveMix, WEDGE_SECS, 1.0, Interp::Linear);
        }
        let path = export(&project);

        let early = sample(&decode_frame(&path, 0.05), band_u(3), 0.5)[1] as f64;
        let late = sample(&decode_frame(&path, WEDGE_SECS - 0.1), band_u(3), 0.5)[1] as f64;
        println!("mid band: {early} early, {late} late");
        assert!(
            late > early + 60.0,
            "the amount ramped on across the encoded file: {early} -> {late}",
        );

        tidy(&path);
    }
}

// ---------------------------------------------------------------------------
// Tone curves in the encoded file.
//
// A curve is a table from input tone to output tone, so proving it reached
// the file means reading tones back and comparing them with what the table
// says — not "the picture got brighter", which a brightness slider also does.
//
// The fixture is a step wedge of known greys rather than the general footage:
// against real footage there is no tone whose value is known in advance, so
// nothing could distinguish "the curve was applied" from "the encoder shifted
// the picture a little". It is verified before it is used, for the same reason
// the green plate is.
// ---------------------------------------------------------------------------

/// The eight grey levels of the wedge, one vertical band each.
const WEDGE: [u8; 8] = [16, 48, 80, 112, 144, 176, 208, 240];
const WEDGE_W: u32 = 320;
const WEDGE_H: u32 = 180;
const WEDGE_SECS: f64 = 1.0;
const WEDGE_FPS: u32 = 30;

/// Encode the step wedge to `path`, band by band, so the file's contents are
/// decided here rather than by a test-pattern generator.
fn write_wedge(path: &std::path::Path) {
    gst::init().expect("gstreamer init");
    let (enc, parse, mux) = encode_chain(Container::Mp4);
    let bitrate = enc.spec.bitrate_arg(12_000_000);
    let element = enc.spec.element;
    let parse = parse.map(|p| format!("{p} ! ")).unwrap_or_default();
    let desc = format!(
        "appsrc name=wedge is-live=false format=time \
         caps=video/x-raw,format=RGBA,width={WEDGE_W},height={WEDGE_H},framerate={WEDGE_FPS}/1 \
         ! videoconvert ! {element} {bitrate} ! {parse}{mux} ! filesink location={}",
        path.display()
    );
    let pipeline = gst::parse::launch(&desc)
        .unwrap_or_else(|e| panic!("build the wedge pipeline ({desc}): {e}"))
        .downcast::<gst::Pipeline>()
        .expect("a pipeline");
    let src = pipeline
        .by_name("wedge")
        .expect("appsrc")
        .downcast::<gst_app::AppSrc>()
        .expect("an appsrc");
    src.set_format(gst::Format::Time);
    pipeline
        .set_state(gst::State::Playing)
        .expect("start the wedge pipeline");

    let mut rgba = vec![255u8; (WEDGE_W * WEDGE_H * 4) as usize];
    for y in 0..WEDGE_H {
        for x in 0..WEDGE_W {
            let band = (x as usize * WEDGE.len() / WEDGE_W as usize).min(WEDGE.len() - 1);
            let v = WEDGE[band];
            let i = ((y * WEDGE_W + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&[v, v, v]);
        }
    }
    let frames = (WEDGE_SECS * WEDGE_FPS as f64).round() as u64;
    let ns = 1_000_000_000 / WEDGE_FPS as u64;
    for i in 0..frames {
        let mut buffer = gst::Buffer::with_size(rgba.len()).expect("allocate");
        {
            let b = buffer.get_mut().expect("fresh buffer is uniquely owned");
            b.set_pts(gst::ClockTime::from_nseconds(ns * i));
            b.set_duration(gst::ClockTime::from_nseconds(ns));
            b.map_writable().expect("map").copy_from_slice(&rgba);
        }
        src.push_buffer(buffer)
            .unwrap_or_else(|e| panic!("push wedge frame {i}: {e:?}"));
    }
    src.end_of_stream().expect("end the wedge stream");
    let bus = pipeline.bus().expect("bus");
    for msg in bus.iter_timed(gst::ClockTime::from_seconds(60)) {
        match msg.view() {
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => {
                let _ = pipeline.set_state(gst::State::Null);
                panic!("wedge encode failed: {e:?}");
            }
            _ => {}
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
}

/// Where band `i` sits horizontally, as a fraction of the frame — the middle
/// of the band, away from the edges an encoder softens.
fn band_u(i: usize) -> f64 {
    (i as f64 + 0.5) / WEDGE.len() as f64
}

/// The step wedge on disk, generated once per test process and **verified**:
/// if the bands do not come back as a monotonically rising neutral ramp,
/// every tone assertion below is a statement about nothing.
fn grey_wedge() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("tilo-ve-wedge-{}.mp4", std::process::id()));
    if !path.exists() {
        write_wedge(&path);
    }
    let frame = decode_frame(&path, 0.4);
    let mut last = -1.0_f64;
    for (i, want) in WEDGE.iter().enumerate() {
        let q = sample(&frame, band_u(i), 0.5);
        let got = q[1] as f64;
        assert!(
            (got - *want as f64).abs() < 12.0,
            "band {i} of the generated wedge should read ~{want}, got {q:?}",
        );
        assert!(
            q[0].abs_diff(q[1]) < 12 && q[1].abs_diff(q[2]) < 12,
            "and be neutral grey, got {q:?}",
        );
        assert!(
            got > last,
            "the wedge rises: band {i} is {got}, previous {last}"
        );
        last = got;
    }
    path
}

/// A one-clip project over the wedge, at the wedge's own frame size.
fn wedge_project(name: &str) -> (Project, uuid::Uuid) {
    let wedge = grey_wedge();
    let mut p = Project::new(name);
    p.width = WEDGE_W;
    p.height = WEDGE_H;
    let mid = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id: mid,
        name: "wedge".into(),
        kind: MediaKind::Video,
        duration: WEDGE_SECS,
        width: WEDGE_W,
        height: WEDGE_H,
        fps: WEDGE_FPS as f64,
        palette: 3,
        path: wedge.to_string_lossy().into_owned(),
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
    let c = p.clip_mut(cid).unwrap();
    c.in_point = 0.1;
    c.out_point = 0.8;
    (p, cid)
}

// ---------------------------------------------------------------------------
// A source the export cannot read.
// ---------------------------------------------------------------------------
