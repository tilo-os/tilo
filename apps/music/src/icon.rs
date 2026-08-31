//! Hand-drawn vector icons.
//!
//! The toolkit has no icon font, so the chrome (nav rail, transport
//! controls, badges) draws its glyphs directly into the Vello scene. Every
//! icon is centred in a logical `size`×`size` box at `center` and tinted
//! with the caller's (theme-derived) color, so icons inherit the OS theme
//! exactly like text does.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Shape, Stroke};
use vello::peniko::{Color, Fill};

/// Identifier for each glyph the app needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Home,
    Search,
    Songs,
    Albums,
    Artists,
    Playlist,
    Queue,
    Download,
    Downloaded,
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Heart,
    HeartFilled,
    VolumeHigh,
    VolumeMute,
    Plus,
    ChevronLeft,
    Note,
    Disc,
}

fn stroke(scene: &mut Scene, t: Affine, color: Color, w: f64, path: &BezPath) {
    scene.stroke(
        &Stroke::new(w).with_caps(vello::kurbo::Cap::Round),
        t,
        color,
        None,
        path,
    );
}

/// Shared centre + half-extent geometry for one icon draw, threaded through
/// the per-glyph draw helpers below so each keeps a 6-argument signature
/// instead of carrying `cx`/`cy`/`half` separately. Part of [`draw`].
#[derive(Clone, Copy)]
struct IconGeom {
    cx: f64,
    cy: f64,
    half: f64,
}

/// Draw `icon` centred at `center` within a `size`×`size` box.
pub fn draw(scene: &mut Scene, t: Affine, icon: Icon, center: Point, size: f64, color: Color) {
    let s = size;
    let half = s / 2.0;
    let cx = center.x;
    let cy = center.y;
    let lw = (s * 0.11).max(1.4);
    let geo = IconGeom { cx, cy, half };

    match icon {
        Icon::Play | Icon::Pause | Icon::Next | Icon::Prev => {
            draw_transport(scene, t, icon, color, geo, s);
        }
        Icon::Home | Icon::Search => draw_nav(scene, t, icon, color, geo, lw),
        Icon::Songs | Icon::Queue | Icon::Albums | Icon::Disc | Icon::Artists | Icon::Playlist => {
            draw_library(scene, t, icon, color, geo, lw);
        }
        Icon::Download | Icon::Downloaded => {
            draw_download(scene, t, icon, color, geo, lw);
        }
        Icon::Shuffle | Icon::Repeat | Icon::RepeatOne => {
            draw_shuffle_repeat(scene, t, icon, color, geo, lw);
        }
        Icon::Heart | Icon::HeartFilled | Icon::VolumeHigh | Icon::VolumeMute => {
            draw_heart_volume(scene, t, icon, color, geo, lw);
        }
        Icon::Plus | Icon::ChevronLeft | Icon::Note => {
            draw_misc(scene, t, icon, color, geo, lw);
        }
    }
}

/// Play, Pause, Next, Prev.
fn draw_transport(scene: &mut Scene, t: Affine, icon: Icon, color: Color, geo: IconGeom, s: f64) {
    let IconGeom { cx, cy, half } = geo;
    match icon {
        Icon::Play => {
            let mut p = BezPath::new();
            p.move_to((cx - half * 0.55, cy - half * 0.78));
            p.line_to((cx + half * 0.82, cy));
            p.line_to((cx - half * 0.55, cy + half * 0.78));
            p.close_path();
            scene.fill(Fill::NonZero, t, color, None, &p);
        }
        Icon::Pause => {
            let bw = s * 0.20;
            let bh = s * 0.78;
            let r1 = vello::kurbo::Rect::new(
                cx - s * 0.30,
                cy - bh / 2.0,
                cx - s * 0.30 + bw,
                cy + bh / 2.0,
            );
            let r2 = vello::kurbo::Rect::new(
                cx + s * 0.10,
                cy - bh / 2.0,
                cx + s * 0.10 + bw,
                cy + bh / 2.0,
            );
            scene.fill(Fill::NonZero, t, color, None, &r1);
            scene.fill(Fill::NonZero, t, color, None, &r2);
        }
        _ => {
            // Next | Prev
            let dir = if icon == Icon::Next { 1.0 } else { -1.0 };
            let mut tri = BezPath::new();
            tri.move_to((cx - dir * half * 0.7, cy - half * 0.62));
            tri.line_to((cx + dir * half * 0.35, cy));
            tri.line_to((cx - dir * half * 0.7, cy + half * 0.62));
            tri.close_path();
            scene.fill(Fill::NonZero, t, color, None, &tri);
            let bar_x = cx + dir * half * 0.55;
            let bar = vello::kurbo::Rect::new(
                bar_x - s * 0.07,
                cy - half * 0.62,
                bar_x + s * 0.07,
                cy + half * 0.62,
            );
            scene.fill(Fill::NonZero, t, color, None, &bar);
        }
    }
}

