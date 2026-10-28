//! Procedural viewfinder + thumbnail synthesis.
//!
//! `--shots` has no camera and no event loop, so the viewfinder is painted
//! from code: a handful of deterministic "scenes" (landscape, city, portrait
//! bokeh, …) rendered straight into an RGBA buffer. Recent-capture thumbnails
//! are the same generator at a smaller size, seeded per-capture, so the
//! capture history is fully populated without bundling any binary assets.
//!
//! The scanner frame additionally composites a real QR poster into the
//! scene and decodes it back with [`crate::scan`], so the on-screen
//! bounding box is produced by an actual decode rather than placed by hand.

use crate::model::ScannedCode;
use crate::scan;

/// An RGBA8 image buffer, top-left origin.
#[derive(Clone)]
pub struct FrameBuf {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// A pixel-space rectangle, `x0..x1` × `y0..y1` (half-open). May extend past
/// the buffer edges — `put`/`blend` clip each pixel.
#[derive(Clone, Copy)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl FrameBuf {
    fn new(w: u32, h: u32) -> Self {
        Self {
            rgba: vec![0; (w * h * 4) as usize],
            w,
            h,
        }
    }

    #[inline]
    #[allow(
        clippy::many_single_char_names,
        reason = "x/y pixel coords and r/g/b color channels are the standard names for a pixel setter"
    )]
    fn put(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let Some(px) = self
            .rgba
            .get_mut(i..i + 4)
            .and_then(|s| <&mut [u8; 4]>::try_from(s).ok())
        else {
            return;
        };
        px[0] = r;
        px[1] = g;
        px[2] = b;
        px[3] = 255;
    }

    #[inline]
    #[allow(
        clippy::many_single_char_names,
        reason = "x/y pixel coords and r/g/b color channels are the standard names for a pixel blend"
    )]
    fn blend(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, a: f32) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let Some(px) = self
            .rgba
            .get_mut(i..i + 4)
            .and_then(|s| <&mut [u8; 4]>::try_from(s).ok())
        else {
            return;
        };
        let a = a.clamp(0.0, 1.0);
        let inv = 1.0 - a;
        px[0] = (f32::from(px[0]) * inv + f32::from(r) * a) as u8;
        px[1] = (f32::from(px[1]) * inv + f32::from(g) * a) as u8;
        px[2] = (f32::from(px[2]) * inv + f32::from(b) * a) as u8;
        px[3] = 255;
    }

    fn fill_rect(&mut self, rect: Rect, color: Rgb) {
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                self.put(x, y, color.0, color.1, color.2);
            }
        }
    }
}

type Rgb = (u8, u8, u8);

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    (
        (f32::from(a.0) + (f32::from(b.0) - f32::from(a.0)) * t) as u8,
        (f32::from(a.1) + (f32::from(b.1) - f32::from(a.1)) * t) as u8,
        (f32::from(a.2) + (f32::from(b.2) - f32::from(a.2)) * t) as u8,
    )
}

/// Cheap deterministic value hash → [0,1).
fn hash01(mut n: u32) -> f32 {
    n = (n << 13) ^ n;
    let m = (n
        .wrapping_mul(n.wrapping_mul(n).wrapping_mul(15731).wrapping_add(789_221))
        .wrapping_add(1_376_312_589))
        & 0x7fff_ffff;
    1.0 - (m as f32 / 1_073_741_824.0)
}

struct Palette {
    sky_top: Rgb,
    sky_bot: Rgb,
    ground: Rgb,
    ridge: Rgb,
    accent: Rgb,
    style: u8,
}

/// Six distinct looks, selected by `seed`. `style` selects the composition
/// (0 landscape, 1 sunset, 2 city, 3 portrait bokeh, 4 forest, 5 desert).
fn palette(seed: u8) -> Palette {
    match seed % 6 {
        0 => Palette {
            sky_top: (96, 165, 250),
            sky_bot: (191, 219, 254),
            ground: (52, 96, 64),
            ridge: (30, 58, 70),
            accent: (250, 250, 240),
            style: 0,
        },
        1 => Palette {
            sky_top: (244, 114, 90),
            sky_bot: (252, 211, 77),
            ground: (60, 40, 60),
            ridge: (88, 28, 64),
            accent: (255, 237, 200),
            style: 1,
        },
        2 => Palette {
            sky_top: (30, 41, 59),
            sky_bot: (71, 85, 105),
            ground: (15, 18, 28),
            ridge: (51, 65, 85),
            accent: (250, 204, 21),
            style: 2,
        },
        3 => Palette {
            sky_top: (60, 50, 70),
            sky_bot: (120, 90, 110),
            ground: (40, 30, 45),
            ridge: (90, 70, 90),
            accent: (244, 211, 188),
            style: 3,
        },
        4 => Palette {
            sky_top: (134, 192, 160),
            sky_bot: (214, 232, 200),
            ground: (34, 70, 48),
            ridge: (24, 54, 40),
            accent: (245, 245, 225),
            style: 4,
        },
        _ => Palette {
            sky_top: (250, 200, 140),
            sky_bot: (252, 232, 196),
            ground: (176, 124, 78),
            ridge: (140, 92, 60),
            accent: (255, 244, 222),
            style: 5,
        },
    }
}

