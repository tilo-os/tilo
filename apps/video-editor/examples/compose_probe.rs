#![allow(
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::too_many_lines,
    clippy::unwrap_used,
    reason = "diagnostic compositor probe intentionally favours a compact command-line fixture"
)]

//! Render composite frames straight to PNG, for looking at.
//!
//! The unit tests assert on individual pixels; this writes the whole frame
//! so a person can see what the geometry, the titles and the keyframes
//! actually produce. It runs the production compositor — the same one the
//! preview and the export call — so what it writes is what those draw.
//!
//! ```text
//! cargo run -p video-editor --example compose_probe -- OUTDIR
//! ```

use video_editor::gpu::{HeadlessCompositor, NoSources};
use video_editor::model::{
    Clip, MediaItem, MediaKind, Project, TextAlign, TextSource, Track, TrackKind, Transform,
};
use video_editor::param::{Interp, Param};

fn png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    ui::png::write_rgba8(path, w, h, rgba).expect("write png");
    println!("wrote {}", path.display());
}

fn media(p: &mut Project, name: &str, kind: MediaKind, palette: u8, w: u32, h: u32) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id,
        name: name.into(),
        kind,
        duration: 30.0,
        width: w,
        height: h,
        fps: 30.0,
        palette,
        path: video_editor::media::workspace_path(name),
        ..Default::default()
    });
    id
}

fn track(p: &mut Project, index: u32) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    p.tracks.push(Track {
        id,
        name: format!("V{index}"),
        kind: TrackKind::Video,
        index,
        ..Default::default()
    });
    id
}

fn place(p: &mut Project, t: uuid::Uuid, m: uuid::Uuid, start: f64, len: f64) -> uuid::Uuid {
    let id = p.add_clip(t, m, start).expect("media exists");
    if let Some(c) = p.clip_mut(id) {
        c.out_point = len;
    }
    id
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("target/compose-probe"));
    std::fs::create_dir_all(&out).expect("create outdir");

    let mut c = match HeadlessCompositor::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no wgpu adapter: {e}");
            std::process::exit(1);
        }
    };
    let (w, h) = (640u32, 360u32);

    // A bed, an inset, and a title over both.
    let mut p = Project::new("probe");
    let bed = media(&mut p, "bed.mp4", MediaKind::Video, 0, 1920, 1080);
    let inset = media(&mut p, "inset.mp4", MediaKind::Video, 3, 1920, 1080);
    let v1 = track(&mut p, 1);
    let v2 = track(&mut p, 2);
    let v3 = track(&mut p, 3);
    place(&mut p, v1, bed, 0.0, 10.0);
    let pip = place(&mut p, v2, inset, 0.0, 10.0);
    p.clip_mut(pip).unwrap().transform = Transform::pip(0.32, (1.0, -1.0), 0.04);

    let tid = uuid::Uuid::new_v4();
    p.media.push(MediaItem {
        id: tid,
        name: "Title".into(),
        kind: MediaKind::Text,
        duration: 10.0,
        palette: 5,
        text: Some(TextSource {
            content: "COASTLINE\nepisode two".into(),
            size: 0.16,
            color: [1.0, 1.0, 1.0, 1.0],
            align: TextAlign::Left,
            bold: true,
            ..TextSource::default()
        }),
        ..Default::default()
    });
    let title = place(&mut p, v3, tid, 0.0, 10.0);
    // Lower third: shrink and drop it.
    p.clip_mut(title).unwrap().transform = Transform {
        scale_x: 0.62,
        scale_y: 0.62,
        pos_x: -0.10,
        pos_y: 0.26,
        ..Transform::default()
    };
    png(
        &out.join("01-pip-and-title.png"),
        w,
        h,
        &c.frame(&p, 2.0, w, h, &mut NoSources),
    );

    // The same frame with the inset rotated and cropped, to see both.
    p.clip_mut(pip).unwrap().transform = Transform {
        rotation: -12.0,
        crop_left: 0.12,
        crop_right: 0.12,
        ..Transform::pip(0.32, (1.0, -1.0), 0.04)
    };
    png(
        &out.join("02-rotated-cropped-inset.png"),
        w,
        h,
        &c.frame(&p, 2.0, w, h, &mut NoSources),
    );

    // Keyframes: the inset flies in and grows across four seconds. Three
    // instants of the same animated clip.
    p.clip_mut(pip).unwrap().transform = Transform::default();
    {
        let clip = p.clip_mut(pip).unwrap();
        for (param, keys) in [
            (Param::ScaleX, [(0.0, 0.12), (4.0, 0.45)]),
            (Param::ScaleY, [(0.0, 0.12), (4.0, 0.45)]),
            (Param::PosX, [(0.0, -0.40), (4.0, 0.24)]),
            (Param::PosY, [(0.0, 0.34), (4.0, -0.22)]),
            (Param::Rotation, [(0.0, -25.0), (4.0, 0.0)]),
        ] {
            for (t, v) in keys {
                clip.set_key_at(param, t, v, Interp::Ease);
            }
        }
    }
    for (i, at) in [0.0f64, 2.0, 4.0].iter().enumerate() {
        png(
            &out.join(format!("03-keyframed-{i}-at-{at}s.png")),
            w,
            h,
            &c.frame(&p, *at, w, h, &mut NoSources),
        );
    }

    // Speed: the same clip at 1x and 3x, sampled at the same instant, to
    // show the source time has moved on further.
    let mut sp = Project::new("speed");
    let m = media(&mut sp, "src.mp4", MediaKind::Video, 2, 1920, 1080);
    let t = track(&mut sp, 1);
    let cid = place(&mut sp, t, m, 0.0, 12.0);
    let normal = sp.clip(cid).unwrap().source_time(3.0);
    sp.clip_mut(cid).unwrap().set_speed(3.0, false);
    let fast = sp.clip(cid).unwrap().source_time(3.0);
    println!(
        "speed: at 3s the 1x clip shows source {normal:.2}s, the 3x clip shows {fast:.2}s; \
         timeline length {:.2}s",
        sp.clip(cid).unwrap().duration()
    );
    let _ = Clip::default();
}
