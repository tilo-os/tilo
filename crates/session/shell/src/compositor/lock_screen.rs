//! The compositor-owned session-lock overlay.
//!
//! When [`crate::compositor::idle::SessionManager::is_locked`] is set,
//! [`crate::compositor::native::run`] paints *this* scene instead of the
//! chrome's, hiding app/chrome content and routing every keystroke into the
//! lock screen's password field ([`crate::compositor::lock_controller`]) —
//! hidden content + captured input + credential-gated unlock together are
//! the lock's security boundary. (`ext-session-lock-v1` is also served as a
//! standard trigger; Tilo renders this overlay rather than a client lock
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

use crate::compositor::lock_auth::UNAVAILABLE_MSG;

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
/// coordinate space. The compositor's first output is the primary lock
/// target; secondary displays retain a private, useful indication without
/// duplicating a password target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockOutput {
    /// Top-left of this output in the combined space.
    pub origin: (f64, f64),
    /// Output size in compositor logical pixels.
    pub size: (f64, f64),
    /// Device scale reported for this output. Lock chrome is compositor-owned,
    /// while geometry itself is already in compositor logical coordinates.
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
    /// Resolve a bounded primary-output layout. The backend already gives the
    /// compositor logical output geometry, therefore device scale must not be
    /// multiplied into the card a second time. Only Tilo's explicit UI scale
    /// participates here, then is capped by available space.
    pub fn for_output(output: LockOutput, theme: &Theme) -> Self {
        let width = output.size.0.max(1.0);
        let height = output.size.1.max(1.0);
        let output_rect = Rect::new(
            output.origin.0,
            output.origin.1,
            output.origin.0 + width,
            output.origin.1 + height,
        );
        let edge = (width.min(height) * 0.035)
            .clamp(1.0, 28.0)
            .min(width / 2.0)
            .min(height / 2.0);
        let desired_scale = f64::from(theme.ui_scale).clamp(0.8, 1.6);
        // Text-only accessibility scaling must reserve room in the lock
        // chrome too.  The compositor draws the clock and the form labels
        // directly, rather than through the normal widget layout engine, so
        // leaving these slots at their default dimensions made large type
        // encroach on the date and status rows.
        let text_scale = f64::from(theme.text_scale).clamp(0.5, 4.0);
        let unscaled_panel_height = 222.0 + 126.0 * (text_scale - 1.0);
        let unscaled_stack_height =
            72.0 * text_scale + 2.0 * text_scale + 26.0 * text_scale + 24.0 + unscaled_panel_height;
        let width_fit = ((width - edge * 2.0) / 408.0).max(0.0);
        let height_fit = ((height - edge * 2.0) / unscaled_stack_height).max(0.0);
        // Startup can briefly report a 1×1 output before the first modeset.
        // Keep every rect contained even then; this is not a user-facing scale.
        let scale = desired_scale.min(width_fit).min(height_fit).max(0.001);

        let panel_width = 384.0 * scale;
        let panel_height = unscaled_panel_height * scale;
        let clock_height = 72.0 * text_scale * scale;
        let date_height = 26.0 * text_scale * scale;
        let clock_date_gap = 2.0 * text_scale * scale;
        let panel_gap = 24.0 * scale;
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

        let icon_size = 34.0 * scale;
        let icon = Rect::new(
            cx - icon_size / 2.0,
            panel.y0 + 18.0 * scale,
            cx + icon_size / 2.0,
            panel.y0 + (18.0 * scale) + icon_size,
        );
        let title = Rect::new(
            panel.x0 + 24.0 * scale,
            icon.y1 + 6.0 * scale,
            panel.x1 - 24.0 * scale,
            icon.y1 + 34.0 * text_scale * scale,
        );
        let password = Rect::new(
            panel.x0 + 30.0 * scale,
            title.y1 + 12.0 * scale,
            panel.x1 - 30.0 * scale,
            title.y1 + 12.0 * scale + 46.0 * text_scale * scale,
        );
        let status = Rect::new(
            password.x0,
            password.y1 + 8.0 * scale,
            password.x1,
            password.y1 + 8.0 * scale + 26.0 * text_scale * scale,
        );
        let notifications = Rect::new(
            password.x0,
            status.y1 + 2.0 * scale,
            password.x1,
            panel.y1 - 10.0 * scale,
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

/// Resolve the one output that carries the credential surface. A hot-unplug
/// can remove the previous primary connector while locked; then the first
/// surviving output takes over rather than leaving the password unreachable.
/// Backend output ordering is the compositor's primary-output policy.
fn primary_output(outputs: &[LockOutput]) -> Option<LockOutput> {
    outputs.first().copied()
}

/// The locked semantic tree has one password target, matching the primary
/// output. It never publishes a password or masked value to assistive tech.
pub fn build_accessibility_tree(
    outputs: &[LockOutput],
    theme: &Theme,
    view: &LockView<'_>,
) -> TreeUpdate {
    let (width, height) = combined_extent(outputs);
    let output = primary_output(outputs).unwrap_or_else(|| LockOutput {
        origin: (0.0, 0.0),
        size: (width.max(1.0), height.max(1.0)),
        scale: 1.0,
    });
    let layout = LockLayout::for_output(output, theme);

    let mut root = AccessNode::new(AccessRole::Window);
    root.set_label("Tilo lock screen");
    root.set_bounds(AccessRect::new(0.0, 0.0, width.max(1.0), height.max(1.0)));

    let mut clock = AccessNode::new(AccessRole::Label);
    clock.set_label(match (view.clock.is_empty(), view.date.is_empty()) {
        (true, _) => "Session locked".to_string(),
        (false, true) => format!("Session locked, {}", view.clock),
        (false, false) => format!("Session locked, {}, {}", view.clock, view.date),
    });
    clock.set_bounds(access_rect(layout.clock));

    // `PasswordInput` maps to AT-SPI's PasswordText role. That lets Orca and
    // braille clients suppress credential echo while retaining editable text.
    let mut password = AccessNode::new(AccessRole::PasswordInput);
    password.set_label("Password");
    password.set_bounds(access_rect(layout.password));
    password.add_action(AccessAction::Focus);
    if view.verifying {
        password.set_disabled();
    } else {
        password.add_action(AccessAction::SetValue);
        // A screen-reader user must be able to complete the same trusted
        // credential flow as a keyboard user. Click is AccessKit's standard
        // activation action; native maps it to LockKey::Submit and nowhere
        // else, so it does not expose hidden desktop actions while locked.
        password.add_action(AccessAction::Click);
    }

    let mut help = AccessNode::new(AccessRole::Label);
    help.set_label("Type your password and press Enter");
    help.set_bounds(access_rect(layout.status));

    let mut children = vec![LOCK_CLOCK_ID, LOCK_PASSWORD_ID, LOCK_HELP_ID];
    let mut nodes = vec![
        (LOCK_CLOCK_ID, clock),
        (LOCK_PASSWORD_ID, password),
        (LOCK_HELP_ID, help),
    ];
    let status = if view.verifying {
        Some("Verifying password".to_string())
    } else if let Some(error) = view.error {
        Some(lock_error_status(error).to_string())
    } else if view.notification_count > 0 {
        Some(count_label(view.notification_count))
    } else {
        None
    };
    if let Some(status) = status {
        let mut node = AccessNode::new(AccessRole::Label);
        node.set_label(status);
        node.set_bounds(access_rect(layout.notifications));
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

fn access_rect(rect: Rect) -> AccessRect {
    AccessRect::new(rect.x0, rect.y0, rect.x1, rect.y1)
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
/// credential widget is drawn on the designated primary output. Secondary
/// displays stay private and informative, but do not duplicate password UI.
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
/// from Tilo's private cache; this renderer never opens an app-provided URI.
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

    // 2. One credential surface on the primary display. Secondary displays
    // deliberately have a quieter non-interactive indication.
    let primary = primary_output(outputs);
    for output in outputs {
        let layout = LockLayout::for_output(*output, theme);
        if Some(*output) == primary {
            draw_lock_widget(&mut scene, &layout, theme, text, view);
        } else {
            draw_secondary_lock_widget(&mut scene, &layout, theme, text, view);
        }
    }

    scene
}

/// Draw the interactive lock widget from one already-bounded layout.
fn draw_lock_widget(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    // The lock chrome sizes its type from the theme's text scale itself
    // (`scaled_text_size`); the renderer must not scale it again (HIG §7a)
    // — reset for this pass, as the shell's immediate-mode chrome passes
    // do, and restored at the end.
    let renderer_scale = text.text_scale();
    text.set_text_scale(1.0);
    let type_scale = ui::tokens::TypeScale::default();
    if !view.clock.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.clock,
            scaled_text_size(CLOCK_PX, layout.scale, theme),
            theme.text_primary,
            layout.clock,
        );
    }
    if !view.date.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.date,
            scaled_text_size(type_scale.lg, layout.scale, theme),
            theme.text_dim,
            layout.date,
        );
    }

    // Panel role (HIG §9.1/§9.2): the credential card's corner is the Panel
    // radius token (24) scaled with the rest of the lock chrome, its ground
    // is `panel_ground` (walked clear of the wallpaper-dimmed desktop) and
    // its edge is the derived Panel border rather than a declared colour.
    let panel = RoundedRect::from_rect(
        layout.panel,
        f64::from(theme.tokens.radii.xl) * layout.scale,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        ui::chrome::shell::panel_ground(theme),
        None,
        &panel,
    );
    scene.stroke(
        &Stroke::new((1.0 * layout.scale).max(1.0)),
        Affine::IDENTITY,
        ui::chrome::shell::panel_border(theme),
        None,
        &panel,
    );

    draw_lock_icon(scene, layout, theme);
    draw_centered_text(
        scene,
        text,
        "Session locked",
        scaled_text_size(type_scale.xl, layout.scale, theme),
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
            scaled_text_size(type_scale.sm, layout.scale, theme),
            theme.text_dim,
            layout.notifications,
        );
    }
    text.set_text_scale(renderer_scale);
}

