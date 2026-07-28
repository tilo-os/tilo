//! The compositor-owned session-lock overlay.
//!
//! When [`crate::compositor::idle::SessionManager::is_locked`] is set,
//! [`crate::compositor::native::run`] paints *this* scene instead of the
//! chrome's. The overlay is fully opaque (it fills the viewport), so app /
//! chrome content is hidden, and the loop routes every keystroke into the
//! lock screen's password field ([`crate::compositor::lock_controller`]) —
//! the session only unlocks on a verified password. Hidden content + captured
//! input + credential-gated unlock together are the lock's security boundary.
//! (`ext-session-lock-v1` is also served as a standard external lock trigger;
//! qOS renders this overlay rather than a client lock surface.)
//!
//! The visual is a padlock glyph, a clock/date, and a masked password field
//! with an inline error line. The colours come from the OS [`Theme`] so the
//! lock screen matches the rest of the desktop.

use std::f64::consts::PI;

use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// The live lock-screen state to render, computed by the caller so this
/// paint stays pure and unit-testable (no clock reads, no auth state of its
/// own). Everything shown on the lock surface is derived from these fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct LockView<'a> {
    /// Number of password characters typed so far — rendered as masked
    /// dots. The plaintext never reaches this layer.
    pub password_len: usize,
    /// Authentication error to show under the field (e.g. "Incorrect
    /// username or password"), or `None`. Rendered in the danger colour.
    pub error: Option<&'a str>,
    /// Preformatted wall-clock time (e.g. `"13:37"`). Empty hides the clock.
    pub clock: &'a str,
    /// Preformatted date (e.g. `"Friday, 11 July"`). Empty hides the date.
    pub date: &'a str,
    /// Active notification count (count-only — never content).
    pub notification_count: usize,
}

/// Build the lock-screen scene at `viewport` size, themed via `theme`,
/// using `text` for the glyph-shaped labels, from the live [`LockView`].
///
/// The overlay is fully opaque and fills the viewport, so app/chrome content
/// is hidden behind it. `view.notification_count`, when non-zero, shows
/// "N notifications" so the user knows something arrived **without** leaking
/// any app name, summary, or body (lock-suppression — bodies are never
/// rendered here).
///
/// Returns a fresh [`Scene`] the backend can submit exactly like a chrome
/// frame — no special-casing downstream.
pub fn draw_lock_scene(
    viewport: (f64, f64),
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) -> Scene {
    let mut scene = Scene::new();
    let (w, h) = viewport;

    // 1. Opaque backdrop covering the whole output — hides everything
    //    behind it. Use the app background so the lock screen reads as
    //    part of the same desktop.
    let backdrop = Rect::new(0.0, 0.0, w, h);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &backdrop,
    );

    let cx = w / 2.0;
    let cy = h / 2.0;

    // 2. Clock + date, centred in the upper third — the ambient info a
    //    lock screen shows before you authenticate.
    if !view.clock.is_empty() {
        let clock_size = 64.0_f32;
        let (cw, _) = text.measure(view.clock, clock_size, None);
        text.draw_text(
            &mut scene,
            view.clock,
            clock_size,
            theme.text_primary,
            Affine::translate((cx - cw as f64 / 2.0, cy - 170.0)),
            None,
        );
    }
    if !view.date.is_empty() {
        let date_size = 18.0_f32;
        let (dw, _) = text.measure(view.date, date_size, None);
        text.draw_text(
            &mut scene,
            view.date,
            date_size,
            theme.text_dim,
            Affine::translate((cx - dw as f64 / 2.0, cy - 120.0)),
            None,
        );
    }

    // 3. Padlock body — a rounded rectangle just above centre.
    let bw = 60.0_f64;
    let bh = 46.0_f64;
    let body_y = cy - 60.0;
    let body = RoundedRect::new(cx - bw / 2.0, body_y, cx + bw / 2.0, body_y + bh, 10.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &body,
    );

    // 4. Shackle — an upper semicircle resting on the body's top edge.
    //    In screen (y-down) space, start_angle = PI sweeping +PI traces
    //    the upper half (passing through the topmost point at 1.5*PI).
    let r = bw / 3.2;
    let shackle = Arc::new(Point::new(cx, body_y), (r, r), PI, PI, 0.0);
    scene.stroke(
        &Stroke::new(8.0),
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &shackle,
    );

    // 5. Keyhole — a small dot punched into the body in the backdrop
    //    colour so the glyph reads as a padlock.
    let keyhole = Circle::new(Point::new(cx, body_y + bh / 2.0), 6.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &keyhole,
    );

    // 6. "Locked" title.
    let title = "Locked";
    let title_size = 24.0_f32;
    let (tw, _) = text.measure(title, title_size, None);
    let title_y = body_y + bh + 30.0;
    text.draw_text(
        &mut scene,
        title,
        title_size,
        theme.text_primary,
        Affine::translate((cx - tw as f64 / 2.0, title_y)),
        None,
    );

    // 7. Password field — a rounded pill with one dot per typed character.
    //    The plaintext is never available here; only the length is.
    let field_w = 280.0_f64;
    let field_h = 44.0_f64;
    let field_x = cx - field_w / 2.0;
    let field_y = title_y + 26.0;
    let field = RoundedRect::new(field_x, field_y, field_x + field_w, field_y + field_h, 22.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_primary,
        None,
        &field,
    );
    // A subtle border — focused colour, since the field always holds focus
    // while locked.
    scene.stroke(
        &Stroke::new(1.5),
        Affine::IDENTITY,
        theme.border_focus,
        None,
        &field,
    );
    if view.password_len == 0 {
        // Placeholder prompt when empty.
        let ph = "Enter password";
        let ph_size = 15.0_f32;
        let (pw, ph_h) = text.measure(ph, ph_size, None);
        text.draw_text(
            &mut scene,
            ph,
            ph_size,
            theme.text_dim,
            Affine::translate((
                cx - pw as f64 / 2.0,
                field_y + field_h / 2.0 + ph_h as f64 / 2.0 - 3.0,
            )),
            None,
        );
    } else {
        // One dot per character, centred, capped so a very long password
        // can't overflow the field.
        let dot_r = 4.0_f64;
        let gap = 14.0_f64;
        let shown = view.password_len.min(18);
        let total_w = (shown as f64 - 1.0).max(0.0) * gap;
        let start_x = cx - total_w / 2.0;
        let dot_y = field_y + field_h / 2.0;
        for i in 0..shown {
            let dot = Circle::new(Point::new(start_x + i as f64 * gap, dot_y), dot_r);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                theme.text_primary,
                None,
                &dot,
            );
        }
    }

    // 8. Error line (danger colour) or the neutral hint under the field.
    let line_y = field_y + field_h + 26.0;
    if let Some(err) = view.error {
        let err_size = 14.0_f32;
        let (ew, _) = text.measure(err, err_size, None);
        text.draw_text(
            &mut scene,
            err,
            err_size,
            theme.tokens.colors.semantic.danger,
            Affine::translate((cx - ew as f64 / 2.0, line_y)),
            None,
        );
    } else {
        let hint = "Type your password and press Enter";
        let hint_size = 13.0_f32;
        let (hw, _) = text.measure(hint, hint_size, None);
        text.draw_text(
            &mut scene,
            hint,
            hint_size,
            theme.text_dim,
            Affine::translate((cx - hw as f64 / 2.0, line_y)),
            None,
        );
    }

    // 9. Count-only notification summary (lock-suppression). Show
    //    "N notifications" — never any content.
    let label = count_label(view.notification_count);
    if !label.is_empty() {
        let label_size = 14.0_f32;
        let (lw, _) = text.measure(&label, label_size, None);
        text.draw_text(
            &mut scene,
            &label,
            label_size,
            theme.text_dim,
            Affine::translate((cx - lw as f64 / 2.0, line_y + 34.0)),
            None,
        );
    }

    scene
}