/// Home, Search.
fn draw_nav(scene: &mut Scene, t: Affine, icon: Icon, color: Color, geo: IconGeom, lw: f64) {
    let IconGeom { cx, cy, half } = geo;
    if icon == Icon::Home {
        let mut roof = BezPath::new();
        roof.move_to((cx - half * 0.8, cy - half * 0.05));
        roof.line_to((cx, cy - half * 0.75));
        roof.line_to((cx + half * 0.8, cy - half * 0.05));
        stroke(scene, t, color, lw, &roof);
        let body = vello::kurbo::Rect::new(
            cx - half * 0.55,
            cy - half * 0.05,
            cx + half * 0.55,
            cy + half * 0.7,
        );
        scene.stroke(&Stroke::new(lw), t, color, None, &body);
    } else {
        // Search
        let r = half * 0.55;
        let glass = Circle::new(Point::new(cx - half * 0.15, cy - half * 0.15), r);
        scene.stroke(&Stroke::new(lw), t, color, None, &glass);
        let handle = Line::new(
            (cx - half * 0.15 + r * 0.7, cy - half * 0.15 + r * 0.7),
            (cx + half * 0.7, cy + half * 0.7),
        );
        scene.stroke(
            &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
            t,
            color,
            None,
            &handle,
        );
    }
}

/// Songs, Queue, Albums, Disc, Artists, Playlist.
fn draw_library(scene: &mut Scene, t: Affine, icon: Icon, color: Color, geo: IconGeom, lw: f64) {
    let IconGeom { cx, cy, half } = geo;
    match icon {
        Icon::Songs | Icon::Queue => {
            for (i, frac) in [(-0.6f64), 0.0, 0.6].iter().enumerate() {
                let y = cy + half * frac;
                let x1 = cx - half * 0.75;
                let x2 = if icon == Icon::Queue && i == 2 {
                    cx + half * 0.1
                } else {
                    cx + half * 0.75
                };
                let l = Line::new((x1, y), (x2, y));
                scene.stroke(
                    &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
                    t,
                    color,
                    None,
                    &l,
                );
            }
        }
        Icon::Albums | Icon::Disc => {
            let outer = Circle::new(Point::new(cx, cy), half * 0.82);
            scene.stroke(&Stroke::new(lw), t, color, None, &outer);
            let inner = Circle::new(Point::new(cx, cy), half * 0.16);
            scene.fill(Fill::NonZero, t, color, None, &inner);
        }
        Icon::Artists => {
            let headd = Circle::new(Point::new(cx, cy - half * 0.35), half * 0.32);
            scene.stroke(&Stroke::new(lw), t, color, None, &headd);
            let mut body = BezPath::new();
            body.move_to((cx - half * 0.6, cy + half * 0.75));
            body.curve_to(
                (cx - half * 0.6, cy + half * 0.1),
                (cx + half * 0.6, cy + half * 0.1),
                (cx + half * 0.6, cy + half * 0.75),
            );
            stroke(scene, t, color, lw, &body);
        }
        _ => {
            // Playlist
            for frac in [-0.6f64, -0.1, 0.4] {
                let y = cy + half * frac;
                let l = Line::new((cx - half * 0.75, y), (cx + half * 0.3, y));
                scene.stroke(
                    &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
                    t,
                    color,
                    None,
                    &l,
                );
            }
            // little note at the right
            let note = Circle::new(Point::new(cx + half * 0.45, cy + half * 0.55), half * 0.2);
            scene.fill(Fill::NonZero, t, color, None, &note);
            let stem = Line::new(
                (cx + half * 0.45 + half * 0.2, cy + half * 0.55),
                (cx + half * 0.45 + half * 0.2, cy - half * 0.2),
            );
            scene.stroke(&Stroke::new(lw), t, color, None, &stem);
        }
    }
}

