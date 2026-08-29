//! Shared drawing primitives used by all three panes and the app chrome.
//!
//! These are thin wrappers over the Vello scene plus the two mail-specific
//! bits every pane needs: text elision to a pixel budget and the sender
//! avatar. Avatars try a real remote image (a Gravatar keyed by the address)
//! through the shared [`ui::ImageStore`]; until/unless one resolves they fall
//! back to a coloured initials disc — a real, computed avatar, never a blank
//! placeholder.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, ImageBrush};

use ui::ImageStore;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Avatar;

use crate::model::Addr;

/// Height of a per-pane header strip (folder title / actions / account switch).
pub const HEADER_H: f64 = 56.0;
/// Height of one message row in the list.
pub const LIST_ROW_H: f64 = 78.0;
/// Height of a folder row in the sidebar.
pub const FOLDER_ROW_H: f64 = 36.0;
/// Height of an account header in the sidebar.
pub const ACCOUNT_HEADER_H: f64 = 44.0;
/// Geometry of the `NavShell` top-left chip (collapse / hamburger / back),
/// mirrored from `ui`'s `NavShell` so a pane header can vertically centre its
/// title on the chip beside it and space itself a consistent gap away. The chip
/// is a `CHIP_SIZE`-square inset `CHIP_INSET` from the pane's top-left corner.
pub const CHIP_INSET: f64 = 12.0;
pub const CHIP_SIZE: f64 = 36.0;
/// Horizontal room the `NavShell` hamburger/chevron chip needs at a pane's
/// top-left; pane headers inset their leading content past it.
pub const CHIP_CLEARANCE: f64 = 52.0;
/// Horizontal room the `NavShell` "Back" chip needs (chevron + label).
pub const BACK_CLEARANCE: f64 = 118.0;

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, r: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &RoundedRect::from_rect(rect, r),
    );
}

pub fn stroke_round(scene: &mut Scene, t: Affine, rect: Rect, r: f64, w: f64, color: Color) {
    scene.stroke(
        &Stroke::new(w),
        t,
        color,
        None,
        &RoundedRect::from_rect(rect, r),
    );
}

/// Truncate `text` with a trailing ellipsis so it fits within `max_w` pixels
/// at the given size/weight.
pub fn elide(
    tr: &mut TextRenderer,
    text: &str,
    size: f32,
    weight: TextWeight,
    max_w: f64,
) -> String {
    if max_w <= 1.0 {
        return String::new();
    }
    let (w, _) = tr.measure_weighted(text, size, None, weight);
    if f64::from(w) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand = format!("{}…", chars.iter().collect::<String>().trim_end());
        let (cw, _) = tr.measure_weighted(&cand, size, None, weight);
        if f64::from(cw) <= max_w {
            return cand;
        }
    }
    "…".to_string()
}

/// Deterministic accent colour for an address' initials disc.
pub fn avatar_color(theme: &Theme, email: &str, tint: u8) -> Color {
    let c = &theme.tokens.colors;
    let palette = [
        c.accent[5],
        c.semantic.success,
        c.semantic.info,
        c.semantic.warning,
        c.accent[6],
        c.accent[3],
        Color::from_rgb8(217, 70, 239),
        Color::from_rgb8(20, 184, 166),
    ];
    let h = email.bytes().fold(u32::from(tint), |a, b| {
        a.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    palette[(h as usize) % palette.len()]
}

/// Gravatar URL for an address. `d=404` makes the CDN return a 404 (not a
/// generic silhouette) when the address has no avatar, so the store settles as
/// `Failed` and we keep the initials disc rather than a stock image.
pub fn gravatar_url(email: &str) -> String {
    let hash = md5_hex(email.trim().to_ascii_lowercase().as_bytes());
    format!("https://www.gravatar.com/avatar/{hash}?s=96&d=404")
}

/// Draw a sender avatar centred at `center`. Uses a real Gravatar when the
/// [`ImageStore`] has one decoded; otherwise a coloured initials disc.
#[allow(clippy::too_many_arguments)]
pub fn draw_avatar(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    theme: &Theme,
    images: &ImageStore,
    center: Point,
    radius: f64,
    addr: &Addr,
    tint: u8,
) {
    if let Some(img) = images.get(&gravatar_url(&addr.email)) {
        let iw = f64::from(img.width);
        let ih = f64::from(img.height);
        if iw > 0.0 && ih > 0.0 {
            let rect = Rect::new(
                center.x - radius,
                center.y - radius,
                center.x + radius,
                center.y + radius,
            );
            let scale = (rect.width() / iw).max(rect.height() / ih);
            let dw = iw * scale;
            let dh = ih * scale;
            let dx = rect.x0 + (rect.width() - dw) * 0.5;
            let dy = rect.y0 + (rect.height() - dh) * 0.5;
            let brush_transform = Affine::translate((dx, dy)) * Affine::scale(scale);
            let clip = Circle::new(center, radius);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
            scene.fill(
                Fill::NonZero,
                t,
                &ImageBrush::new(img),
                Some(brush_transform),
                &rect,
            );
            scene.pop_layer();
            return;
        }
    }
    // No photo: fall back to a coloured initials disc. The per-address hue
    // (a stable hash of the email) is preserved; the disc + centred monogram
    // come from the shared toolkit avatar so shape/placement match every app.
    let rect = Rect::new(
        center.x - radius,
        center.y - radius,
        center.x + radius,
        center.y + radius,
    );
    let col = avatar_color(theme, &addr.email, tint);
    Avatar::draw_at(scene, t, rect, &addr.initials(), col, theme, tr);
}

// ---- md5 (RFC 1321), inline to avoid a dependency for one hash ------------

/// Hex-encoded MD5 of `input`. Small self-contained implementation — the only
/// use is deriving the Gravatar path from an email address.
pub fn md5_hex(input: &[u8]) -> String {
    let digest = md5(input);
    let mut s = String::with_capacity(32);
    for b in digest {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap());
        s.push(char::from_digit(u32::from(b & 0xf), 16).unwrap());
    }
    s
}

fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn gravatar_url_lowercases_and_trims() {
        // Canonical Gravatar example hash for "MyEmailAddress@example.com".
        let u = gravatar_url("  MyEmailAddress@example.com ");
        assert!(u.contains("0bc83cb571cd1c50ba6f3e8a78ef1346"), "{u}");
    }
}
