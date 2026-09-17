//! Session power modal: state, layout, paint, animation, and click policy.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::shell::overlay_host::modal_transition_curve;

/// A Panel opens and closes over the motion token's `base` (HIG §9.7). The
/// default token: `open`/`close` have no theme in hand, and the menu is
/// compositor chrome that does not follow per-app motion overrides.
fn panel_open_duration() -> Duration {
    Duration::from_millis(u64::from(ui::tokens::Durations::default().base_ms))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerMenuClick {
    Ignored,
    Consumed,
    /// Locking is handled by the native compositor's `SessionManager`, not
    /// by the external session-action helper.  That keeps the lock scene and
    /// client-input cutoff in one authoritative lifecycle.
    Lock,
    Execute(os_actions::SessionAction),
}

pub(crate) struct PowerMenu {
    open: bool,
    transition: ui::Transition,
    selected: usize,
    /// The user's text scale as of the last draw. Rows hold a label and a
    /// detail line, so their height — and with it the panel's — follows
    /// the type; hit-testing has no theme, so the scale is remembered here
    /// and the same geometry serves paint, clicks and keyboard focus.
    text_scale: f32,
}

impl Default for PowerMenu {
    fn default() -> Self {
        Self {
            open: false,
            transition: ui::Transition::closed(),
            selected: 0,
            text_scale: 1.0,
        }
    }
}

/// The menu's rhythm, resolved from the density and spacing tokens and the
/// user's text scale (HIG §9.6). Unscaled `BASE` tokens: the menu paints in
/// logical pixels like the rest of the compositor chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PowerMenuMetrics {
    /// Panel padding and the gap between the title band and the rows.
    pub pad: f64,
    /// One action row: the Control row height, or taller when the label
    /// plus detail lines at the user's scale need more.
    pub row_h: f64,
    /// Gap between rows.
    pub row_gap: f64,
    /// Top of the first row from the panel's top edge.
    pub rows_top: f64,
    /// Single-line height of the Control label (`scale.sm`).
    label_h: f64,
    /// Single-line height of the Control sub-line (`scale.xs`).
    detail_h: f64,
}

impl PowerMenuMetrics {
    pub(crate) fn for_text_scale(text_scale: f32) -> Self {
        let spacing = ui::tokens::Spacing::BASE;
        let metrics = ui::tokens::Metrics::BASE;
        let type_scale = ui::tokens::TypeScale::default();
        let line = |px: f32| {
            f64::from(ui::text_render::TextStyle::new(px * text_scale).line_height_px())
        };
        let pad = f64::from(spacing.s4);
        let title_h = line(type_scale.lg);
        let label_h = line(type_scale.sm);
        let detail_h = line(type_scale.xs);
        let inner = f64::from(spacing.s2);
        Self {
            pad,
            row_h: f64::from(metrics.panel_row_height).max(inner + label_h + detail_h + inner),
            row_gap: f64::from(spacing.s2),
            rows_top: pad + title_h + pad,
            label_h,
            detail_h,
        }
    }

    /// Height the panel needs to show every action without clipping.
    fn panel_height(self) -> f64 {
        let n = small_count_to_f64(ACTIONS.len());
        self.rows_top + n * self.row_h + (n - 1.0).max(0.0) * self.row_gap + self.pad
    }

    /// The row height that fits every action inside `panel` — the wanted
    /// `row_h`, or less when the viewport capped the panel (3–4× text on a
    /// short output). The rows always tile the panel, so no action can be
    /// painted or hit-tested outside it; a row squeezed below its two lines
    /// drops the detail line at paint time.
    fn row_height_in(self, panel: Rect) -> f64 {
        let n = small_count_to_f64(ACTIONS.len());
        let available = panel.height() - self.rows_top - self.pad - (n - 1.0).max(0.0) * self.row_gap;
        self.row_h.min((available / n).max(0.0))
    }
}