/// Sky gradient above the horizon, darkened ground gradient below it.
fn paint_sky_and_ground(buf: &mut FrameBuf, pal: &Palette, w: i32, h: i32, horizon: i32) {
    for y in 0..h {
        if y < horizon {
            let t = y as f32 / horizon.max(1) as f32;
            let c = lerp(pal.sky_top, pal.sky_bot, t);
            for x in 0..w {
                buf.put(x, y, c.0, c.1, c.2);
            }
        } else {
            let t = (y - horizon) as f32 / (h - horizon).max(1) as f32;
            let c = lerp(pal.ground, lerp(pal.ground, (0, 0, 0), 0.3), t);
            for x in 0..w {
                buf.put(x, y, c.0, c.1, c.2);
            }
        }
    }
}

/// Sun disc with glow above the horizon (styles 1 and 5).
fn paint_sun_glow(buf: &mut FrameBuf, pal: &Palette, w: i32, h: i32, horizon: i32) {
    let cx = w as f32 * 0.5;
    let cy = horizon as f32 - h as f32 * 0.12;
    let rad = h as f32 * 0.10;
    for y in 0..horizon {
        for x in 0..w {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d < rad {
                buf.blend(x, y, pal.accent.0, pal.accent.1, pal.accent.2, 0.95);
            } else if d < rad * 3.0 {
                let a = 1.0 - (d - rad) / (rad * 2.0);
                buf.blend(x, y, pal.accent.0, pal.accent.1, pal.accent.2, a * 0.35);
            }
        }
    }
}

/// City skyline: vertical building blocks with lit windows (style 2).
fn paint_city_skyline(buf: &mut FrameBuf, pal: &Palette, seed: u8, w: i32, h: i32, horizon: i32) {
    let mut x = 0;
    let mut col = 0u32;
    while x < w {
        let bw = 24 + (hash01(col * 7 + u32::from(seed) * 31) * 46.0) as i32;
        let bh = (h as f32 * (0.18 + hash01(col * 13 + 3) * 0.34)) as i32;
        let top = horizon - bh;
        let shade = lerp(pal.ridge, (12, 14, 22), hash01(col * 5) * 0.6);
        buf.fill_rect(
            Rect {
                x0: x,
                y0: top,
                x1: (x + bw).min(w),
                y1: horizon,
            },
            shade,
        );
        // windows
        let mut wy = top + 8;
        while wy < horizon - 6 {
            let mut wx = x + 5;
            while wx < x + bw - 5 {
                if hash01((wx * 131 + wy * 57 + col as i32) as u32) > 0.45 {
                    buf.blend(wx, wy, pal.accent.0, pal.accent.1, pal.accent.2, 0.85);
                    buf.blend(wx + 1, wy, pal.accent.0, pal.accent.1, pal.accent.2, 0.85);
                    buf.blend(wx, wy + 1, pal.accent.0, pal.accent.1, pal.accent.2, 0.6);
                }
                wx += 9;
            }
            wy += 11;
        }
        x += bw + 4;
        col += 1;
    }
}

/// Soft bokeh circles in the upper area (style 3).
fn paint_bokeh(buf: &mut FrameBuf, pal: &Palette, seed: u8, w: i32, horizon: i32) {
    for i in 0..26u32 {
        let bx = (hash01(i * 17 + u32::from(seed)) * w as f32) as i32;
        let by = (hash01(i * 31 + 7) * horizon as f32) as i32;
        let br = 10.0 + hash01(i * 13 + 3) * 34.0;
        let tint = lerp(pal.accent, pal.sky_top, hash01(i * 5));
        for y in (by - br as i32)..(by + br as i32) {
            for xx in (bx - br as i32)..(bx + br as i32) {
                let d = (((xx - bx).pow(2) + (y - by).pow(2)) as f32).sqrt();
                if d < br {
                    let a = (1.0 - d / br) * 0.25;
                    buf.blend(xx, y, tint.0, tint.1, tint.2, a);
                }
            }
        }
    }
}

