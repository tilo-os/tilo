//! Attachment image loading for the chat surface.
//!
//! Image attachments are painted through the shared [`ui::ImageStore`], which
//! fetches + decodes + caches off the UI thread and hands back a GPU-ready
//! `peniko::ImageData` on a later frame. This module supplies the injected
//! [`ui::Fetcher`]: the transport that turns an attachment's blob key into raw
//! encoded bytes.
//!
//! Chat images are content-addressed bytes in the app's content store, not
//! peer-local file paths. This fetcher runs on `ImageStore` worker threads with
//! a clone of the app's store handle and returns only verified bytes.
//!
//! The one non-filesystem case is the `tilo-demo://` scheme used only by the
//! ephemeral `--shots`/demo seed: it synthesises a real PNG in memory so the
//! offline screenshot drives the exact same decode+cache+paint path as a real
//! stored image, without shipping binary fixtures or touching the network.

use std::io::Cursor;
use std::sync::Arc;

use tilo_content::ContentStore;
use ui::Fetcher;

/// URL scheme for the synthesised demo attachment thumbnails. Only the
/// ephemeral demo seed emits these; live attachments use content hashes.
pub const DEMO_SCHEME: &str = "tilo-demo://";
/// Prefix used for content hashes when they are handed to `ImageStore`.
pub const BLOB_SCHEME: &str = "tilo-blob://";

pub fn blob_key(hash: &str) -> String {
    if hash.starts_with(DEMO_SCHEME) {
        hash.to_string()
    } else {
        format!("{BLOB_SCHEME}{hash}")
    }
}

/// A believable "call overlay mockup" attachment for the demo conversation.
pub const DEMO_CALL_GRID: &str = "tilo-demo://call-grid";
/// A believable "shared screenshot" attachment for the demo `+` action.
pub const DEMO_SHARED_SHOT: &str = "tilo-demo://shared-shot";
/// The demo conversation's shared notes file (no picture to decode).
pub const DEMO_NOTES: &str = "tilo-demo://transport-notes";
/// The demo conversation's screen recording, whose poster frame is
/// synthesised like the demo pictures.
pub const DEMO_CLIP: &str = "tilo-demo://tile-promotion";

/// The image key of a video's poster frame, where one exists without a
/// video decoder: the demo recording has one; a stored clip has none until
/// the media engine decodes a frame.
pub fn poster_key(blob: &str) -> Option<String> {
    blob.starts_with(DEMO_SCHEME).then(|| blob.to_string())
}

/// The fetcher injected into the app's [`ui::ImageStore`]. Reads verified
/// bytes from `content` or synthesises a PNG for the demo scheme. Runs on the
/// image store's background worker threads.
pub fn fetcher(content: ContentStore) -> Fetcher {
    Arc::new(move |key: &str| {
        if let Some(id) = key.strip_prefix(DEMO_SCHEME) {
            return synth_demo_png(id);
        }
        key.strip_prefix(BLOB_SCHEME)
            .and_then(|hash| content.read(hash))
            .filter(|b| !b.is_empty())
    })
}

// ---------------------------------------------------------------------------
// Demo thumbnail synthesis (shots / ephemeral only)
// ---------------------------------------------------------------------------

/// Encode a real PNG for a demo attachment id, or `None` for an unknown id.
fn synth_demo_png(id: &str) -> Option<Vec<u8>> {
    let (w, h, buf) = match id {
        "call-grid" => call_grid(),
        "shared-shot" => shared_shot(),
        "tile-promotion" => tile_promotion_poster(),
        _ => return None,
    };
    encode_png(w, h, buf)
}

fn encode_png(w: u32, h: u32, rgba: Vec<u8>) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

