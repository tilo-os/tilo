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

use ui::accesskit::{
    Action as AccessAction, Node as AccessNode, NodeId as AccessNodeId, Rect as AccessRect,
    Role as AccessRole, TreeId, TreeInfo, TreeUpdate,
};

// `ui::accesskit_bridge` reserves 0, low ids, 1<<60, 2<<60, 1<<62 and
// 1<<63 for desktop widgets/resources/actions/text/synthetic nodes. Keeping
// the lock in the unused 3<<60 range makes a stale hidden-desktop action
// incapable of aliasing a credential control after the tree swap.
const LOCK_ID_BASE: u64 = 3_u64 << 60;
pub const LOCK_ROOT_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE);
pub const LOCK_PASSWORD_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 1);
const LOCK_CLOCK_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 2);
const LOCK_HELP_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 3);
const LOCK_STATUS_ID: AccessNodeId = AccessNodeId(LOCK_ID_BASE + 4);

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
    /// Device scale reported for this output. Lock chrome is compositor-owned,
    /// so it must apply this itself rather than relying on a Wayland client to
    /// render at the requested scale.
    pub scale: f64,
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

/// Build the complete, compositor-owned semantic tree shown while the session
/// is locked. It intentionally contains no application/chrome nodes and never
/// publishes the password value (not even its masked representation).
///
/// The password field supports AT focus and value entry; Enter remains the
/// single submit gesture, shared with physical keyboard input. This avoids a
/// synthetic "Unlock" control that is not present on the painted surface.
pub fn build_accessibility_tree(outputs: &[LockOutput], view: &LockView<'_>) -> TreeUpdate {
    let (width, height) = combined_extent(outputs);
    let output = outputs.first().copied().unwrap_or(LockOutput {
        origin: (0.0, 0.0),
        size: (width, height),
        scale: 1.0,
    });
    let (cx, cy) = output.center();

    let mut root = AccessNode::new(AccessRole::Window);
    root.set_label("qOS lock screen");
    root.set_bounds(AccessRect::new(0.0, 0.0, width, height));

    let mut clock = AccessNode::new(AccessRole::Label);
    let clock_label = if view.clock.is_empty() {
        "Session locked".to_string()
    } else if view.date.is_empty() {
        format!("Session locked, {}", view.clock)
    } else {
        format!("Session locked, {}, {}", view.clock, view.date)
    };
    clock.set_label(clock_label);

    let mut password = AccessNode::new(AccessRole::TextInput);
    password.set_label("Password");
    password.set_bounds(AccessRect::new(
        cx - 140.0,
        cy + 42.0,
        cx + 140.0,
        cy + 86.0,
    ));
    password.add_action(AccessAction::Focus);
    if view.verifying {
        password.set_disabled();
    } else {
        password.add_action(AccessAction::SetValue);
    }

    let mut help = AccessNode::new(AccessRole::Label);
    help.set_label("Type your password and press Enter");

    let mut children = vec![LOCK_CLOCK_ID, LOCK_PASSWORD_ID, LOCK_HELP_ID];
    let mut nodes = vec![
        (LOCK_CLOCK_ID, clock),
        (LOCK_PASSWORD_ID, password),
        (LOCK_HELP_ID, help),
    ];
    let status = if view.verifying {
        Some("Verifying password".to_string())
    } else if let Some(error) = view.error {
        Some(error.to_string())
    } else if view.notification_count > 0 {
        Some(format!("{} notifications", view.notification_count))
    } else {
        None
    };
    if let Some(status) = status {
        let mut node = AccessNode::new(AccessRole::Label);
        node.set_label(status);
        children.push(LOCK_STATUS_ID);
        nodes.push((LOCK_STATUS_ID, node));
    }
    root.set_children(children);
    nodes.push((LOCK_ROOT_ID, root));
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo::new(LOCK_ROOT_ID)),
        tree_id: TreeId::ROOT,
        focus: if view.verifying {
            LOCK_ROOT_ID
        } else {
            LOCK_PASSWORD_ID
        },
    }
}

/// Responsive geometry for one output's lock chrome. Keeping layout separate
/// from painting makes containment and non-overlap contractual instead of a
/// property inferred from screenshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockLayout {
    pub output: Rect,
    pub clock: Rect,
    pub date: Rect,
    pub panel: Rect,
    pub icon: Rect,
    pub title: Rect,
    pub password: Rect,
    pub status: Rect,
    pub notifications: Rect,
    pub scale: f64,
}