/// Layered mountain ridges (the default style).
fn paint_mountain_ridges(
    buf: &mut FrameBuf,
    pal: &Palette,
    seed: u8,
    w: i32,
    h: i32,
    horizon: i32,
) {
    for layer in 0..3 {
        let base = horizon - (3 - layer) * (h / 18);
        let amp = h as f32 * (0.05 + layer as f32 * 0.02);
        let shade = lerp(pal.ridge, pal.ground, layer as f32 / 3.0);
        for x in 0..w {
            let t = x as f32 / w as f32 * std::f32::consts::PI * (2.0 + layer as f32);
            let ridge =
                base as f32 - (t + f32::from(seed)).sin() * amp - (t * 1.7).cos() * amp * 0.4;
            for y in (ridge as i32)..horizon {
                buf.put(x, y, shade.0, shade.1, shade.2);
            }
        }
    }
}

/// Dispatches to the composition matching the palette's `style`.
fn paint_style(buf: &mut FrameBuf, pal: &Palette, seed: u8, w: i32, h: i32, horizon: i32) {
    match pal.style {
        1 | 5 => paint_sun_glow(buf, pal, w, h, horizon),
        2 => paint_city_skyline(buf, pal, seed, w, h, horizon),
        3 => paint_bokeh(buf, pal, seed, w, horizon),
        _ => paint_mountain_ridges(buf, pal, seed, w, h, horizon),
    }
}

/// Subtle film grain + corner vignette so it reads as a photo.
fn paint_grain_and_vignette(buf: &mut FrameBuf, seed: u8, w: i32, h: i32) {
    for y in 0..h {
        for x in 0..w {
            let n = hash01((x * 374_761 + y * 668_265 + i32::from(seed) * 9176) as u32);
            if n > 0.93 {
                buf.blend(x, y, 255, 255, 255, 0.04);
            }
            let dx = (x as f32 / w as f32 - 0.5) * 2.0;
            let dy = (y as f32 / h as f32 - 0.5) * 2.0;
            let v = (dx * dx + dy * dy).sqrt();
            if v > 0.7 {
                buf.blend(x, y, 0, 0, 0, (v - 0.7) * 0.5);
            }
        }
    }
}

fn paint_scene(buf: &mut FrameBuf, seed: u8) {
    let w = buf.w as i32;
    let h = buf.h as i32;
    let pal = palette(seed);
    let horizon = (h as f32 * 0.62) as i32;

    paint_sky_and_ground(buf, &pal, w, h, horizon);
    paint_style(buf, &pal, seed, w, h, horizon);
    paint_grain_and_vignette(buf, seed, w, h);
}

/// Live-preview viewfinder for the Camera screen (no code in frame).
pub fn viewfinder(seed: u8, w: u32, h: u32) -> FrameBuf {
    let mut buf = FrameBuf::new(w, h);
    paint_scene(&mut buf, seed);
    buf
}

/// A small procedural thumbnail for a stored capture.
pub fn thumbnail(seed: u8, w: u32, h: u32) -> FrameBuf {
    let mut buf = FrameBuf::new(w, h);
    paint_scene(&mut buf, seed);
    buf
}

/// Composite a QR poster encoding `payload` into a fresh scene, then decode
/// it back. Returns the frame and the decoded code with corners in frame
/// pixel coordinates. If encode/decode somehow fails the frame is still
/// returned (with `None`) so the viewfinder is never blank.
pub fn scanner_frame(seed: u8, w: u32, h: u32, payload: &str) -> (FrameBuf, Option<ScannedCode>) {
    let centre = (w as f32 / 2.0, h as f32 / 2.0 - h as f32 / 22.0);
    scanner_frame_at(seed, w, h, payload, centre, h as f32 * 0.40)
}