/// A tidy 2×2 call grid mockup: dark canvas, four participant tiles, the
/// top-left one lit as the active speaker. Reads unmistakably as a rendered
/// design frame rather than a flat gradient.
fn call_grid() -> (u32, u32, Vec<u8>) {
    let (w, h) = (640u32, 360u32);
    let mut c = Canvas::new(w, h);
    c.vgradient([26, 28, 38], [17, 18, 26]);

    let margin = 26.0;
    let gap = 16.0;
    let tw = (f64::from(w) - 2.0 * margin - gap) / 2.0;
    let th = (f64::from(h) - 2.0 * margin - gap) / 2.0;
    let tiles = [
        (margin, margin, [58, 74, 120], [120, 158, 255], true),
        (
            margin + tw + gap,
            margin,
            [70, 60, 96],
            [176, 150, 244],
            false,
        ),
        (
            margin,
            margin + th + gap,
            [54, 84, 84],
            [120, 214, 200],
            false,
        ),
        (
            margin + tw + gap,
            margin + th + gap,
            [92, 72, 66],
            [240, 178, 150],
            false,
        ),
    ];
    for (x, y, base, accent, active) in tiles {
        let r = Rectf { x, y, w: tw, h: th };
        c.round_rect(r, 12.0, base);
        // A camera "surface" sheen across the upper half.
        c.round_rect(
            Rectf {
                x,
                y,
                w: tw,
                h: th * 0.5,
            },
            12.0,
            lighten(base, 18),
        );
        // Centred avatar disc.
        let cx = x + tw * 0.5;
        let cy = y + th * 0.46;
        c.disc(cx, cy, th * 0.24, accent);
        c.disc(cx, cy, th * 0.24 - 6.0, lighten(accent, 26));
        // Name plate.
        c.round_rect(
            Rectf {
                x: x + 12.0,
                y: y + th - 30.0,
                w: tw * 0.44,
                h: 18.0,
            },
            6.0,
            [12, 13, 18],
        );
        if active {
            c.stroke_round(r, 12.0, [88, 214, 140], 3.0);
        }
    }
    (w, h, c.into_rgba())
}

/// A "shared screenshot" attachment: a light document window with a header
/// bar and a few content rows — clearly a captured UI, not a gradient.
fn shared_shot() -> (u32, u32, Vec<u8>) {
    let (w, h) = (620u32, 400u32);
    let mut c = Canvas::new(w, h);
    c.vgradient([32, 34, 44], [22, 23, 31]);
    let card = Rectf {
        x: 28.0,
        y: 28.0,
        w: f64::from(w) - 56.0,
        h: f64::from(h) - 56.0,
    };
    c.round_rect(card, 16.0, [244, 245, 248]);
    // Header bar.
    c.round_rect(
        Rectf {
            x: card.x,
            y: card.y,
            w: card.w,
            h: 46.0,
        },
        16.0,
        [94, 122, 232],
    );
    for i in 0..3 {
        c.disc(
            card.x + 22.0 + f64::from(i) * 20.0,
            card.y + 23.0,
            6.0,
            [255, 255, 255],
        );
    }
    // Content rows.
    let mut ry = card.y + 74.0;
    for i in 0..6 {
        let rw = card.w * if i % 3 == 0 { 0.82 } else { 0.62 };
        c.round_rect(
            Rectf {
                x: card.x + 24.0,
                y: ry,
                w: rw,
                h: 16.0,
            },
            5.0,
            [212, 216, 224],
        );
        if i % 3 == 0 {
            c.round_rect(
                Rectf {
                    x: card.x + 24.0,
                    y: ry,
                    w: 90.0,
                    h: 16.0,
                },
                5.0,
                [126, 150, 240],
            );
        }
        ry += 38.0;
    }
    (w, h, c.into_rgba())
}