impl PowerMenu {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.selected = 0;
        self.transition
            .set_open(true, panel_open_duration(), ui::Curve::EaseOut);
    }

    pub(crate) fn close(&mut self) -> bool {
        let was_open = self.open;
        self.open = false;
        if was_open {
            self.transition
                .set_open(false, panel_open_duration(), ui::Curve::EaseOut);
        }
        was_open
    }

    pub(crate) fn is_animating(&self, now: Instant) -> bool {
        self.transition.is_animating(now)
    }

    /// Pin the production modal at its resting state for deterministic visual
    /// review. This is deliberately test-only state control: opening and
    /// arming still go through [`Self::open`] and [`Self::key`].
    pub(crate) fn force_settled_for_visual_test(&mut self) {
        self.transition
            .set_open(self.open, Duration::ZERO, ui::Curve::Linear);
    }

    pub(crate) fn click(&mut self, viewport: Rect, point: Point) -> PowerMenuClick {
        if !self.open {
            return PowerMenuClick::Ignored;
        }
        if !self.panel_rect(viewport).contains(point) {
            self.close();
            return PowerMenuClick::Consumed;
        }
        let Some(index) =
            (0..ACTIONS.len()).find(|&index| self.action_rect(viewport, index).contains(point))
        else {
            return PowerMenuClick::Consumed;
        };
        self.selected = index;
        self.activate(index)
    }

    pub(crate) fn metrics(&self) -> PowerMenuMetrics {
        PowerMenuMetrics::for_text_scale(self.text_scale)
    }

    /// The flyout, anchored under the top-right status area it was opened
    /// from, at the list-Panel width (HIG §9.6) and as tall as its rows need
    /// at the current text scale — capped to the viewport's bottom inset.
    pub(crate) fn panel_rect(&self, viewport: Rect) -> Rect {
        panel_rect_with(viewport, self.metrics())
    }

    /// Row `index`, inset by the panel padding, on the metrics' pitch.
    pub(crate) fn action_rect(&self, viewport: Rect, index: usize) -> Rect {
        action_rect_with(viewport, index, self.metrics())
    }

    pub(crate) fn key(&mut self, key: &winit::keyboard::Key, shift: bool) -> PowerMenuClick {
        use winit::keyboard::{Key, NamedKey};
        if !self.open {
            return PowerMenuClick::Ignored;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                self.selected = self
                    .selected
                    .checked_sub(1)
                    .unwrap_or(ACTIONS.len().saturating_sub(1));
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                self.selected = if self.selected.saturating_add(1) >= ACTIONS.len() {
                    0
                } else {
                    self.selected.saturating_add(1)
                };
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::Tab) => {
                if shift {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or(ACTIONS.len().saturating_sub(1));
                } else {
                    self.selected = if self.selected.saturating_add(1) >= ACTIONS.len() {
                        0
                    } else {
                        self.selected.saturating_add(1)
                    };
                }
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate(self.selected),
            _ => PowerMenuClick::Consumed,
        }
    }

    fn activate(&mut self, index: usize) -> PowerMenuClick {
        let Some(action) = ACTIONS.get(index) else {
            return PowerMenuClick::Consumed;
        };
        self.close();
        match action.0 {
            PowerMenuAction::Lock => PowerMenuClick::Lock,
            PowerMenuAction::External(action) => PowerMenuClick::Execute(action),
        }
    }

    pub(crate) fn draw(
        &mut self,
        viewport: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if theme.reduce_motion {
            self.transition
                .set_open(self.open, Duration::ZERO, ui::Curve::Linear);
        }
        // Geometry follows the type: remember the scale so the click path,
        // which has no theme, hit-tests the rows exactly where they paint.
        self.text_scale = theme.text_scale;
        let now = Instant::now();
        if !self.transition.is_visible(now) {
            return;
        }
        let (alpha, dy, scale) = modal_transition_curve(&self.transition, now, 0.94);
        let panel = self.panel_rect(viewport);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &viewport,
        );
        let centre = panel.center();
        // `Affine`'s `Mul` composes transform matrices with plain float
        // multiply-adds — it cannot overflow or panic — but kurbo has no
        // non-operator way to chain a translate/scale/translate around an
        // arbitrary center point.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine * Affine is a panic-free float matrix multiply; no combinator \
                      exists to compose this translate/scale/translate chain"
        )]
        let modal_transform = transform
            * Affine::translate((centre.x, centre.y + dy))
            * Affine::scale(scale)
            * Affine::translate((-centre.x, -centre.y));
        // Theme type tokens already carry the user's text scale, and the
        // panel paints with those resolved sizes; the renderer must not
        // scale them again (HIG §7a) — reset for this pass, as the shell's
        // immediate-mode chrome passes do, and restored afterwards.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_panel(scene, modal_transform, text_renderer, theme, viewport);
        text_renderer.set_text_scale(renderer_scale);
        scene.pop_layer();
    }

    fn draw_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        let panel = self.panel_rect(viewport);
        let m = self.metrics();
        let colors = &theme.tokens.colors;
        // This is a compact flyout rooted in the status area, not a modal
        // task. Its bounds are the click-dismiss surface; the rest of the
        // desktop stays visually present so the power button and result read
        // as one continuous interaction.
        // Panel role (HIG §9.1/§9.2): the flyout is the same material as
        // every other Panel — `panel_ground` at the Panel radius, with the
        // derived Panel border — rather than a ramp slot and a literal 24.
        let panel_radius = ui::chrome::ChromeRole::Panel.radius(theme, panel.height());
        let card = RoundedRect::from_rect(panel, panel_radius);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );
        // Theme-resolved sizes, painted once: `draw` has reset the renderer
        // for this pass.
        let title_size = theme.tokens.typography.scale.lg;
        let label_size = theme.tokens.typography.scale.sm;
        let detail_size = theme.tokens.typography.scale.xs;
        // Panel title on the padding line (HIG §9.6), centred on the band
        // the metrics reserved for it.
        let (_, title_h) =
            text_renderer.measure_weighted("Session", title_size, None, TextWeight::Semibold);
        let title_band_h = m.rows_top - 2.0 * m.pad;
        text_renderer.draw_text_weighted(
            scene,
            "Session",
            title_size,
            colors.text_primary,
            at(
                transform,
                (
                    panel.x0 + m.pad,
                    panel.y0 + m.pad + (title_band_h - f64::from(title_h)) / 2.0,
                ),
            ),
            None,
            TextWeight::Semibold,
        );
        let inset = f64::from(theme.tokens.spacing.s3);
        let icon_px = f64::from(theme.tokens.metrics.icon_md);
        for (index, (_, label, detail, icon)) in ACTIONS.iter().enumerate() {
            let row = self.action_rect(viewport, index);
            let selected = self.selected == index;
            // Control role: the chosen row is a lift (HIG §9.3), never an
            // accent wash; idle rows sit on a quiet lift so the list reads
            // as rows rather than as bare text on the panel.
            let shape = RoundedRect::from_rect(
                row,
                ui::chrome::ChromeRole::Control.radius(theme, row.height()),
            );
            let tier = ui::chrome::selection_tier(
                theme,
                ui::chrome::SelectionState::active(selected),
            );
            let fill = if selected { tier.fill } else { colors.lift(0.35) };
            scene.fill(Fill::NonZero, transform, fill, None, &shape);
            if let Some(border) = tier.border {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    border,
                    None,
                    &shape,
                );
            }
            // The standard glyph on the row's inset, centred on the row.
            let icon_rect = Rect::new(
                row.x0 + inset,
                row.center().y - icon_px / 2.0,
                row.x0 + inset + icon_px,
                row.center().y + icon_px / 2.0,
            );
            ui::icons::draw_icon(
                scene,
                transform,
                icon_rect,
                *icon,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            // Label over detail as one stack centred in the row, each line
            // measured and elided to the room left of the glyph; the row
            // itself was sized for these two lines, so at 200 % the stack
            // grows the row rather than painting through its neighbour.
            let text_x = icon_rect.x1 + inset;
            let avail = (row.x1 - inset - text_x).max(1.0);
            let label = text_renderer.elide_to_width_weighted(
                label,
                label_size,
                text_width(avail),
                TextWeight::Semibold,
            );
            let detail = text_renderer.elide_to_width(detail, detail_size, text_width(avail));
            let (_, label_h) =
                text_renderer.measure_weighted(&label, label_size, None, TextWeight::Semibold);
            let (_, detail_h) = text_renderer.measure(&detail, detail_size, None);
            // A row the viewport squeezed below its two lines keeps the
            // label and drops the detail, rather than painting the detail
            // through the next row.
            let show_detail = f64::from(label_h) + f64::from(detail_h) <= row.height();
            let stack_h = f64::from(label_h) + if show_detail { f64::from(detail_h) } else { 0.0 };
            let label_y = row.y0 + ((row.height() - stack_h) / 2.0).max(0.0);
            text_renderer.draw_text_weighted(
                scene,
                &label,
                label_size,
                colors.text_primary,
                at(transform, (text_x, label_y)),
                None,
                TextWeight::Semibold,
            );
            if show_detail {
                text_renderer.draw_text(
                    scene,
                    &detail,
                    detail_size,
                    colors.text_dim,
                    at(transform, (text_x, label_y + f64::from(label_h))),
                    None,
                );
            }
        }
    }
}