/// [`scanner_frame`] with the poster centred on `centre` (frame pixels) and
/// about `size` pixels square, so a sample scene can hold its code inside
/// the scan guide wherever the layout puts it.
pub fn scanner_frame_at(
    seed: u8,
    w: u32,
    h: u32,
    payload: &str,
    centre: (f32, f32),
    size: f32,
) -> (FrameBuf, Option<ScannedCode>) {
    let mut buf = FrameBuf::new(w, h);
    paint_scene(&mut buf, seed);

    let Some(qr) = scan::encode(payload) else {
        return (buf, None);
    };
    let modules = qr.size as i32;
    let quiet = 4i32; // module quiet zone
    let total_modules = modules + quiet * 2;
    let mscale = (size as i32 / total_modules).max(2);
    let poster = total_modules * mscale;
    let px0 = centre.0 as i32 - poster / 2;
    let py0 = centre.1 as i32 - poster / 2;

    // Card with soft shadow.
    let pad = mscale * 2;
    for sy in 0..10 {
        let a = 0.12 - sy as f32 * 0.011;
        buf.fill_rect_blend(
            Rect {
                x0: px0 - pad + sy,
                y0: py0 - pad + sy,
                x1: px0 + poster + pad + sy,
                y1: py0 + poster + pad + sy,
            },
            (0, 0, 0),
            a,
        );
    }
    buf.fill_rect(
        Rect {
            x0: px0 - pad,
            y0: py0 - pad,
            x1: px0 + poster + pad,
            y1: py0 + poster + pad,
        },
        (250, 250, 250),
    );

    // QR modules (dark on the card's white quiet zone).
    for my in 0..modules {
        for mx in 0..modules {
            if !qr.is_dark(mx as usize, my as usize) {
                continue;
            }
            let x0 = px0 + (mx + quiet) * mscale;
            let y0 = py0 + (my + quiet) * mscale;
            buf.fill_rect(
                Rect {
                    x0,
                    y0,
                    x1: x0 + mscale,
                    y1: y0 + mscale,
                },
                (17, 17, 17),
            );
        }
    }

    // Decode the composited frame for real, identity-mapped (buffer coords
    // == frame coords).
    let luma = to_luma(&buf);
    let scanned = scan::decode_and_classify(&luma, w as usize, h as usize, |p| p);
    (buf, scanned)
}

impl FrameBuf {
    fn fill_rect_blend(&mut self, rect: Rect, color: Rgb, a: f32) {
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                self.blend(x, y, color.0, color.1, color.2, a);
            }
        }
    }
}

/// BT.601 luma of an RGBA frame — the greyscale buffer the QR detector wants.
pub fn luma(buf: &FrameBuf) -> Vec<u8> {
    to_luma(buf)
}

/// Drop the alpha channel: tightly-packed RGBA8 → tightly-packed RGB8. Used
/// by the photo (JPEG) and video (MJPEG) encoders, which have no alpha.
pub fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        if let [r, g, b, _a] = px {
            out.push(*r);
            out.push(*g);
            out.push(*b);
        }
    }
    out
}

/// BT.601 luma of an RGBA buffer.
fn to_luma(buf: &FrameBuf) -> Vec<u8> {
    let n = (buf.w * buf.h) as usize;
    let mut out = vec![0u8; n];
    for (px, luma) in buf.rgba.chunks_exact(4).zip(out.iter_mut()) {
        if let [r, g, b, _a] = px {
            let r = f32::from(*r);
            let g = f32::from(*g);
            let b = f32::from(*b);
            *luma = (0.299 * r + 0.587 * g + 0.114 * b) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_frames_are_opaque_sized_and_scannable() {
        {
            let f = viewfinder(0, 320, 200);
            assert_eq!(f.rgba.len(), 320 * 200 * 4);
            // Every alpha byte is 255 (fully painted, no holes).
            assert!(f.rgba.chunks_exact(4).all(|p| p[3] == 255));
        }
        {
            for seed in 0..6u8 {
                let f = viewfinder(seed, 160, 100);
                assert!(f.rgba.chunks_exact(4).all(|p| p[3] == 255));
            }
        }
        {
            let payload = "WIFI:T:WPA2;S:Tilo Guest;P:hunter2;H:false;;";
            let (frame, scanned) = scanner_frame(2, 900, 560, payload);
            assert_eq!(frame.rgba.len(), 900 * 560 * 4);
            let scanned = scanned.expect("the composited QR must decode");
            assert_eq!(scanned.raw, payload);
            for (x, y) in scanned.corners {
                assert!((0.0..=900.0).contains(&x), "corner x in range: {x}");
                assert!((0.0..=560.0).contains(&y), "corner y in range: {y}");
            }
        }
    }
}
