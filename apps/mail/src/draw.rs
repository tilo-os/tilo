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
    let palette: [Color; 8] = [
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
    // The palette holds exactly 8 entries, so masking to three bits always
    // lands inside it; the fallback is unreachable.
    palette
        .get((h & 0b111) as usize)
        .copied()
        .unwrap_or(c.accent[5])
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
            let brush_transform = Affine::translate((dx, dy)).pre_scale(scale);
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
    use std::fmt::Write as _;
    let digest = md5(input);
    let mut s = String::with_capacity(32);
    for b in digest {
        // Writing to a `String` is infallible.
        let _ = write!(s, "{b:02x}");
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
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(chunk.chunks_exact(4)) {
            // `chunks_exact(4)` always yields four bytes, so this never fails.
            if let Ok(le) = <[u8; 4]>::try_from(bytes) {
                *word = u32::from_le_bytes(le);
            }
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for (round, (&kc, &shift)) in K.iter().zip(S.iter()).enumerate() {
            // The word index is masked to four bits, so it always selects one
            // of the sixteen words and the fallback below is unreachable.
            // `round` is at most 63, so the wrapping never engages either.
            let (mix, idx) = match round {
                0..=15 => ((b & c) | (!b & d), round & 0b1111),
                16..=31 => (
                    (d & b) | (!d & c),
                    round.wrapping_mul(5).wrapping_add(1) & 0b1111,
                ),
                32..=47 => (b ^ c ^ d, round.wrapping_mul(3).wrapping_add(5) & 0b1111),
                _ => (c ^ (b | !d), round.wrapping_mul(7) & 0b1111),
            };
            let mix = mix
                .wrapping_add(a)
                .wrapping_add(kc)
                .wrapping_add(words.get(idx).copied().unwrap_or(0));
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(mix.rotate_left(shift));
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
