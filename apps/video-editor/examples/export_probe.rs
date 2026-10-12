#![allow(
    clippy::expect_used,
    clippy::map_unwrap_or,
    clippy::panic,
    clippy::redundant_clone,
    clippy::unwrap_used,
    reason = "diagnostic export probe intentionally exits loudly on invalid command-line input"
)]

//! Export a real clip through the editor's own pipeline and report where it
//! landed — the evidence behind any claim that the export carries the user's
//! footage, in a form a human can check with their own tools.
//!
//! ```text
//! cargo run -p video-editor --features gst --example export_probe -- <clip> <out.mp4>
//! ```
//!
//! It builds a one-clip project the way an import does, runs the same
//! `begin_export` the dialog runs, and prints the encoder that was selected.
//! Decode the result and compare it against the source: a valid container
//! proves nothing, which is exactly how a "valid MP4 of a synthesized gradient"
//! survived here.

#[cfg(not(feature = "gst"))]
fn main() {
    eprintln!("build with --features gst");
    std::process::exit(2);
}

#[cfg(feature = "gst")]
#[allow(
    clippy::disallowed_methods,
    reason = "the probe is single-threaded until begin_export runs: probing the clip only \
              parses its container header, so nothing reads the environment concurrently \
              when the output path is set"
)]
fn main() {
    use video_editor::export::{ExportSettings, ExportStatus, begin_export};
    use video_editor::model::{MediaItem, MediaKind, Project, Track, TrackKind};

    let mut args = std::env::args().skip(1);
    let clip = args.next().expect("usage: export_probe <clip> <out>");
    let out = args.next().expect("usage: export_probe <clip> <out>");

    let probed = video_editor::probe::probe_file(std::path::Path::new(&clip))
        .unwrap_or_else(|e| panic!("{clip}: {e}"));
    println!(
        "source: {clip}\n  {}x{} @ {:.3} fps, {:.2}s",
        probed.width, probed.height, probed.fps, probed.duration
    );

    let mut p = Project::new("export probe");
    p.width = probed.width.max(2);
    p.height = probed.height.max(2);
    let mid = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id: mid,
        name: "clip".into(),
        kind: MediaKind::Video,
        duration: probed.duration,
        width: probed.width,
        height: probed.height,
        fps: probed.fps,
        path: clip.clone(),
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
    // Two seconds is enough to compare pictures and short enough to run by hand.
    let cid = p.add_clip(tid, mid, 0.0).unwrap();
    p.clip_mut(cid).unwrap().out_point = 2.0_f64.min(probed.duration.max(0.5));

    // SAFETY: no other thread exists yet; the export reads this variable.
    unsafe { std::env::set_var("TILO_VE_EXPORT_OUT", &out) };
    let settings = ExportSettings::default();
    match begin_export(&p, &settings) {
        ExportStatus::Done { path } => {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            println!("wrote: {path} ({bytes} bytes)");
            println!(
                "\nA container check would stop here and prove nothing. Decode it back\n\
                 and compare it with the source before believing it carries the footage."
            );
        }
        other => {
            eprintln!("export did not produce a file: {other:?}");
            std::process::exit(1);
        }
    }
}