/// The poster frame of the demo screen recording: a desktop of window tiles
/// mid-way through promoting one of them, which grows out of its slot over
/// its neighbours.
fn tile_promotion_poster() -> (u32, u32, Vec<u8>) {
    let (w, h) = (640u32, 360u32);
    let mut c = Canvas::new(w, h);
    c.vgradient([40, 46, 62], [24, 27, 38]);
    let margin = 24.0;
    let gap = 14.0;
    let cols = 3.0;
    let rows = 2.0;
    let tw = (f64::from(w) - 2.0 * margin - (cols - 1.0) * gap) / cols;
    let th = (f64::from(h) - 2.0 * margin - (rows - 1.0) * gap) / rows;
    let palette = [
        [214, 220, 232],
        [62, 68, 86],
        [228, 222, 210],
        [58, 64, 80],
        [220, 226, 236],
        [70, 64, 82],
    ];
    for (i, base) in palette.iter().enumerate() {
        let col = (i % 3) as f64;
        let row = (i / 3) as f64;
        let r = Rectf {
            x: margin + col * (tw + gap),
            y: margin + row * (th + gap),
            w: tw,
            h: th,
        };
        window_tile(&mut c, r, *base);
    }
    // The tile being promoted: lifted out of the middle slot and part-way
    // to filling the screen.
    let grow = 0.42;
    let from = Rectf {
        x: margin + tw + gap,
        y: margin,
        w: tw,
        h: th,
    };
    let to = Rectf {
        x: margin,
        y: margin,
        w: f64::from(w) - 2.0 * margin,
        h: f64::from(h) - 2.0 * margin,
    };
    let lerp = |a: f64, b: f64| a + (b - a) * grow;
    let promoted = Rectf {
        x: lerp(from.x, to.x),
        y: lerp(from.y, to.y) + 10.0,
        w: lerp(from.w, to.w),
        h: lerp(from.h, to.h),
    };
    c.round_rect(
        Rectf {
            x: promoted.x - 3.0,
            y: promoted.y - 3.0,
            w: promoted.w + 6.0,
            h: promoted.h + 6.0,
        },
        14.0,
        [18, 20, 28],
    );
    window_tile(&mut c, promoted, [244, 245, 248]);
    (w, h, c.into_rgba())
}

/// One window tile on the poster: its body, a header strip and a few lines
/// of content in a tone that reads on the body.
fn window_tile(c: &mut Canvas, r: Rectf, base: [u8; 3]) {
    let light = u16::from(base[0]) + u16::from(base[1]) + u16::from(base[2]) > 384;
    let line = if light {
        [196, 200, 212]
    } else {
        [92, 98, 118]
    };
    c.round_rect(r, 10.0, base);
    c.round_rect(
        Rectf {
            x: r.x,
            y: r.y,
            w: r.w,
            h: (r.h * 0.16).max(12.0),
        },
        10.0,
        lighten(base, if light { -14 } else { 14 }),
    );
    let mut y = r.y + r.h * 0.28;
    for i in 0..4 {
        let share = if i % 2 == 0 { 0.72 } else { 0.5 };
        c.round_rect(
            Rectf {
                x: r.x + r.w * 0.08,
                y,
                w: r.w * share,
                h: (r.h * 0.06).max(4.0),
            },
            3.0,
            line,
        );
        y += r.h * 0.15;
    }
}