/// Width for the text shaper; panel widths are far inside `f32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a chrome panel is a few hundred logical pixels wide; f32 is exact enough for text wrapping"
)]
fn text_width(width: f64) -> f32 {
    width as f32
}

#[derive(Clone, Copy)]
enum PowerMenuAction {
    Lock,
    External(os_actions::SessionAction),
}

const ACTIONS: [(PowerMenuAction, &str, &str, ui::icons::Icon); 6] = [
    (
        PowerMenuAction::Lock,
        "Lock",
        "Keep apps running",
        ui::icons::Icon::Shield,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::Logout),
        "Log out",
        "End this session",
        ui::icons::Icon::ArrowRight,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::SwitchUser),
        "Switch user",
        "Keep this session running",
        ui::icons::Icon::Users,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::Suspend),
        "Suspend",
        "Resume where you left off",
        ui::icons::Icon::Sun,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::Restart),
        "Restart",
        "Restart the computer",
        ui::icons::Icon::RotateCw,
    ),
    (
        PowerMenuAction::External(os_actions::SessionAction::ShutDown),
        "Shut down",
        "Turn off the computer",
        ui::icons::Icon::Power,
    ),
];

/// Edge inset of the flyout from the viewport: `spacing.s6` at 1×.
const EDGE_INSET: f64 = ui::tokens::Spacing::BASE.s6 as f64;
/// Top of the flyout: under the 40 px bar with a `s4 + s1` gap, so the
/// panel hangs off the status area it was opened from (60 at 1×, the
/// anchor `tests::menu_is_anchored_below_the_top_right_status_area` pins).
const TOP_INSET: f64 = ui::tokens::Metrics::BASE.bar_height as f64
    + ui::tokens::Spacing::BASE.s4 as f64
    + ui::tokens::Spacing::BASE.s1 as f64;