/// Download, Downloaded.
fn draw_download(scene: &mut Scene, t: Affine, icon: Icon, color: Color, geo: IconGeom, lw: f64) {
    let IconGeom { cx, cy, half } = geo;
    let arrow = Line::new((cx, cy - half * 0.7), (cx, cy + half * 0.2));
    scene.stroke(
        &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
        t,
        color,
        None,
        &arrow,
    );
    let mut head = BezPath::new();
    head.move_to((cx - half * 0.35, cy - half * 0.15));
    head.line_to((cx, cy + half * 0.25));
    head.line_to((cx + half * 0.35, cy - half * 0.15));
    stroke(scene, t, color, lw, &head);
    if icon == Icon::Downloaded {
        let tray = vello::kurbo::Rect::new(
            cx - half * 0.7,
            cy + half * 0.45,
            cx + half * 0.7,
            cy + half * 0.7,
        );
        scene.fill(Fill::NonZero, t, color, None, &tray);
    } else {
        let mut tray = BezPath::new();
        tray.move_to((cx - half * 0.7, cy + half * 0.4));
        tray.line_to((cx - half * 0.7, cy + half * 0.72));
        tray.line_to((cx + half * 0.7, cy + half * 0.72));
        tray.line_to((cx + half * 0.7, cy + half * 0.4));
        stroke(scene, t, color, lw, &tray);
    }
}

/// Shuffle, Repeat, `RepeatOne`.
fn draw_shuffle_repeat(
    scene: &mut Scene,
    t: Affine,
    icon: Icon,
    color: Color,
    geo: IconGeom,
    lw: f64,
) {
    let IconGeom { cx, cy, half } = geo;
    if icon == Icon::Shuffle {
        for sign in [-1.0f64, 1.0] {
            let mut p = BezPath::new();
            p.move_to((cx - half * 0.75, cy + sign * half * 0.45));
            p.curve_to(
                (cx - half * 0.1, cy + sign * half * 0.45),
                (cx + half * 0.1, cy - sign * half * 0.45),
                (cx + half * 0.6, cy - sign * half * 0.45),
            );
            stroke(scene, t, color, lw, &p);
            // arrow head
            let hx = cx + half * 0.6;
            let hy = cy - sign * half * 0.45;
            let mut a = BezPath::new();
            a.move_to((hx - half * 0.28, hy - half * 0.18));
            a.line_to((hx + half * 0.05, hy));
            a.line_to((hx - half * 0.28, hy + half * 0.18));
            stroke(scene, t, color, lw, &a);
        }
        return;
    }
    // Repeat | RepeatOne
    let r = half * 0.6;
    // A near-full circular arc, leaving a gap at the right where the
    // arrow head sits — reads as the classic "loop" glyph.
    let arc = vello::kurbo::Arc::new(
        Point::new(cx, cy),
        (r, r),
        -1.0,                       // start angle (radians)
        std::f64::consts::PI * 1.7, // sweep
        0.0,
    );
    scene.stroke(
        &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
        t,
        color,
        None,
        &arc.to_path(0.2),
    );
    // Arrow head at the arc's end (top-right gap).
    let ex = cx + r * (-1.0f64).cos();
    let ey = cy + r * (-1.0f64).sin();
    let mut a = BezPath::new();
    a.move_to((ex - half * 0.34, ey - half * 0.1));
    a.line_to((ex + half * 0.04, ey));
    a.line_to((ex - half * 0.16, ey + half * 0.34));
    stroke(scene, t, color, lw, &a);
    if icon == Icon::RepeatOne {
        let one = Line::new((cx, cy - half * 0.24), (cx, cy + half * 0.24));
        scene.stroke(
            &Stroke::new(lw * 1.2).with_caps(vello::kurbo::Cap::Round),
            t,
            color,
            None,
            &one,
        );
    }
}