/// Lock-screen count label: "N notifications" (or empty for zero). Kept
/// next to [`draw_lock_scene`] and re-using the shell's redaction helper
/// so the lock screen and the toast stack agree on wording.
fn count_label(count: usize) -> String {
    crate::shell::notification::lock_suppressed_label(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> LockView<'static> {
        LockView {
            clock: "13:37",
            date: "Friday, 11 July",
            ..LockView::default()
        }
    }

    /// The lock scene must encode *something* (backdrop + glyph + text).
    /// A blank scene would mean the overlay never paints and app content
    /// would leak through — the whole point of the lock is defeated.
    #[test]
    fn lock_scene_is_non_empty() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let scene = draw_lock_scene((1600.0, 900.0), &theme, &mut text, &view());
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
        let v = LockView {
            notification_count: 3,
            ..view()
        };
        let _ = draw_lock_scene((1.0, 1.0), &theme, &mut text, &v);
    }

    /// The password field renders both empty (placeholder) and populated
    /// (masked dots) without panicking, including a very long password that
    /// must be capped to the field width.
    #[test]
    fn lock_scene_renders_password_field_states() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        for len in [0usize, 1, 8, 200] {
            let v = LockView {
                password_len: len,
                ..view()
            };
            let scene = draw_lock_scene((1600.0, 900.0), &theme, &mut text, &v);
            assert!(!scene.encoding().is_empty());
        }
    }

    /// An error string paints (in the danger colour) without panicking.
    #[test]
    fn lock_scene_renders_error() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let v = LockView {
            password_len: 0,
            error: Some("Incorrect username or password"),
            ..view()
        };
        let scene = draw_lock_scene((1600.0, 900.0), &theme, &mut text, &v);
        assert!(!scene.encoding().is_empty());
    }

    /// The count-only summary never renders content. We can't read text
    /// back out of a Vello scene, so assert the contract at the label
    /// helper: it is count-only and empty for zero.
    #[test]
    fn count_label_is_count_only() {
        assert_eq!(count_label(0), "");
        assert_eq!(count_label(1), "1 notification");
        assert_eq!(count_label(4), "4 notifications");
    }

    /// Rendering with a non-zero count still produces a valid scene.
    #[test]
    fn lock_scene_with_notifications_paints() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let v = LockView {
            notification_count: 5,
            ..view()
        };
        let scene = draw_lock_scene((1600.0, 900.0), &theme, &mut text, &v);
        assert!(!scene.encoding().is_empty());
    }
}