fn panel_rect_with(viewport: Rect, m: PowerMenuMetrics) -> Rect {
    // A list Panel (HIG §9.6): six two-line rows want the roster width,
    // not a bespoke 420. Narrow displays shrink it inside the edge insets.
    let width = f64::from(ui::tokens::Metrics::BASE.panel_width_list)
        .min(viewport.width() - 2.0 * EDGE_INSET);
    let height = m.panel_height().min(viewport.height() - 2.0 * EDGE_INSET);
    // This menu is launched from the top-right system status area. Keep the
    // actions beside that origin instead of making the pointer cross the
    // whole display to a modal in the centre. The same edge inset is used
    // on narrow displays, where the sheet naturally becomes nearly
    // full-width without losing its anchor.
    let right = viewport.x1 - EDGE_INSET;
    let top = viewport.y0 + TOP_INSET;
    Rect::new(
        right - width,
        top,
        right,
        (top + height).min(viewport.y1 - EDGE_INSET),
    )
}

fn action_rect_with(viewport: Rect, index: usize, m: PowerMenuMetrics) -> Rect {
    let panel = panel_rect_with(viewport, m);
    let row_h = m.row_height_in(panel);
    let y0 = panel.y0 + m.rows_top + small_count_to_f64(index) * (row_h + m.row_gap);
    Rect::new(panel.x0 + m.pad, y0, panel.x1 - m.pad, y0 + row_h)
}