/// Heart, `HeartFilled`, `VolumeHigh`, `VolumeMute`.
fn draw_heart_volume(
    scene: &mut Scene,
    t: Affine,
    icon: Icon,
    color: Color,
    geo: IconGeom,
    lw: f64,
) {
    let IconGeom { cx, cy, half } = geo;
    if icon == Icon::Heart || icon == Icon::HeartFilled {
        let mut p = BezPath::new();
        p.move_to((cx, cy + half * 0.6));
        p.curve_to(
            (cx - half * 1.1, cy - half * 0.1),
            (cx - half * 0.45, cy - half * 0.8),
            (cx, cy - half * 0.25),
        );
        p.curve_to(
            (cx + half * 0.45, cy - half * 0.8),
            (cx + half * 1.1, cy - half * 0.1),
            (cx, cy + half * 0.6),
        );
        p.close_path();
        if icon == Icon::HeartFilled {
            scene.fill(Fill::NonZero, t, color, None, &p);
        } else {
            stroke(scene, t, color, lw, &p);
        }
        return;
    }
    // VolumeHigh | VolumeMute
    let mut spk = BezPath::new();
    spk.move_to((cx - half * 0.75, cy - half * 0.25));
    spk.line_to((cx - half * 0.35, cy - half * 0.25));
    spk.line_to((cx + half * 0.05, cy - half * 0.6));
    spk.line_to((cx + half * 0.05, cy + half * 0.6));
    spk.line_to((cx - half * 0.35, cy + half * 0.25));
    spk.line_to((cx - half * 0.75, cy + half * 0.25));
    spk.close_path();
    scene.fill(Fill::NonZero, t, color, None, &spk);
    if icon == Icon::VolumeHigh {
        for rr in [0.9f64, 1.4] {
            let arc = vello::kurbo::Arc::new(
                Point::new(cx + half * 0.05, cy),
                (half * rr, half * rr),
                -0.7,
                1.4,
                0.0,
            );
            scene.stroke(
                &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
                t,
                color,
                None,
                &arc.to_path(0.2),
            );
        }
    } else {
        let x = Line::new(
            (cx + half * 0.3, cy - half * 0.3),
            (cx + half * 0.75, cy + half * 0.3),
        );
        scene.stroke(&Stroke::new(lw), t, color, None, &x);
        let x2 = Line::new(
            (cx + half * 0.75, cy - half * 0.3),
            (cx + half * 0.3, cy + half * 0.3),
        );
        scene.stroke(&Stroke::new(lw), t, color, None, &x2);
    }
}

/// Plus, `ChevronLeft`, Note.
fn draw_misc(scene: &mut Scene, t: Affine, icon: Icon, color: Color, geo: IconGeom, lw: f64) {
    let IconGeom { cx, cy, half } = geo;
    match icon {
        Icon::Plus => {
            let hl = Line::new((cx - half * 0.7, cy), (cx + half * 0.7, cy));
            let vl = Line::new((cx, cy - half * 0.7), (cx, cy + half * 0.7));
            scene.stroke(
                &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
                t,
                color,
                None,
                &hl,
            );
            scene.stroke(
                &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
                t,
                color,
                None,
                &vl,
            );
        }
        Icon::ChevronLeft => {
            let mut p = BezPath::new();
            p.move_to((cx + half * 0.35, cy - half * 0.6));
            p.line_to((cx - half * 0.35, cy));
            p.line_to((cx + half * 0.35, cy + half * 0.6));
            stroke(scene, t, color, lw, &p);
        }
        _ => {
            // Note
            let head = vello::kurbo::Ellipse::new(
                Point::new(cx - half * 0.25, cy + half * 0.45),
                (half * 0.3, half * 0.22),
                0.0,
            );
            scene.fill(Fill::NonZero, t, color, None, &head);
            let stem = Line::new(
                (cx + half * 0.04, cy + half * 0.45),
                (cx + half * 0.04, cy - half * 0.6),
            );
            scene.stroke(&Stroke::new(lw), t, color, None, &stem);
            let flag = Line::new(
                (cx + half * 0.04, cy - half * 0.6),
                (cx + half * 0.5, cy - half * 0.4),
            );
            scene.stroke(
                &Stroke::new(lw).with_caps(vello::kurbo::Cap::Round),
                t,
                color,
                None,
                &flag,
            );
        }
    }
}
