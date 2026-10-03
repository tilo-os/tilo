//! Procedural album art.
//!
//! The offline sample library ships no image files, so cover art is
//! synthesised deterministically from each album/artist id: a hashed hue
//! drives a two-tone rounded tile with layered translucent blobs and the
//! item's initials. The same id always yields the same art, so the grid is
//! stable across runs and the screenshot looks like a real library.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, RoundedRect};
use vello::peniko::{Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};

/// FNV-1a hash of a string — small, fast, deterministic.
pub fn hash_str(s: &str) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, s.as_bytes());
    std::hash::Hasher::finish(&hasher)
}

/// Convert HSL (`h` in degrees, `s`/`l` in `0..=1`) to an sRGB color.
#[allow(
    clippy::many_single_char_names,
    reason = "h/s/l/c/x/m are the standard variable names for the CSS/Wikipedia HSL-to-RGB \
              conversion algorithm; renaming them would only make the code harder to \
              cross-check against the reference formula"
)]
fn hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color::from_rgb8(
        (((r1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((g1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((b1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
    )
}

/// The two dominant colors for an item's art.
pub fn art_colors(seed: &str) -> (Color, Color) {
    let h = hash_str(seed);
    let hue = (h % 360) as f32;
    let hue2 = (hue + 40.0 + ((h >> 8) % 40) as f32).rem_euclid(360.0);
    let base = hsl(hue, 0.55, 0.42);
    let accent = hsl(hue2, 0.62, 0.58);
    (base, accent)
}

/// Derive up-to-two-character initials from a title.
pub fn initials(title: &str) -> String {
    let mut out = String::new();
    for word in title.split_whitespace() {
        if let Some(c) = word.chars().find(|c| c.is_alphanumeric()) {
            out.push(c.to_ascii_uppercase());
            if out.len() >= 2 {
                break;
            }
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// Draw a procedural cover tile into `rect`.
pub fn draw_album_art(
    scene: &mut Scene,
    transform: Affine,
    rect: vello::kurbo::Rect,
    seed: &str,
    label: &str,
    text_renderer: &mut TextRenderer,
    radius: f64,
) {
    let (base, accent) = art_colors(seed);
    let shape = RoundedRect::from_rect(rect, radius);
    // Clip to the rounded tile so the blobs don't spill past the corners.
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        transform,
        &shape,
    );
    scene.fill(Fill::NonZero, transform, base, None, &rect);

    let w = rect.width();
    let h = rect.height();
    // Two soft blobs of the accent color, positions derived from the hash.
    let hh = hash_str(seed);
    let cx1 = rect.x0 + w * (0.18 + (hh % 30) as f64 / 100.0);
    let cy1 = rect.y0 + h * (0.20 + ((hh >> 4) % 30) as f64 / 100.0);
    scene.fill(
        Fill::NonZero,
        transform,
        accent.multiply_alpha(0.85),
        None,
        &Circle::new(Point::new(cx1, cy1), w * 0.42),
    );
    let cx2 = rect.x0 + w * (0.78 - ((hh >> 8) % 25) as f64 / 100.0);
    let cy2 = rect.y0 + h * (0.82 - ((hh >> 12) % 25) as f64 / 100.0);
    scene.fill(
        Fill::NonZero,
        transform,
        Color::WHITE.multiply_alpha(0.10),
        None,
        &Circle::new(Point::new(cx2, cy2), w * 0.30),
    );
    // A faint dark vignette toward the bottom for legibility.
    scene.fill(
        Fill::NonZero,
        transform,
        Color::BLACK.multiply_alpha(0.18),
        None,
        &vello::kurbo::Rect::new(rect.x0, rect.y0 + h * 0.62, rect.x1, rect.y1),
    );

    // Initials, centred.
    let text = initials(label);
    let size = (h * 0.32) as f32;
    let (tw, th) = text_renderer.measure_weighted(&text, size, None, TextWeight::Bold);
    let tx = rect.x0 + (w - f64::from(tw)) / 2.0;
    let ty = rect.y0 + (h - f64::from(th)) / 2.0;
    text_renderer.draw_text_weighted(
        scene,
        &text,
        size,
        Color::WHITE.multiply_alpha(0.92),
        transform * Affine::translate((tx, ty)),
        None,
        TextWeight::Bold,
    );

    scene.pop_layer();
}