/// Quiet secondary-display indication. It must never paint a second password
/// affordance: focus and assistive technology point at the sole primary field.
fn draw_secondary_lock_widget(
    scene: &mut Scene,
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    // Same renderer reset as the primary widget (HIG §7a).
    let renderer_scale = text.text_scale();
    text.set_text_scale(1.0);
    let type_scale = ui::tokens::TypeScale::default();
    if !view.clock.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.clock,
            scaled_text_size(SECONDARY_CLOCK_PX, layout.scale, theme),
            theme.text_primary,
            layout.clock,
        );
    }
    if !view.date.is_empty() {
        draw_centered_text(
            scene,
            text,
            view.date,
            scaled_text_size(type_scale.lg, layout.scale, theme),
            theme.text_dim,
            layout.date,
        );
    }
    let message = Rect::new(
        layout.output.x0 + 24.0 * layout.scale,
        layout.date.y1 + 26.0 * layout.scale,
        layout.output.x1 - 24.0 * layout.scale,
        layout.date.y1 + 78.0 * layout.scale,
    );
    draw_centered_text(
        scene,
        text,
        "Session locked",
        scaled_text_size(type_scale.lg, layout.scale, theme),
        theme.text_primary,
        Rect::new(
            message.x0,
            message.y0,
            message.x1,
            message.y0 + 24.0 * layout.scale,
        ),
    );
    draw_centered_text(
        scene,
        text,
        "Password prompt is on the primary display",
        scaled_text_size(type_scale.sm, layout.scale, theme),
        theme.text_dim,
        Rect::new(
            message.x0,
            message.y0 + 28.0 * layout.scale,
            message.x1,
            message.y1,
        ),
    );
    text.set_text_scale(renderer_scale);
}

