//! Icon + screenshot image loading for the store.
//!
//! App icons and screenshots are painted through the shared [`ui::ImageStore`],
//! which fetches + decodes + caches off the UI thread and hands back a
//! GPU-ready `peniko::ImageData` on a later frame, showing an honest loading
//! skeleton until it resolves. This module supplies the injected
//! [`ui::Fetcher`]: the transport that turns an icon/screenshot URL into raw
//! encoded bytes.
//!
//! Two transports live behind one closure:
//!
//! * **`http(s)://…`** — a real Flathub CDN icon or screenshot. Fetched with
//!   `curl` on the store's background image threads (the toolkit stays
//!   HTTP-free; the app injects the transport). A miss degrades to an honest
//!   fallback, never a fake image.
//! * **`qos-demo://…`** — synthesised in-memory PNGs used only by the bundled
//!   sample catalog + the offline `--shots` path, so a screenshot drives the
//!   exact same fetch→decode→paint pipeline a real Flathub image would,
//!   deterministically and without the network. This is what makes the
//!   offline detail-page shot show a **real decoded** icon + screenshots
//!   rather than a permanent placeholder.

use std::io::Cursor;
use std::sync::Arc;

use ui::Fetcher;

/// URL scheme for synthesised demo images (icons + screenshots). Only the
/// sample catalog + the `--shots`/demo path emit these; the live app uses
/// real Flathub CDN URLs.
pub const DEMO_SCHEME: &str = "qos-demo://";

/// Build a `qos-demo://` icon URL for an app id (sample catalog only).
pub fn demo_icon(id: &str) -> String {
    format!("{DEMO_SCHEME}icon/{id}")
}

/// Build a `qos-demo://` screenshot URL for an app id + shot index.
pub fn demo_shot(id: &str, index: usize) -> String {
    format!("{DEMO_SCHEME}shot/{id}/{index}")
}

/// The fetcher injected into the store's [`ui::ImageStore`]. Runs on the
/// store's background worker threads, so it may block on `curl`.
pub fn fetcher() -> Fetcher {
    Arc::new(|key: &str| {
        if let Some(rest) = key.strip_prefix(DEMO_SCHEME) {
            return synth_demo_png(rest);
        }
        if key.starts_with("http://") || key.starts_with("https://") {
            return crate::flatpak::http_get_bytes(key);
        }
        None
    })
}

// ---------------------------------------------------------------------------
// Demo image synthesis (sample catalog / shots only)
// ---------------------------------------------------------------------------

