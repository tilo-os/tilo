//! The compositor-owned session-lock overlay.
//!
//! When [`crate::compositor::idle::SessionManager::is_locked`] is set,
//! [`crate::compositor::native::run`] paints *this* scene instead of the
//! chrome's, hiding app/chrome content and routing every keystroke into the
//! lock screen's password field ([`crate::compositor::lock_controller`]) —
//! hidden content + captured input + credential-gated unlock together are
//! the lock's security boundary. (`ext-session-lock-v1` is also served as a
//! standard trigger; qOS renders this overlay rather than a client lock
//! surface.)
//!
//! Colours come from the OS [`Theme`] so the lock screen matches the rest of
//! the desktop.

use std::f64::consts::PI;

use vello::Scene;
use vello::kurbo::{Affine, Arc, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Extend, Fill, ImageBrush, ImageData};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Live lock-screen state to render, computed by the caller so this paint
/// stays pure and unit-testable (no clock reads, no auth state of its own).
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
    /// Credential verification running on the worker thread
    /// ([`crate::compositor::lock_controller::LockController::is_verifying`]);
    /// renders "Verifying…" and suppresses the empty-field placeholder so
    /// the screen reads as checking, not "type here".
    pub verifying: bool,
}

/// One output the lock overlay must cover, in the compositor's combined
/// coordinate space. The overlay draws its interactive widget centred
/// within each output's rectangle, so a two-monitor setup shows a usable
/// lock on every screen rather than one stranded across the bezel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockOutput {
    /// Top-left of this output in the combined space.
    pub origin: (f64, f64),
    /// Output size in pixels.
    pub size: (f64, f64),
}

impl LockOutput {
    /// The centre point of this output, where its lock widget is anchored.
    pub fn center(&self) -> (f64, f64) {
        (
            self.origin.0 + self.size.0 / 2.0,
            self.origin.1 + self.size.1 / 2.0,
        )
    }
}

/// Bounding box `(w, h)` covering every output, i.e. the size of the single
/// scene the compositor paints. `(0, 0)` for an empty list.
fn combined_extent(outputs: &[LockOutput]) -> (f64, f64) {
    let mut w = 0.0_f64;
    let mut h = 0.0_f64;
    for o in outputs {
        w = w.max(o.origin.0 + o.size.0);
        h = h.max(o.origin.1 + o.size.1);
    }
    (w.max(0.0), h.max(0.0))
}

/// Build the lock-screen scene spanning every output in `outputs`, themed
/// via `theme`, using `text` for glyph-shaped labels, from the live
/// [`LockView`].
///
/// The overlay fills the **combined** geometry of all outputs so app/chrome
/// content is hidden everywhere (see
/// [`crate::compositor::backend::combined_geometry`]); the interactive
/// widget is drawn once per output, centred on that output (see
/// [`LockOutput`]).
///
/// `view.notification_count`, when non-zero, shows "N notifications" —
/// never any app name, summary, or body.
///
/// Returns a fresh [`Scene`] the backend can submit exactly like a chrome frame.
pub fn draw_lock_scene(
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) -> Scene {
    draw_lock_scene_with_wallpaper(outputs, theme, text, view, None)
}