/// Widens a row index to `f64` for draw-time row-position math. Every
/// call site is a row index into the fixed 5-entry [`ACTIONS`] table, so
/// the integer is exactly representable.
fn small_count_to_f64(n: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded well under 2^53 — a row index into a 5-action menu"
    )]
    {
        n as f64
    }
}

/// `transform * Affine::translate(xy)`, used at every glyph draw site
/// below. `Affine::mul` is plain matrix composition with no side effects,
/// but it is a custom `Mul` impl rather than primitive integer arithmetic,
/// which is what trips the lint at each call site.
fn at(transform: Affine, xy: (f64, f64)) -> Affine {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine::mul is pure matrix composition (translate here), not primitive integer arithmetic that can silently wrap"
    )]
    {
        transform * Affine::translate(xy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    #[test]
    fn lock_switch_user_and_suspend_execute_on_first_click() {
        let mut menu = PowerMenu::default();
        for (index, expected) in [
            (0, PowerMenuClick::Lock),
            (
                2,
                PowerMenuClick::Execute(os_actions::SessionAction::SwitchUser),
            ),
            (
                3,
                PowerMenuClick::Execute(os_actions::SessionAction::Suspend),
            ),
        ] {
            menu.open();
            let point = menu.action_rect(VIEWPORT, index).center();
            assert_eq!(menu.click(VIEWPORT, point), expected);
            assert!(!menu.open);
        }
    }

    #[test]
    fn every_visible_action_executes_on_first_click() {
        for (index, expected) in [
            (1, os_actions::SessionAction::Logout),
            (4, os_actions::SessionAction::Restart),
            (5, os_actions::SessionAction::ShutDown),
        ] {
            let mut menu = PowerMenu::default();
            menu.open();
            let point = menu.action_rect(VIEWPORT, index).center();
            assert_eq!(
                menu.click(VIEWPORT, point),
                PowerMenuClick::Execute(expected)
            );
            assert!(!menu.open);
        }
    }

    #[test]
    fn outside_click_closes_without_an_action() {
        let mut menu = PowerMenu::default();
        menu.open();
        assert_eq!(menu.click(VIEWPORT, Point::ZERO), PowerMenuClick::Consumed);
        assert!(!menu.open);
    }

    #[test]
    fn menu_is_anchored_below_the_top_right_status_area() {
        let menu = PowerMenu::default();
        for viewport in [VIEWPORT, Rect::new(0.0, 0.0, 960.0, 540.0)] {
            let panel = menu.panel_rect(viewport);
            assert_eq!(panel.x1, viewport.x1 - 24.0);
            assert_eq!(panel.y0, viewport.y0 + 60.0);
            assert!(panel.y1 <= viewport.y1 - 24.0);
            for index in 0..ACTIONS.len() {
                assert!(
                    panel.contains(menu.action_rect(viewport, index).center()),
                    "action {index} must remain inside the anchored menu at {viewport:?}"
                );
            }
        }
    }

    #[test]
    fn keyboard_navigation_reaches_actions_and_activates_once() {
        use winit::keyboard::{Key, NamedKey};

        let mut menu = PowerMenu::default();
        menu.open();
        assert_eq!(
            menu.key(&Key::Named(NamedKey::ArrowDown), false),
            PowerMenuClick::Consumed
        );
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Enter), false),
            PowerMenuClick::Execute(os_actions::SessionAction::Logout)
        );

        menu.open();
        menu.selected = 5;
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Space), false),
            PowerMenuClick::Execute(os_actions::SessionAction::ShutDown)
        );
    }

    #[test]
    fn escape_closes_the_keyboard_modal_without_an_action() {
        use winit::keyboard::{Key, NamedKey};

        let mut menu = PowerMenu::default();
        menu.open();
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Escape), false),
            PowerMenuClick::Consumed
        );
        assert!(!menu.open);
    }

    #[test]
    fn opening_requests_animation_frames_without_waiting_for_draw() {
        let mut menu = PowerMenu::default();
        menu.open();
        assert!(menu.is_animating(Instant::now()));
    }

    /// HIG §9.6: a list Panel at the roster width, Control rows on the
    /// `s2` pitch inside `s4` padding — read from the tokens, not restated.
    #[test]
    fn rows_are_control_rows_on_the_token_pitch_at_default_text() {
        let m = PowerMenuMetrics::for_text_scale(1.0);
        assert_eq!(m.row_h, f64::from(ui::tokens::Metrics::BASE.panel_row_height));
        assert_eq!(m.row_gap, f64::from(ui::tokens::Spacing::BASE.s2));
        assert_eq!(m.pad, f64::from(ui::tokens::Spacing::BASE.s4));
        let menu = PowerMenu::default();
        let panel = menu.panel_rect(VIEWPORT);
        assert_eq!(
            panel.width(),
            f64::from(ui::tokens::Metrics::BASE.panel_width_list)
        );
        let first = menu.action_rect(VIEWPORT, 0);
        let last = menu.action_rect(VIEWPORT, ACTIONS.len() - 1);
        assert_eq!(first.height(), m.row_h, "an uncapped panel keeps the Control row");
        assert_eq!(first.x0 - panel.x0, m.pad);
        assert_eq!(panel.x1 - first.x1, m.pad);
        assert!(
            (panel.y1 - last.y1 - m.pad).abs() < 1e-9,
            "the last row sits on the panel padding: {panel:?} / {last:?}"
        );
        assert_eq!(menu.action_rect(VIEWPORT, 1).y0 - first.y1, m.row_gap);
    }

    /// When the viewport caps the panel (3–4× text on a short output) the
    /// rows shrink to tile the capped panel rather than being laid on the
    /// uncapped pitch past its bottom edge, where they could be painted but
    /// never pressed — and none drops under the minimum pointer target.
    #[test]
    fn a_capped_panel_still_tiles_every_row_inside_it() {
        for (viewport, scale) in [
            (Rect::new(0.0, 0.0, 1280.0, 720.0), 3.0_f32),
            (VIEWPORT, 4.0),
        ] {
            let m = PowerMenuMetrics::for_text_scale(scale);
            let panel = panel_rect_with(viewport, m);
            assert!(
                panel.height() < m.panel_height(),
                "the test needs a capped panel at {scale}× on {viewport:?}"
            );
            let target = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
            for index in 0..ACTIONS.len() {
                let row = action_rect_with(viewport, index, m);
                assert!(
                    row.y1 <= panel.y1 - m.pad + 1e-9,
                    "row {index} stays inside the capped panel: {row:?} in {panel:?}"
                );
                assert!(row.height() >= target, "row {index} keeps a pointer target");
                if let Some(next) = index.checked_add(1).filter(|n| *n < ACTIONS.len()) {
                    assert!(action_rect_with(viewport, next, m).y0 >= row.y1);
                }
            }
        }
    }

    /// At 200 % a label and a detail line no longer fit a 44 px row; the
    /// row grows to hold both and the panel grows to hold every row, so
    /// nothing paints through its neighbour.
    #[test]
    fn large_text_grows_the_rows_and_the_panel_holds_them() {
        let one = PowerMenuMetrics::for_text_scale(1.0);
        let two = PowerMenuMetrics::for_text_scale(2.0);
        let inner = f64::from(ui::tokens::Spacing::BASE.s2);
        assert!(two.row_h >= two.label_h + two.detail_h + 2.0 * inner);
        assert!(two.row_h > one.row_h);
        for viewport in [VIEWPORT, Rect::new(0.0, 0.0, 800.0, 700.0)] {
            let panel = panel_rect_with(viewport, two);
            for index in 0..ACTIONS.len() {
                let row = action_rect_with(viewport, index, two);
                assert!(
                    row.y1 <= panel.y1 - two.pad + 1e-9,
                    "row {index} must stay inside the panel at {viewport:?}"
                );
                if let Some(next) = index.checked_add(1).filter(|n| *n < ACTIONS.len()) {
                    assert!(action_rect_with(viewport, next, two).y0 >= row.y1);
                }
            }
        }
    }

    /// The click path has no theme: it must hit-test the rows where the
    /// last frame painted them, so a 200 % session activates the row under
    /// the pointer rather than the one the 1× geometry would put there.
    #[test]
    fn clicks_hit_the_rows_where_a_large_text_frame_painted_them() {
        let mut menu = PowerMenu::default();
        menu.open();
        menu.force_settled_for_visual_test();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let mut scene = Scene::new();
        menu.draw(VIEWPORT, &mut scene, Affine::IDENTITY, &mut tr, &theme);
        let painted = action_rect_with(VIEWPORT, 3, PowerMenuMetrics::for_text_scale(2.0));
        assert!(
            !PowerMenu::default()
                .action_rect(VIEWPORT, 3)
                .contains(painted.center()),
            "the test is only meaningful if 1× and 2× geometry disagree"
        );
        assert_eq!(
            menu.click(VIEWPORT, painted.center()),
            PowerMenuClick::Execute(os_actions::SessionAction::Suspend)
        );
    }

    /// Review fixtures: the ux-bench cannot open the power menu on macOS,
    /// so it is photographed from its paint path at 1× and 200 % text, on a
    /// laptop viewport, and on the light and high-contrast palettes. The
    /// rendered pixels are asserted, not just the file: the title band is
    /// the Panel ground and an idle row's interior is its lift floor.
    #[test]
    fn power_menu_review_fixtures_are_written() {
        use crate::shell::osd::review_offscreen::OffscreenRenderer;
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/ui-review/s2-r2/power-menu");
        // One wgpu device per viewport size, shared by every variant of it.
        let mut desktop = OffscreenRenderer::new(1280, 800);
        let mut laptop = OffscreenRenderer::new(800, 700);
        let variants = [
            ("dark-1x-1280x800", Theme::dark(), 1.0_f32, (1280_u32, 800_u32)),
            (
                "dark-2x-1280x800",
                Theme::dark().with_text_scale(2.0),
                2.0,
                (1280, 800),
            ),
            (
                "dark-2x-800x700",
                Theme::dark().with_text_scale(2.0),
                2.0,
                (800, 700),
            ),
            ("light-1x-1280x800", Theme::light(), 1.0, (1280, 800)),
            (
                "high-contrast-1x-1280x800",
                Theme::high_contrast(),
                1.0,
                (1280, 800),
            ),
        ];
        for (name, theme, scale, (w, h)) in variants {
            let viewport = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
            let mut menu = PowerMenu::default();
            menu.open();
            menu.selected = 2;
            menu.force_settled_for_visual_test();
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let mut scene = Scene::new();
            menu.draw(viewport, &mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(tr.text_scale(), scale, "the pass restores the renderer's scale");
            let renderer = if w == 1280 { &mut desktop } else { &mut laptop };
            let ground = theme.tokens.colors.surfaces.base;
            let path = out.join(format!("{name}.png"));
            renderer.render_png(&scene, ground, &path);
            assert!(path.is_file(), "power menu fixture missing: {}", path.display());
            let rgba = renderer.render_resilient(&scene, ground);
            let panel = menu.panel_rect(viewport);
            let m = menu.metrics();
            let probes = [
                (
                    Point::new(panel.center().x, panel.y0 + m.pad / 2.0),
                    ui::chrome::shell::panel_ground(&theme),
                    "title band is the Panel ground",
                ),
                (
                    {
                        let row = menu.action_rect(viewport, 0);
                        Point::new(row.x1 - m.pad, row.center().y)
                    },
                    theme.tokens.colors.lift(0.35),
                    "an idle row's interior is its lift floor",
                ),
            ];
            for (at, expected, what) in probes {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "probe points are inside a positive viewport"
                )]
                let (r, g, b, _) = OffscreenRenderer::sample(&rgba, w, at.x as u32, at.y as u32);
                let e = expected.to_rgba8();
                assert!(
                    r.abs_diff(e.r) <= 2 && g.abs_diff(e.g) <= 2 && b.abs_diff(e.b) <= 2,
                    "{name}: {what}: got ({r},{g},{b}) expected ({},{},{})",
                    e.r,
                    e.g,
                    e.b
                );
            }
        }
    }
}