/// The primary clock: a display size with no place on the compressed
/// chrome type scale (HIG §9.6 tops out at `x4l` 40); the one authored size
/// this surface keeps, named so it is not mistaken for a token.
const CLOCK_PX: f32 = 56.0;
/// The secondary display's clock, a step quieter than the primary's.
const SECONDARY_CLOCK_PX: f32 = 52.0;

/// A lock-chrome type size: the *unscaled* role size, multiplied by the
/// layout's bounded scale and the user's text scale. The widget painters
/// reset the renderer for their pass, so this is painted exactly once
/// (HIG §7a).
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

fn draw_lock_icon(scene: &mut Scene, layout: &LockLayout, theme: &Theme) {
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
    layout: &LockLayout,
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
            scaled_text_size(ui::tokens::TypeScale::default().base, layout.scale, theme),
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
    layout: &LockLayout,
    theme: &Theme,
    text: &mut TextRenderer,
    view: &LockView<'_>,
) {
    // One status role for every state (Control sub-line, HIG §9.6): the
    // 14 / 13 / 12 the three states used to pick between were not sizes on
    // the scale and made the row jump as the state changed.
    let type_scale = ui::tokens::TypeScale::default();
    let (value, color) = if view.verifying {
        ("Verifying…", theme.text_dim)
    } else if let Some(err) = view.error {
        // Keep the recovery action visible after a failed attempt. This must
        // stay a single line at 1280x800: wrapping a red status below the
        // password field makes the result look like a second prompt.
        let retry = lock_error_status(err);
        draw_centered_text(
            scene,
            text,
            retry,
            scaled_text_size(type_scale.sm, layout.scale, theme),
            theme.tokens.colors.semantic.danger,
            layout.status,
        );
        return;
    } else {
        ("Press Enter to unlock", theme.text_dim)
    };
    draw_centered_text(
        scene,
        text,
        value,
        scaled_text_size(type_scale.sm, layout.scale, theme),
        color,
        layout.status,
    );
}