/// As [`draw_lock_scene`], using an optional compositor-owned private image
/// behind the lock scrim. The image data has already been normalised and read
/// from qOS's private cache; this renderer never opens an app-provided URI.
pub fn draw_lock_scene_with_wallpaper(
    outputs: &[LockOutput],
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
    wallpaper: Option<&ImageData>,
) -> Scene {
    let mut scene = Scene::new();

    // 1. Opaque backdrop over the combined geometry of every output — the
    //    full bounding box (not per-output rects) guarantees no gap ever
    //    shows app content.
    let (ext_w, ext_h) = combined_extent(outputs);
    if ext_w > 0.0 && ext_h > 0.0 {
        let backdrop = Rect::new(0.0, 0.0, ext_w, ext_h);
        let mut wallpaper_layer = false;
        if let Some(image) = wallpaper
            && image.width > 0
            && image.height > 0
        {
            let scale = (ext_w / f64::from(image.width)).max(ext_h / f64::from(image.height));
            let width = f64::from(image.width) * scale;
            let height = f64::from(image.height) * scale;
            let brush_transform =
                Affine::translate(((ext_w - width) / 2.0, (ext_h - height) / 2.0))
                    .pre_scale_non_uniform(scale, scale);
            let brush = ImageBrush::new(image.clone()).with_extend(Extend::Pad);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                &brush,
                Some(brush_transform),
                &backdrop,
            );
            // A firm dark scrim leaves the lock experience legible and does
            // not expose a bright image through password/notification chrome.
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                0.68,
                Affine::IDENTITY,
                &backdrop,
            );
            wallpaper_layer = true;
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.bg_app,
            None,
            &backdrop,
        );
        if wallpaper_layer {
            scene.pop_layer();
        }
    }

    // 2. One interactive lock widget per output, centred on that output.
    for output in outputs {
        draw_lock_widget(&mut scene, output.center(), theme, text, view);
    }

    scene
}

/// Draw the interactive lock widget (clock, padlock, password field, status
/// line, notification summary) centred at `(cx, cy)`. Factored out of
/// [`draw_lock_scene`] so it can be stamped once per output; draws no
/// backdrop of its own.
///
/// Split into per-section helpers (below) purely to keep each function
/// short; the sections still run in the same top-to-bottom order and each
/// one's vertical position is threaded through from the one above it.
fn draw_lock_widget(
    scene: &mut Scene,
    (cx, cy): (f64, f64),
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    draw_lock_clock_and_date(scene, cx, cy, theme, text, view);
    let title_y = draw_lock_padlock_and_title(scene, cx, cy, theme, text);
    let field_geom = draw_lock_password_field(scene, cx, title_y, theme, text, view);
    draw_lock_status_and_notifications(scene, cx, field_geom, theme, text, view);
}

/// Clock + date, centred in the upper third. Part of [`draw_lock_widget`].
fn draw_lock_clock_and_date(
    scene: &mut Scene,
    cx: f64,
    cy: f64,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    if !view.clock.is_empty() {
        let clock_size = 64.0_f32;
        let (cw, _) = text.measure(view.clock, clock_size, None);
        text.draw_text(
            scene,
            view.clock,
            clock_size,
            theme.text_primary,
            Affine::translate((cx - f64::from(cw) / 2.0, cy - 170.0)),
            None,
        );
    }
    if !view.date.is_empty() {
        let date_size = 18.0_f32;
        let (dw, _) = text.measure(view.date, date_size, None);
        text.draw_text(
            scene,
            view.date,
            date_size,
            theme.text_dim,
            Affine::translate((cx - f64::from(dw) / 2.0, cy - 120.0)),
            None,
        );
    }
}

/// Padlock glyph (body, shackle, keyhole) + the "Locked" title beneath it.
/// Returns the title's baseline y — where the password field starts. Part
/// of [`draw_lock_widget`].
fn draw_lock_padlock_and_title(
    scene: &mut Scene,
    cx: f64,
    cy: f64,
    theme: &Theme,
    text: &mut TextRenderer,
) -> f64 {
    // Padlock body — a rounded rectangle just above centre.
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

    // Shackle — upper semicircle on the body's top edge. In screen
    // (y-down) space, start_angle = PI sweeping +PI traces the upper half.
    let r = bw / 3.2;
    let shackle = Arc::new(Point::new(cx, body_y), (r, r), PI, PI, 0.0);
    scene.stroke(
        &Stroke::new(8.0),
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &shackle,
    );

    // Keyhole — a dot punched into the body in the backdrop colour.
    let keyhole = Circle::new(Point::new(cx, body_y + bh / 2.0), 6.0);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &keyhole,
    );

    // "Locked" title.
    let title = "Locked";
    let title_size = 24.0_f32;
    let (tw, _) = text.measure(title, title_size, None);
    let title_y = body_y + bh + 30.0;
    text.draw_text(
        scene,
        title,
        title_size,
        theme.text_primary,
        Affine::translate((cx - f64::from(tw) / 2.0, title_y)),
        None,
    );
    title_y
}