// ---------------------------------------------------------------------------
// Tiny software raster canvas (enough for the demo thumbnails)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rectf {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            px: vec![0u8; (w * h * 4) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, rgb: [u8; 3], a: f64) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || a <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let a = a.clamp(0.0, 1.0);
        let Some(px) = self.px.get_mut(i..i + 4) else {
            return;
        };
        for (dst, src) in px.iter_mut().zip(rgb) {
            *dst = (f64::from(src) * a + f64::from(*dst) * (1.0 - a)) as u8;
        }
        if let Some(alpha) = self.px.get_mut(i + 3) {
            *alpha = 255;
        }
    }

    fn vgradient(&mut self, top: [u8; 3], bottom: [u8; 3]) {
        for y in 0..self.h {
            let t = f64::from(y) / f64::from(self.h.max(1) - 1);
            let rgb = [
                lerp(top[0], bottom[0], t),
                lerp(top[1], bottom[1], t),
                lerp(top[2], bottom[2], t),
            ];
            for x in 0..self.w {
                self.blend(x as i32, y as i32, rgb, 1.0);
            }
        }
    }

    /// Filled rounded rectangle with anti-aliased corners.
    fn round_rect(&mut self, r: Rectf, radius: f64, rgb: [u8; 3]) {
        let rad = radius.min(r.w * 0.5).min(r.h * 0.5).max(0.0);
        let x0 = r.x.floor() as i32;
        let y0 = r.y.floor() as i32;
        let x1 = (r.x + r.w).ceil() as i32;
        let y1 = (r.y + r.h).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let a = Self::round_coverage(r, rad, f64::from(x) + 0.5, f64::from(y) + 0.5);
                self.blend(x, y, rgb, a);
            }
        }
    }

    fn stroke_round(&mut self, r: Rectf, radius: f64, rgb: [u8; 3], width: f64) {
        let rad = radius.min(r.w * 0.5).min(r.h * 0.5).max(0.0);
        let inner = Rectf {
            x: r.x + width,
            y: r.y + width,
            w: r.w - 2.0 * width,
            h: r.h - 2.0 * width,
        };
        let x0 = r.x.floor() as i32;
        let y0 = r.y.floor() as i32;
        let x1 = (r.x + r.w).ceil() as i32;
        let y1 = (r.y + r.h).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let fx = f64::from(x) + 0.5;
                let fy = f64::from(y) + 0.5;
                let outer = Self::round_coverage(r, rad, fx, fy);
                let hole = Self::round_coverage(inner, (rad - width).max(0.0), fx, fy);
                self.blend(x, y, rgb, (outer - hole).max(0.0));
            }
        }
    }

    /// Coverage (0..1) of a point inside a rounded rect, softened at edges.
    fn round_coverage(r: Rectf, rad: f64, px: f64, py: f64) -> f64 {
        // Distance outside the rounded rectangle (0 inside).
        let dx = (r.x + rad - px).max(px - (r.x + r.w - rad)).max(0.0);
        let dy = (r.y + rad - py).max(py - (r.y + r.h - rad)).max(0.0);
        // Straight edges (outside the corner boxes).
        let inside_x = px >= r.x && px <= r.x + r.w;
        let inside_y = py >= r.y && py <= r.y + r.h;
        if !inside_x || !inside_y {
            return 0.0;
        }
        if dx == 0.0 || dy == 0.0 {
            return 1.0;
        }
        let d = (dx * dx + dy * dy).sqrt();
        (rad + 0.5 - d).clamp(0.0, 1.0)
    }

    fn disc(&mut self, cx: f64, cy: f64, rad: f64, rgb: [u8; 3]) {
        let x0 = (cx - rad).floor() as i32;
        let y0 = (cy - rad).floor() as i32;
        let x1 = (cx + rad).ceil() as i32;
        let y1 = (cy + rad).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let d =
                    ((f64::from(x) + 0.5 - cx).powi(2) + (f64::from(y) + 0.5 - cy).powi(2)).sqrt();
                self.blend(x, y, rgb, (rad + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    fn into_rgba(self) -> Vec<u8> {
        self.px
    }
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u8
}

fn lighten(rgb: [u8; 3], by: i32) -> [u8; 3] {
    [
        (i32::from(rgb[0]) + by).clamp(0, 255) as u8,
        (i32::from(rgb[1]) + by).clamp(0, 255) as u8,
        (i32::from(rgb[2]) + by).clamp(0, 255) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetcher_decodes_demo_images_and_returns_none_for_unknown_sources() {
        let bytes = fetcher(ContentStore::memory())(DEMO_CALL_GRID).expect("demo image bytes");
        // Must be a real PNG the ui ImageStore can decode.
        let img = image::load_from_memory(&bytes).expect("valid png");
        assert!(img.width() > 0 && img.height() > 0, "case demo call grid");
        assert!(
            fetcher(ContentStore::memory())(DEMO_SHARED_SHOT).is_some(),
            "case demo shared shot"
        );

        // Missing file is None, not a panic.
        assert!(
            fetcher(ContentStore::memory())("/var/workspace/does/not/exist.png").is_none(),
            "case missing file"
        );
        assert!(
            fetcher(ContentStore::memory())("tilo-demo://unknown").is_none(),
            "case unknown demo scheme"
        );
    }
}