/// Encode a real PNG for a demo key of the form `icon/<id>` or
/// `shot/<id>/<index>`, or `None` for an unknown shape.
fn synth_demo_png(rest: &str) -> Option<Vec<u8>> {
    let mut parts = rest.splitn(2, '/');
    let kind = parts.next()?;
    let tail = parts.next().unwrap_or("");
    let (w, h, buf) = match kind {
        "icon" => icon_png(tail),
        "shot" => {
            // tail = "<id>/<index>"; fold the whole thing into the seed so
            // each shot of each app is visually distinct.
            shot_png(tail)
        }
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

/// A believable app icon: a rounded colour tile with a diagonal sheen and a
/// centred emblem, seeded from the app id so each app reads as distinct.
fn icon_png(seed: &str) -> (u32, u32, Vec<u8>) {
    let (w, h) = (128u32, 128u32);
    let mut c = Canvas::new(w, h);
    let hue = hash(seed);
    let base = palette(hue, 0);
    let accent = palette(hue, 1);
    // Rounded tile.
    c.round_rect(Rectf { x: 6.0, y: 6.0, w: 116.0, h: 116.0 }, 28.0, base);
    // Upper sheen.
    c.round_rect(Rectf { x: 6.0, y: 6.0, w: 116.0, h: 58.0 }, 28.0, lighten(base, 22));
    // Centred emblem: two overlapping discs + a bar, an abstract "app" mark.
    c.disc(52.0, 60.0, 24.0, accent);
    c.disc(78.0, 68.0, 20.0, lighten(accent, 28));
    c.round_rect(Rectf { x: 40.0, y: 90.0, w: 48.0, h: 12.0 }, 6.0, [246, 247, 250]);
    (w, h, c.into_rgba())
}

/// A believable app screenshot: a light document window with a coloured
/// header bar and a few content rows, seeded so each shot differs.
fn shot_png(seed: &str) -> (u32, u32, Vec<u8>) {
    let (w, h) = (640u32, 400u32);
    let mut c = Canvas::new(w, h);
    let hue = hash(seed);
    c.vgradient(shade(palette(hue, 0), -40), shade(palette(hue, 0), -70));
    let card = Rectf { x: 26.0, y: 26.0, w: w as f64 - 52.0, h: h as f64 - 52.0 };
    c.round_rect(card, 16.0, [244, 245, 248]);
    // Header bar in the app's accent.
    c.round_rect(Rectf { x: card.x, y: card.y, w: card.w, h: 48.0 }, 16.0, palette(hue, 1));
    for i in 0..3 {
        c.disc(card.x + 22.0 + i as f64 * 20.0, card.y + 24.0, 6.0, [255, 255, 255]);
    }
    // A side rail + content rows.
    c.round_rect(Rectf { x: card.x + 18.0, y: card.y + 66.0, w: 120.0, h: card.h - 84.0 }, 10.0, [226, 229, 236]);
    let mut ry = card.y + 74.0;
    for i in 0..6 {
        let rw = (card.w - 190.0) * if i % 3 == 0 { 0.92 } else { 0.66 };
        c.round_rect(Rectf { x: card.x + 154.0, y: ry, w: rw, h: 16.0 }, 5.0, [212, 216, 224]);
        if i % 3 == 0 {
            c.round_rect(Rectf { x: card.x + 154.0, y: ry, w: 96.0, h: 16.0 }, 5.0, palette(hue, 1));
        }
        ry += 40.0;
    }
    (w, h, c.into_rgba())
}

/// A stable small hash of a string, for deterministic per-app colours.
fn hash(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// A pleasant seeded colour pair. `variant` 0 = base tile, 1 = accent.
fn palette(seed: u32, variant: u32) -> [u8; 3] {
    // Six curated base hues so demo icons look designed, not random mud.
    const BASES: [[u8; 3]; 6] = [
        [88, 101, 242],  // indigo
        [37, 155, 122],  // teal
        [214, 108, 74],  // terracotta
        [155, 89, 182],  // violet
        [52, 152, 219],  // sky
        [225, 160, 60],  // amber
    ];
    let base = BASES[(seed % BASES.len() as u32) as usize];
    if variant == 0 {
        base
    } else {
        lighten(base, 40)
    }
}

fn shade(rgb: [u8; 3], by: i32) -> [u8; 3] {
    [
        (rgb[0] as i32 + by).clamp(0, 255) as u8,
        (rgb[1] as i32 + by).clamp(0, 255) as u8,
        (rgb[2] as i32 + by).clamp(0, 255) as u8,
    ]
}

fn lighten(rgb: [u8; 3], by: i32) -> [u8; 3] {
    shade(rgb, by)
}

// ---------------------------------------------------------------------------
// Tiny software raster canvas (enough for the demo images)
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
        Self { w, h, px: vec![0u8; (w * h * 4) as usize] }
    }

    fn blend(&mut self, x: i32, y: i32, rgb: [u8; 3], a: f64) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || a <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let a = a.clamp(0.0, 1.0);
        for k in 0..3 {
            let dst = self.px[i + k] as f64;
            self.px[i + k] = (rgb[k] as f64 * a + dst * (1.0 - a)) as u8;
        }
        self.px[i + 3] = 255;
    }

    fn vgradient(&mut self, top: [u8; 3], bottom: [u8; 3]) {
        for y in 0..self.h {
            let t = y as f64 / (self.h.max(1) - 1) as f64;
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

    fn round_rect(&mut self, r: Rectf, radius: f64, rgb: [u8; 3]) {
        let rad = radius.min(r.w * 0.5).min(r.h * 0.5).max(0.0);
        let x0 = r.x.floor() as i32;
        let y0 = r.y.floor() as i32;
        let x1 = (r.x + r.w).ceil() as i32;
        let y1 = (r.y + r.h).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let a = self.round_coverage(r, rad, x as f64 + 0.5, y as f64 + 0.5);
                self.blend(x, y, rgb, a);
            }
        }
    }

    fn round_coverage(&self, r: Rectf, rad: f64, px: f64, py: f64) -> f64 {
        let dx = (r.x + rad - px).max(px - (r.x + r.w - rad)).max(0.0);
        let dy = (r.y + rad - py).max(py - (r.y + r.h - rad)).max(0.0);
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
                let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
                self.blend(x, y, rgb, (rad + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    fn into_rgba(self) -> Vec<u8> {
        self.px
    }
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_icon_synthesises_a_decodable_png() {
        let url = demo_icon("org.gimp.GIMP");
        let bytes = fetcher()(&url).expect("demo icon bytes");
        let img = image::load_from_memory(&bytes).expect("valid png");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn demo_shot_synthesises_a_decodable_png() {
        let url = demo_shot("org.gimp.GIMP", 0);
        let bytes = fetcher()(&url).expect("demo shot bytes");
        let img = image::load_from_memory(&bytes).expect("valid png");
        assert!(img.width() > 200 && img.height() > 100);
    }

    #[test]
    fn distinct_ids_give_distinct_icons() {
        // Different apps should not all collapse to one colour.
        let a = fetcher()(&demo_icon("org.gimp.GIMP")).unwrap();
        let b = fetcher()(&demo_icon("org.mozilla.firefox")).unwrap();
        assert_ne!(a, b, "distinct app ids should synthesise distinct icons");
    }

    #[test]
    fn non_demo_non_http_is_none() {
        assert!(fetcher()("file:///etc/passwd").is_none());
        assert!(fetcher()("qos-demo://unknown/shape").is_none());
    }
}
