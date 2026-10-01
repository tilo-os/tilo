//! Shared drawing primitives used by all three panes and the app chrome.
//!
//! These are thin wrappers over the Vello scene plus the two mail-specific
//! bits every pane needs: text elision to a pixel budget and the sender
//! avatar. Avatars try a remote image (a Gravatar keyed by the address)
//! through the shared [`ui::ImageStore`]; until one resolves they fall back to
//! a coloured initials disc.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill, ImageBrush};

use ui::ImageStore;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Avatar;

use crate::model::Addr;
pub use ui::paint::{fill_rect, fill_round, stroke_round};

pub struct AvatarPaintContext<'a> {
    pub scene: &'a mut Scene,
    pub text: &'a mut TextRenderer,
    pub transform: Affine,
    pub theme: &'a Theme,
    pub images: &'a ImageStore,
}

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

// Rounded fills and edged surfaces go through the toolkit — `ui::surface::pill`
// for a plain rounded fill (or one with a semantic border such as focus) and
// `ui::surface::card` for a surface with its derived rest edge — so every
// pane's chips, rows and buttons share the toolkit's edge rule (HIG §9.5)
// instead of a private `fill_round` + `stroke_round(border_standard)` pair.

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
    tr.elide_to_width_weighted(text, size, ui::num::f32_of_f64(max_w), weight)
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
        c.categorical(3),
        c.categorical(4),
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
    let hash = hex::encode(<md5::Md5 as md5::Digest>::digest(
        email.trim().to_ascii_lowercase().as_bytes(),
    ));
    format!("https://www.gravatar.com/avatar/{hash}?s=96&d=404")
}

/// Draw a sender avatar centred at `center`. Uses a real Gravatar when the
/// [`ImageStore`] has one decoded; otherwise a coloured initials disc.
pub fn draw_avatar(
    ctx: &mut AvatarPaintContext<'_>,
    center: Point,
    radius: f64,
    addr: &Addr,
    tint: u8,
) {
    let scene = &mut *ctx.scene;
    let tr = &mut *ctx.text;
    let t = ctx.transform;
    let theme = ctx.theme;
    if let Some(img) = ctx.images.get(&gravatar_url(&addr.email)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn gravatar_urls_hash_the_normalised_address() {
        // Phase `gravatar_url_lowercases_and_trims`.
        {
            // Canonical Gravatar example hash for "MyEmailAddress@example.com".
            let u = gravatar_url("  MyEmailAddress@example.com ");
            assert!(u.contains("0bc83cb571cd1c50ba6f3e8a78ef1346"), "{u}");
        }
    }

    #[test]
    fn every_avatar_hue_comes_from_the_theme() {
        let dark = Theme::dark();
        let nord = Theme::named("Nord").expect("Nord");
        for i in 0..64 {
            let email = format!("person{i}@example.com");
            assert_ne!(
                avatar_color(&dark, &email, 0),
                avatar_color(&nord, &email, 0),
                "{email}: its initials disc ignores the theme"
            );
        }
    }
}