/// The visual and accessible recovery line after a failed unlock.
///
/// Auth deliberately returns generic, non-leaky prose, but the full
/// diagnostic plus a recovery instruction exceeded the fixed status row at
/// common 1280x800 layouts. Keep the action in the line while preserving the
/// distinction between a rejected credential and an unavailable verifier.
fn lock_error_status(error: &str) -> &'static str {
    if error == UNAVAILABLE_MSG {
        "Can't verify · Enter your password to try again"
    } else {
        "Sign-in failed · Enter your password to try again"
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
            scale: 1.0,
        }]
    }

    #[test]
    fn lock_accessibility_tree_has_one_actionable_password_and_hides_its_value() {
        let v = LockView {
            password_len: 12,
            notification_count: 4,
            ..view()
        };
        let update = build_accessibility_tree(&single(1600.0, 900.0), &Theme::dark(), &v);
        assert_eq!(update.focus, LOCK_PASSWORD_ID);
        let passwords = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == AccessRole::PasswordInput)
            .collect::<Vec<_>>();
        assert_eq!(
            passwords.len(),
            1,
            "only the primary lock field is semantic"
        );
        let (_, password) = passwords[0];
        assert_eq!(password.label(), Some("Password"));
        assert!(password.supports_action(AccessAction::Focus));
        assert!(password.supports_action(AccessAction::SetValue));
        assert!(password.supports_action(AccessAction::Click));
        assert_eq!(password.value(), None, "credential values never enter AT");
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
        let update = build_accessibility_tree(&single(800.0, 600.0), &Theme::dark(), &v);
        assert_eq!(update.focus, LOCK_ROOT_ID);
        let (_, password) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == LOCK_PASSWORD_ID)
            .expect("password field is present");
        assert!(!password.supports_action(AccessAction::SetValue));
        assert!(!password.supports_action(AccessAction::Click));
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

    #[test]
    fn lock_error_status_is_concise_and_preserves_retry_guidance() {
        assert_eq!(
            lock_error_status("Incorrect username or password"),
            "Sign-in failed · Enter your password to try again"
        );
        assert_eq!(
            lock_error_status(UNAVAILABLE_MSG),
            "Can't verify · Enter your password to try again"
        );
    }

    #[test]
    fn lock_error_status_stays_on_one_line_at_1280_by_800() {
        let theme = Theme::dark();
        let output = LockOutput {
            origin: (0.0, 0.0),
            size: (1280.0, 800.0),
            scale: 1.0,
        };
        let layout = LockLayout::for_output(output, &theme);
        let mut text = TextRenderer::new();
        for message in [
            lock_error_status("Incorrect username or password"),
            lock_error_status(UNAVAILABLE_MSG),
        ] {
            let (width, height) =
                text.measure(message, scaled_text_size(12.0, layout.scale, &theme), None);
            assert!(
                f64::from(width) <= layout.status.width(),
                "{message:?} must not wrap in the lock status row"
            );
            assert!(
                f64::from(height) <= layout.status.height(),
                "{message:?} must fit the lock status row"
            );
        }
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

    /// Review fixtures: the ux-bench cannot lock the session on macOS, so
    /// the lock scene is photographed from its paint path — 1× and 200 %
    /// text on a desktop and a laptop output, plus the light and
    /// high-contrast palettes — with a typed password and a notification
    /// count, and once more in the error state at 200 %.
    /// The rendered pixels are asserted, not just the file: the credential
    /// card's interior is the Panel ground on every palette and scale.
    #[test]
    fn lock_screen_review_fixtures_are_written() {
        use crate::shell::osd::review_offscreen::OffscreenRenderer;
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/ui-review/s2-r2/lock-screen");
        // One wgpu device per output size, shared by every variant of it.
        let mut desktop = OffscreenRenderer::new(1280, 800);
        let mut laptop = OffscreenRenderer::new(800, 700);
        let variants = [
            ("dark-1x-1280x800", Theme::dark(), (1280_u32, 800_u32)),
            (
                "dark-2x-1280x800",
                Theme::dark().with_text_scale(2.0),
                (1280, 800),
            ),
            (
                "dark-2x-800x700",
                Theme::dark().with_text_scale(2.0),
                (800, 700),
            ),
            ("light-1x-1280x800", Theme::light(), (1280, 800)),
            (
                "high-contrast-1x-1280x800",
                Theme::high_contrast(),
                (1280, 800),
            ),
        ];
        for (name, theme, (w, h)) in variants {
            let renderer = if w == 1280 { &mut desktop } else { &mut laptop };
            let outputs = single(f64::from(w), f64::from(h));
            let mut text = TextRenderer::new();
            let typed = LockView {
                password_len: 6,
                notification_count: 3,
                ..view()
            };
            let scene = draw_lock_scene(&outputs, &theme, &mut text, &typed);
            let ground = theme.tokens.colors.surfaces.base;
            let path = out.join(format!("{name}.png"));
            renderer.render_png(&scene, ground, &path);
            assert!(path.is_file(), "lock fixture missing: {}", path.display());
            // Probe the card's top padding, left of the lock glyph: what the
            // compositor painted there must be the Panel ground.
            let layout = LockLayout::for_output(outputs[0], &theme);
            let probe = (
                layout.panel.x0 + layout.panel.width() / 4.0,
                layout.panel.y0 + 8.0 * layout.scale,
            );
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "probe points are inside a positive output"
            )]
            let (r, g, b, _) = OffscreenRenderer::sample(
                &renderer.render_resilient(&scene, ground),
                w,
                probe.0 as u32,
                probe.1 as u32,
            );
            let e = ui::chrome::shell::panel_ground(&theme).to_rgba8();
            assert!(
                r.abs_diff(e.r) <= 2 && g.abs_diff(e.g) <= 2 && b.abs_diff(e.b) <= 2,
                "{name}: card interior ({r},{g},{b}) is the Panel ground ({},{},{})",
                e.r,
                e.g,
                e.b
            );
            if name == "dark-2x-1280x800" {
                let errored = LockView {
                    error: Some("Incorrect password"),
                    ..view()
                };
                let scene = draw_lock_scene(&outputs, &theme, &mut text, &errored);
                let path = out.join("dark-2x-1280x800-error.png");
                renderer.render_png(&scene, ground, &path);
                assert!(path.is_file(), "lock fixture missing: {}", path.display());
            }
        }
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

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the layout width is a bounded screen coordinate passed to the text renderer's f32 API"
    )]
    #[test]
    fn accessible_lock_layout_reserves_every_text_row() {
        let output = LockOutput {
            origin: (0.0, 0.0),
            size: (1280.0, 720.0),
            scale: 1.0,
        };
        let normal = LockLayout::for_output(output, &Theme::dark());

        for theme in [
            Theme::dark().with_text_scale(1.3),
            Theme::high_contrast()
                .with_ui_scale(1.25)
                .with_text_scale(1.3),
        ] {
            let layout = LockLayout::for_output(output, &theme);
            let mut text = TextRenderer::new();
            for (label, value, size, rect) in [
                ("clock", "21:47", 56.0, layout.clock),
                ("date", "Saturday, September 5", 18.0, layout.date),
                ("title", "Session locked", 20.0, layout.title),
                ("password", "Enter password", 15.0, layout.password),
                (
                    "status",
                    "Incorrect username or password",
                    13.0,
                    layout.status,
                ),
                (
                    "notifications",
                    "12 notifications",
                    13.0,
                    layout.notifications,
                ),
            ] {
                let size_px = scaled_text_size(size, layout.scale, &theme);
                let (_, measured_height) = text.measure(value, size_px, Some(rect.width() as f32));
                assert!(
                    f64::from(measured_height) <= rect.height() + 0.5,
                    "{label} text must fit its own row under {}: measured={measured_height}, row={rect:?}",
                    theme.name,
                );
            }
            assert!(layout.clock.y1 <= layout.date.y0);
            assert!(layout.date.y1 <= layout.panel.y0);
            assert!(layout.icon.y1 <= layout.title.y0);
            assert!(layout.title.y1 <= layout.password.y0);
            assert!(layout.password.y1 <= layout.status.y0);
            assert!(layout.status.y1 <= layout.notifications.y0);
            assert!(
                layout.panel.height() > normal.panel.height(),
                "large text must receive additional panel space"
            );
        }
    }

    #[test]
    fn density_scale_does_not_double_scale_logical_lock_geometry() {
        let theme = Theme::dark();
        let logical = LockOutput {
            origin: (0.0, 0.0),
            size: (1600.0, 900.0),
            scale: 1.0,
        };
        let hidpi = LockOutput {
            scale: 2.0,
            ..logical
        };
        let normal = LockLayout::for_output(logical, &theme);
        let high_density = LockLayout::for_output(hidpi, &theme);
        assert_eq!(
            normal, high_density,
            "output scale is presentation density, not UI zoom"
        );
    }

    #[test]
    fn startup_sized_output_keeps_every_lock_rect_contained() {
        let layout = LockLayout::for_output(
            LockOutput {
                origin: (0.0, 0.0),
                size: (1.0, 1.0),
                scale: 1.0,
            },
            &Theme::dark(),
        );
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

    /// The primary output is the first backend output. That deterministic
    /// policy is also the hot-unplug fallback: once the primary disappears,
    /// the first remaining display receives the one credential surface.
    #[test]
    fn primary_output_is_deterministic_and_falls_back_after_hotplug() {
        let primary = LockOutput {
            origin: (0.0, 0.0),
            size: (1920.0, 1080.0),
            scale: 1.0,
        };
        let secondary = LockOutput {
            origin: (1920.0, 0.0),
            size: (1280.0, 1024.0),
            scale: 1.0,
        };
        assert_eq!(primary_output(&[primary, secondary]), Some(primary));
        assert_eq!(primary_output(&[secondary]), Some(secondary));
        assert_eq!(primary_output(&[]), None);
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

    /// A secondary output must paint a useful indication, but not a second
    /// full credential card. Its scene therefore adds operations, while fewer
    /// than another primary widget would add.
    #[test]
    fn multi_output_paints_quiet_secondary_without_duplicate_credential_card() {
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
        assert!(two_ops > one_ops, "secondary output must be visibly locked");
        // A full primary card has fill/stroke/icon/field/status operations;
        // the secondary status is intentionally much smaller than that.
        assert!(
            two_ops < one_ops * 2,
            "secondary must not mirror a second full password card: {two_ops} vs {one_ops}"
        );
    }
}