impl LockLayout {
    /// Resolve a bounded lock layout for one output. The device/theme scale is
    /// honoured when it fits and reduced on compact outputs, so a high-DPI
    /// laptop never clips the password/error controls below the panel.
    pub fn for_output(output: LockOutput, theme: &Theme) -> Self {
        let width = output.size.0.max(1.0);
        let height = output.size.1.max(1.0);
        let output_rect = Rect::new(
            output.origin.0,
            output.origin.1,
            output.origin.0 + width,
            output.origin.1 + height,
        );
        let edge = (width.min(height) * 0.035).clamp(1.0, 28.0);
        let desired_scale = (output.scale * f64::from(theme.ui_scale)).clamp(0.5, 3.0);
        let width_fit = ((width - edge * 2.0) / 420.0).max(0.05);
        let height_fit = ((height - edge * 2.0) / 470.0).max(0.05);
        let scale = desired_scale.min(width_fit).min(height_fit).max(0.05);

        let panel_width = 400.0 * scale;
        let panel_height = 270.0 * scale;
        let clock_height = 78.0 * scale;
        let date_height = 28.0 * scale;
        let clock_date_gap = 4.0 * scale;
        let panel_gap = 34.0 * scale;
        let stack_height = clock_height + clock_date_gap + date_height + panel_gap + panel_height;
        let top = output.origin.1 + (height - stack_height) / 2.0;
        let cx = output.origin.0 + width / 2.0;

        let clock = Rect::new(
            output.origin.0 + edge,
            top,
            output.origin.0 + width - edge,
            top + clock_height,
        );
        let date_y = clock.y1 + clock_date_gap;
        let date = Rect::new(clock.x0, date_y, clock.x1, date_y + date_height);
        let panel_y = date.y1 + panel_gap;
        let panel = Rect::new(
            cx - panel_width / 2.0,
            panel_y,
            cx + panel_width / 2.0,
            panel_y + panel_height,
        );

        let icon_size = 42.0 * scale;
        let icon = Rect::new(
            cx - icon_size / 2.0,
            panel.y0 + 22.0 * scale,
            cx + icon_size / 2.0,
            panel.y0 + (22.0 * scale) + icon_size,
        );
        let title = Rect::new(
            panel.x0 + 28.0 * scale,
            icon.y1 + 8.0 * scale,
            panel.x1 - 28.0 * scale,
            icon.y1 + 40.0 * scale,
        );
        let password = Rect::new(
            panel.x0 + 30.0 * scale,
            title.y1 + 14.0 * scale,
            panel.x1 - 30.0 * scale,
            title.y1 + 64.0 * scale,
        );
        let status = Rect::new(
            password.x0,
            password.y1 + 10.0 * scale,
            password.x1,
            password.y1 + 42.0 * scale,
        );
        let notifications = Rect::new(
            password.x0,
            status.y1 + 4.0 * scale,
            password.x1,
            panel.y1 - 14.0 * scale,
        );

        Self {
            output: output_rect,
            clock,
            date,
            panel,
            icon,
            title,
            password,
            status,
            notifications,
            scale,
        }
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
        draw_lock_widget(
            &mut scene,
            LockLayout::for_output(*output, theme),
            theme,
            text,
            view,
        );
    }

    scene
}

/// Draw the interactive lock widget from one already-bounded layout.
fn draw_lock_widget(
    scene: &mut Scene,
    layout: LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    if !view.clock.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.clock,
            scaled_text_size(64.0, layout.scale, theme),
            theme.text_primary,
            layout.clock,
        );
    }
    if !view.date.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.date,
            scaled_text_size(18.0, layout.scale, theme),
            theme.text_dim,
            layout.date,
        );
    }

    let panel = RoundedRect::from_rect(layout.panel, 24.0 * layout.scale);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_primary,
        None,
        &panel,
    );
    scene.stroke(
        &Stroke::new((1.0 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        theme.border_standard,
        None,
        &panel,
    );

    draw_lock_icon(scene, layout, theme);
    draw_centered_text(
        scene,
        text,
        "Session locked",
        scaled_text_size(22.0, layout.scale, theme),
        theme.text_primary,
        layout.title,
    );
    draw_password_field(scene, layout, theme, text, view);
    draw_status(scene, layout, theme, text, view);
    let label = count_label(view.notification_count);
    if !label.is_empty() {
        draw_centered_text(
            scene,
            text,
            &label,
            scaled_text_size(13.0, layout.scale, theme),
            theme.text_dim,
            layout.notifications,
        );
    }
}

fn scaled_text_size(base: f32, scale: f64, theme: &Theme) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded lock-layout scale is at most 3; f32 is exact enough for font pixels"
    )]
    let scale = scale as f32;
    base * scale * theme.text_scale
}

