//! Track D2 — the compositor-owned session-lock overlay.
//!
//! When [`crate::compositor::idle::SessionManager::is_locked`] is set,
//! [`crate::compositor::native::run`] paints *this* scene instead of the
//! chrome's. The overlay is fully opaque (it fills the viewport), so app
//! / chrome content is hidden, and the loop captures input until the
//! explicit unlock action — together that is the security boundary the
//! lock provides without an `ext-session-lock-v1` client.
//!
//! The visual is deliberately minimal (a padlock glyph + two lines of
//! text): the point of this pass is correct state handling, not a
//! polished lock screen. The colours come from the OS [`Theme`] so the
//! lock screen matches the rest of the desktop.

use std::f64::consts::PI;

use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Build the lock-screen scene at `viewport` size, themed via `theme`,
/// using `text` for the glyph-shaped labels.
///
/// Returns a fresh [`Scene`] the backend can submit exactly like a
/// chrome frame — no special-casing downstream.
pub fn draw_lock_scene(viewport: (f64, f64), theme: &Theme, text: &mut TextRenderer) -> Scene {
    let mut scene = Scene::new();
    let (w, h) = viewport;

    // 1. Opaque backdrop covering the whole output — hides everything
    //    behind it. Use the app background so the lock screen reads as
    //    part of the same desktop.
    let backdrop = Rect::new(0.0, 0.0, w, h);
    scene.fill(Fill::NonZero, Affine::IDENTITY, theme.bg_app, None, &backdrop);

    let cx = w / 2.0;
    let cy = h / 2.0;

    // 2. Padlock body — a rounded rectangle just above centre.
    let bw = 84.0_f64;
    let bh = 64.0_f64;
    let body_y = cy - 30.0;
    let body = RoundedRect::new(cx - bw / 2.0, body_y, cx + bw / 2.0, body_y + bh, 12.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &body,
    );

    // 3. Shackle — an upper semicircle resting on the body's top edge.
    //    In screen (y-down) space, start_angle = PI sweeping +PI traces
    //    the upper half (passing through the topmost point at 1.5*PI).
    let r = bw / 3.2;
    let shackle = Arc::new(Point::new(cx, body_y), (r, r), PI, PI, 0.0);
    scene.stroke(
        &Stroke::new(11.0),
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &shackle,
    );

    // 4. Keyhole — a small dot punched into the body in the backdrop
    //    colour so the glyph reads as a padlock.
    let keyhole = Circle::new(Point::new(cx, body_y + bh / 2.0), 8.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &keyhole,
    );

    // 5. Labels, centred horizontally.
    let title = "Locked";
    let title_size = 28.0_f32;
    let (tw, _) = text.measure(title, title_size, None);
    text.draw_text(
        &mut scene,
        title,
        title_size,
        theme.text_primary,
        Affine::translate((cx - tw as f64 / 2.0, body_y + bh + 28.0)),
        None,
    );

    let hint = "Press Enter to unlock";
    let hint_size = 16.0_f32;
    let (hw, _) = text.measure(hint, hint_size, None);
    text.draw_text(
        &mut scene,
        hint,
        hint_size,
        theme.text_dim,
        Affine::translate((cx - hw as f64 / 2.0, body_y + bh + 68.0)),
        None,
    );

    scene
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock scene must encode *something* (backdrop + glyph + text).
    /// A blank scene would mean the overlay never paints and app content
    /// would leak through — the whole point of the lock is defeated.
    #[test]
    fn lock_scene_is_non_empty() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let scene = draw_lock_scene((1600.0, 900.0), &theme, &mut text);
        assert!(
            !scene.encoding().is_empty(),
            "lock overlay must produce a non-empty scene so it hides app content"
        );
    }

    /// Painting at a tiny viewport must not panic (degenerate sizes can
    /// appear before the first real `OutputResized`).
    #[test]
    fn lock_scene_handles_tiny_viewport() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let _ = draw_lock_scene((1.0, 1.0), &theme, &mut text);
    }
}