/// Password field — rounded pill, one dot per typed character. Only the
/// length is available here, never the plaintext. Returns `(field_y,
/// field_h)` so the status line below can be positioned. Part of
/// [`draw_lock_widget`].
fn draw_lock_password_field(
    scene: &mut Scene,
    cx: f64,
    title_y: f64,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) -> (f64, f64) {
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
    if view.verifying {
        // Buffer handed to the worker: no dots, no placeholder — the
        // status line below reads "Verifying…".
    } else if view.password_len == 0 {
        // Placeholder prompt when empty.
        let ph = "Enter password";
        let ph_size = 15.0_f32;
        let (pw, ph_h) = text.measure(ph, ph_size, None);
        text.draw_text(
            scene,
            ph,
            ph_size,
            theme.text_dim,
            Affine::translate((
                cx - f64::from(pw) / 2.0,
                field_y + field_h / 2.0 + f64::from(ph_h) / 2.0 - 3.0,
            )),
            None,
        );
    } else {
        // One dot per character, centred, capped so a very long password
        // can't overflow the field.
        let dot_r = 4.0_f64;
        let gap = 14.0_f64;
        let shown = view.password_len.min(18);
        // `shown` is capped to at most 18 by the `.min` above, so both
        // usize -> f64 conversions below are always exact.
        #[allow(
            clippy::cast_precision_loss,
            reason = "shown and i are both bounded by `.min(18)` above, so this usize -> f64 cast never loses precision"
        )]
        {
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
    }
    (field_y, field_h)
}

/// Status line (verifying / error / hint) plus the count-only notification
/// summary below the password field. Part of [`draw_lock_widget`].
fn draw_lock_status_and_notifications(
    scene: &mut Scene,
    cx: f64,
    (field_y, field_h): (f64, f64),
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    // Status line under the field: "Verifying…" while a check runs, else
    // the last error (danger colour), else the neutral hint.
    let line_y = field_y + field_h + 26.0;
    if view.verifying {
        let verifying = "Verifying…";
        let v_size = 14.0_f32;
        let (vw, _) = text.measure(verifying, v_size, None);
        text.draw_text(
            scene,
            verifying,
            v_size,
            theme.text_dim,
            Affine::translate((cx - f64::from(vw) / 2.0, line_y)),
            None,
        );
    } else if let Some(err) = view.error {
        let err_size = 14.0_f32;
        let (ew, _) = text.measure(err, err_size, None);
        text.draw_text(
            scene,
            err,
            err_size,
            theme.tokens.colors.semantic.danger,
            Affine::translate((cx - f64::from(ew) / 2.0, line_y)),
            None,
        );
    } else {
        let hint = "Type your password and press Enter";
        let hint_size = 13.0_f32;
        let (hw, _) = text.measure(hint, hint_size, None);
        text.draw_text(
            scene,
            hint,
            hint_size,
            theme.text_dim,
            Affine::translate((cx - f64::from(hw) / 2.0, line_y)),
            None,
        );
    }

    // Count-only notification summary (lock-suppression). Show
    // "N notifications" — never any content.
    let label = count_label(view.notification_count);
    if !label.is_empty() {
        let label_size = 14.0_f32;
        let (lw, _) = text.measure(&label, label_size, None);
        text.draw_text(
            scene,
            &label,
            label_size,
            theme.text_dim,
            Affine::translate((cx - f64::from(lw) / 2.0, line_y + 34.0)),
            None,
        );
    }
}