fn draw_centered_text(
    scene: &mut Scene,
    text: &mut TextRenderer,
    value: &str,
    size: f32,
    color: vello::peniko::Color,
    bounds: Rect,
) {
    if value.is_empty() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "lock output geometry is bounded to compositor dimensions; f32 width is sufficient for text shaping"
    )]
    let max_width = bounds.width() as f32;
    let (width, height) = text.measure(value, size, Some(max_width));
    let x = bounds.x0 + (bounds.width() - f64::from(width.min(max_width))) / 2.0;
    let y = bounds.y0 + (bounds.height() - f64::from(height)) / 2.0;
    text.draw_text(
        scene,
        value,
        size,
        color,
        Affine::translate((x, y)),
        Some(max_width),
    );
}

fn draw_lock_icon(scene: &mut Scene, layout: LockLayout, theme: &Theme) {
    let cx = layout.icon.center().x;
    let body_width = layout.icon.width() * 0.72;
    let body_height = layout.icon.height() * 0.50;
    let body_y = layout.icon.y0 + layout.icon.height() * 0.46;
    let body = RoundedRect::new(
        cx - body_width / 2.0,
        body_y,
        cx + body_width / 2.0,
        body_y + body_height,
        6.0 * layout.scale,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &body,
    );

    let r = body_width * 0.31;
    let shackle = Arc::new(Point::new(cx, body_y), (r, r), PI, PI, 0.0);
    scene.stroke(
        &Stroke::new((5.0 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        theme.accent_primary,
        None,
        &shackle,
    );

    let keyhole = Circle::new(
        Point::new(cx, body_y + body_height / 2.0),
        3.5 * layout.scale,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &keyhole,
    );
}

fn draw_password_field(
    scene: &mut Scene,
    layout: LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    let field = RoundedRect::from_rect(layout.password, layout.password.height() / 2.0);
    scene.fill(Fill::NonZero, Affine::IDENTITY, theme.bg_app, None, &field);
    // A subtle border — focused colour, since the field always holds focus
    // while locked.
    scene.stroke(
        &Stroke::new((1.5 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        theme.border_focus,
        None,
        &field,
    );
    if view.verifying {
        // The status below names the in-flight action. Keeping the field
        // visually quiet prevents it looking like a second prompt.
    } else if view.password_len == 0 {
        draw_centered_text(
            scene,
            text,
            "Enter password",
            scaled_text_size(15.0, layout.scale, theme),
            theme.text_dim,
            layout.password,
        );
    } else {
        let dot_r = 4.0 * layout.scale;
        let gap = 14.0 * layout.scale;
        let shown = view.password_len.min(18);
        // `shown` is capped to at most 18 by the `.min` above, so both
        // usize -> f64 conversions below are always exact.
        #[allow(
            clippy::cast_precision_loss,
            reason = "shown and i are both bounded by `.min(18)` above, so this usize -> f64 cast never loses precision"
        )]
        {
            let total_w = (shown as f64 - 1.0).max(0.0) * gap;
            let start_x = layout.password.center().x - total_w / 2.0;
            let dot_y = layout.password.center().y;
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
}

fn draw_status(
    scene: &mut Scene,
    layout: LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    let (value, color, size) = if view.verifying {
        ("Verifying…", theme.text_dim, 14.0)
    } else if let Some(err) = view.error {
        (err, theme.tokens.colors.semantic.danger, 13.0)
    } else {
        ("Press Enter to unlock", theme.text_dim, 13.0)
    };
    draw_centered_text(
        scene,
        text,
        value,
        scaled_text_size(size, layout.scale, theme),
        color,
        layout.status,
    );
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
            scale: 1.0,
        }]
    }

    #[test]
    fn lock_accessibility_tree_is_actionable_and_never_exposes_password() {
        let v = LockView {
            password_len: 12,
            notification_count: 4,
            ..view()
        };
        let update = build_accessibility_tree(&single(1600.0, 900.0), &v);
        assert_eq!(update.focus, LOCK_PASSWORD_ID);
        let (_, password) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == LOCK_PASSWORD_ID)
            .expect("password field is present");
        assert_eq!(password.role(), AccessRole::TextInput);
        assert_eq!(password.label(), Some("Password"));
        assert!(password.supports_action(AccessAction::Focus));
        assert!(password.supports_action(AccessAction::SetValue));
        assert_eq!(
            password.value(),
            None,
            "masked credentials are not semantic values"
        );
        assert!(
            update.nodes.iter().all(|(_, node)| {
                node.label()
                    .is_none_or(|label| !label.contains("••") && !label.contains("********"))
            }),
            "the semantic tree must not encode the password or its mask"
        );
    }

    #[test]
    fn verifying_lock_tree_disables_password_mutation_and_announces_state() {
        let v = LockView {
            verifying: true,
            ..view()
        };
        let update = build_accessibility_tree(&single(800.0, 600.0), &v);
        assert_eq!(update.focus, LOCK_ROOT_ID);
        let (_, password) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == LOCK_PASSWORD_ID)
            .expect("password field is present");
        assert!(!password.supports_action(AccessAction::SetValue));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.label()
                .is_some_and(|label| label == "Verifying password")
        }));
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

    fn assert_inside(inner: Rect, outer: Rect, name: &str) {
        assert!(
            inner.x0 >= outer.x0
                && inner.y0 >= outer.y0
                && inner.x1 <= outer.x1
                && inner.y1 <= outer.y1
                && inner.x0 <= inner.x1
                && inner.y0 <= inner.y1,
            "{name} {inner:?} escapes output {outer:?}"
        );
    }

    #[test]
    fn responsive_layout_is_bounded_and_non_overlapping() {
        let theme = Theme::dark();
        for output in [
            LockOutput {
                origin: (0.0, 0.0),
                size: (1600.0, 900.0),
                scale: 1.0,
            },
            LockOutput {
                origin: (0.0, 0.0),
                size: (1366.0, 768.0),
                scale: 1.25,
            },
            LockOutput {
                origin: (0.0, 0.0),
                size: (1280.0, 720.0),
                scale: 1.0,
            },
            LockOutput {
                origin: (40.0, 30.0),
                size: (1280.0, 720.0),
                scale: 2.0,
            },
        ] {
            let layout = LockLayout::for_output(output, &theme);
            for (name, rect) in [
                ("clock", layout.clock),
                ("date", layout.date),
                ("panel", layout.panel),
                ("icon", layout.icon),
                ("title", layout.title),
                ("password", layout.password),
                ("status", layout.status),
                ("notifications", layout.notifications),
            ] {
                assert_inside(rect, layout.output, name);
            }
            assert!(layout.clock.y1 <= layout.date.y0);
            assert!(layout.date.y1 <= layout.panel.y0);
            assert!(layout.icon.y1 <= layout.title.y0);
            assert!(layout.title.y1 <= layout.password.y0);
            assert!(layout.password.y1 <= layout.status.y0);
            assert!(layout.status.y1 <= layout.notifications.y0);
            assert_inside(layout.icon, layout.panel, "icon/panel");
            assert_inside(layout.title, layout.panel, "title/panel");
            assert_inside(layout.password, layout.panel, "password/panel");
            assert_inside(layout.status, layout.panel, "status/panel");
            assert_inside(layout.notifications, layout.panel, "notifications/panel");
        }
    }

    #[test]
    fn asymmetric_outputs_get_independent_bounded_layouts() {
        let theme = Theme::dark();
        for output in [
            LockOutput {
                origin: (0.0, 0.0),
                size: (1366.0, 768.0),
                scale: 1.25,
            },
            LockOutput {
                origin: (1366.0, 48.0),
                size: (1280.0, 720.0),
                scale: 1.0,
            },
        ] {
            let layout = LockLayout::for_output(output, &theme);
            assert_inside(layout.panel, layout.output, "panel");
            assert_eq!(
                layout.panel.x0 + layout.panel.x1,
                output.origin.0 * 2.0 + output.size.0
            );
        }
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
            scale: 1.0,
        };
        assert_eq!(primary.center(), (960.0, 540.0));
        let secondary = LockOutput {
            origin: (1920.0, 0.0),
            size: (1280.0, 1024.0),
            scale: 1.0,
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
                scale: 1.0,
            },
            LockOutput {
                origin: (1920.0, 0.0),
                size: (1280.0, 1024.0),
                scale: 1.0,
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
                scale: 1.0,
            },
            LockOutput {
                origin: (1920.0, 0.0),
                size: (1920.0, 1080.0),
                scale: 1.0,
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