/// Lock-screen count label: "N notifications" (empty for zero); reuses the
/// shell's redaction helper so the lock screen and toast stack agree on
/// wording.
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

    /// A single output at the origin of size `w`x`h` — the winit / headless
    /// (and single-monitor DRM) case.
    fn single(w: f64, h: f64) -> [LockOutput; 1] {
        [LockOutput {
            origin: (0.0, 0.0),
            size: (w, h),
        }]
    }

    /// The lock scene must encode *something* — a blank scene would mean
    /// the overlay never paints and app content leaks through, defeating
    /// the lock.
    #[test]
    fn lock_scene_is_non_empty() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &view());
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
        let _ = draw_lock_scene(&single(1.0, 1.0), &theme, &mut text, &v);
    }

    /// An empty output list must not panic and produces an empty scene
    /// (the caller falls back to a default viewport before this is hit).
    #[test]
    fn lock_scene_with_no_outputs_is_empty_not_a_panic() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let scene = draw_lock_scene(&[], &theme, &mut text, &view());
        assert!(scene.encoding().is_empty());
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
            let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
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
        let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
        assert!(!scene.encoding().is_empty());
    }

    /// The "Verifying…" state paints without panicking.
    #[test]
    fn lock_scene_renders_verifying_state() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let v = LockView {
            verifying: true,
            ..view()
        };
        let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
        assert!(!scene.encoding().is_empty());
    }

    /// The count-only summary never renders content; we can't read text
    /// back out of a Vello scene, so assert the contract at the label
    /// helper instead.
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
        let scene = draw_lock_scene(&single(1600.0, 900.0), &theme, &mut text, &v);
        assert!(!scene.encoding().is_empty());
    }

    // ---- multi-output ---------------------------------------------------

    /// Each output's widget is anchored at that output's centre. A second
    /// monitor at `(1920, 0)` centres its widget at `(2560, 512)`, not at the
    /// combined-space centre — so the lock is usable on every screen instead
    /// of being stranded across the bezel.
    #[test]
    fn output_center_is_the_middle_of_each_output() {
        let primary = LockOutput {
            origin: (0.0, 0.0),
            size: (1920.0, 1080.0),
        };
        assert_eq!(primary.center(), (960.0, 540.0));
        let secondary = LockOutput {
            origin: (1920.0, 0.0),
            size: (1280.0, 1024.0),
        };
        assert_eq!(secondary.center(), (2560.0, 512.0));
    }

    /// The backdrop spans the bounding box of every output, so no gap between
    /// screens can ever show app content.
    #[test]
    fn combined_extent_covers_all_outputs() {
        let outs = [
            LockOutput {
                origin: (0.0, 0.0),
                size: (1920.0, 1080.0),
            },
            LockOutput {
                origin: (1920.0, 0.0),
                size: (1280.0, 1024.0),
            },
        ];
        assert_eq!(combined_extent(&outs), (3200.0, 1080.0));
        assert_eq!(combined_extent(&single(1600.0, 900.0)), (1600.0, 900.0));
        assert_eq!(combined_extent(&[]), (0.0, 0.0));
    }

    /// Two outputs must stamp a widget on **each** — proven by strictly more
    /// draw operations than the single-output scene (a per-output widget adds
    /// its glyphs + shapes on top of the shared backdrop).
    #[test]
    fn multi_output_draws_a_widget_per_output() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();

        let one = draw_lock_scene(&single(1920.0, 1080.0), &theme, &mut text, &view());
        let two_outs = [
            LockOutput {
                origin: (0.0, 0.0),
                size: (1920.0, 1080.0),
            },
            LockOutput {
                origin: (1920.0, 0.0),
                size: (1920.0, 1080.0),
            },
        ];
        let two = draw_lock_scene(&two_outs, &theme, &mut text, &view());

        let one_ops = one.encoding().draw_tags.len();
        let two_ops = two.encoding().draw_tags.len();
        assert!(
            two_ops > one_ops,
            "two outputs must draw more than one (per-output widget): {two_ops} vs {one_ops}"
        );
    }
}
