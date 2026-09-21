//! Session power modal: state, layout, paint, animation, and click policy.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::shell::overlay_host::{MODAL_OPEN_ANIM_MS, modal_transition_curve};

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
}

impl Default for PowerMenu {
    fn default() -> Self {
        Self {
            open: false,
            transition: ui::Transition::closed(),
            selected: 0,
        }
    }
}

impl PowerMenu {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Semantic hit targets for the visible session actions.  The menu is
    /// painted directly by the overlay host rather than as child widgets, so
    /// external interaction/accessibility tests need the same geometry the
    /// click handler uses instead of guessing screen coordinates.
    pub(crate) fn action_rects(&self, viewport: Rect) -> Vec<(&'static str, Rect)> {
        if !self.open {
            return Vec::new();
        }
        ACTIONS
            .iter()
            .enumerate()
            .map(|(index, (action, _, _, _))| {
                (action.semantic_name(), action_rect(viewport, index))
            })
            .collect()
    }

    pub(crate) fn accessibility_snapshot(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.open.then(|| {
            AccessNode::new(Role::Dialog)
                .with_name("Session")
                .with_description(
                    "Lock, leave, switch user, suspend, restart, or shut down this computer",
                )
                .with_state(AccessState::new().modal(true))
                .with_children(
                    ACTIONS
                        .iter()
                        .map(|(_, label, detail, _)| {
                            AccessNode::new(Role::Button)
                                .with_name(*label)
                                .with_description(*detail)
                                .with_state(AccessState::new().focusable(true))
                        })
                        .collect(),
                )
        })
    }

    pub(crate) fn accessibility_rect_for_path(
        &self,
        viewport: Rect,
        path: &[usize],
    ) -> Option<Rect> {
        if !self.open {
            return None;
        }
        match path {
            [] => Some(panel_rect(viewport)),
            [index] if *index < ACTIONS.len() => Some(action_rect(viewport, *index)),
            _ => None,
        }
    }

    pub(crate) fn focus_accessibility_path(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if !self.open || *index >= ACTIONS.len() {
            return false;
        }
        self.selected = *index;
        true
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.selected = 0;
        self.transition.set_open(
            true,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
    }

    pub(crate) fn close(&mut self) -> bool {
        let was_open = self.open;
        self.open = false;
        if was_open {
            self.transition.set_open(
                false,
                Duration::from_millis(MODAL_OPEN_ANIM_MS),
                ui::Curve::EaseOut,
            );
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
        let stable_point = point;
        let painted_point = self.untransform_pointer(viewport, point, Instant::now());
        if !panel_rect(viewport).contains(painted_point)
            && !panel_rect(viewport).contains(stable_point)
        {
            self.close();
            return PowerMenuClick::Consumed;
        }
        // Prefer the inverse-painted coordinate while the flyout animates so
        // adjacent actions cannot be confused. The stable rectangles remain
        // valid semantic hit targets too: `action_rects` publishes them to
        // accessibility and automation before the transition settles.
        let Some(index) = (0..ACTIONS.len())
            .find(|&index| action_rect(viewport, index).contains(painted_point))
            .or_else(|| {
                (0..ACTIONS.len())
                    .find(|&index| action_rect(viewport, index).contains(stable_point))
            })
        else {
            return PowerMenuClick::Consumed;
        };
        self.selected = index;
        self.activate(index)
    }

    /// Convert a pointer in painted viewport coordinates back into the
    /// flyout's stable layout coordinates. During the 220ms open/close scale
    /// animation the rows are painted through a centre transform; hit-testing
    /// the untransformed rectangles made a visibly correct click activate an
    /// adjacent action (most dangerously Lock instead of Log out).
    fn untransform_pointer(&self, viewport: Rect, point: Point, now: Instant) -> Point {
        let (_, dy, scale) = modal_transition_curve(&self.transition, now, 0.94);
        if !scale.is_finite() || scale <= f64::EPSILON {
            return point;
        }
        let centre = panel_rect(viewport).center();
        Point::new(
            centre.x + (point.x - centre.x) / scale,
            centre.y + (point.y - centre.y - dy) / scale,
        )
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
        let now = Instant::now();
        if !self.transition.is_visible(now) {
            return;
        }
        let (alpha, dy, scale) = modal_transition_curve(&self.transition, now, 0.94);
        let panel = panel_rect(viewport);
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
        self.draw_panel(scene, modal_transform, text_renderer, theme, viewport);
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
        let panel = panel_rect(viewport);
        let colors = &theme.tokens.colors;
        // This is a compact flyout rooted in the status area, not a modal
        // task. Its bounds are the click-dismiss surface; the rest of the
        // desktop stays visually present so the power button and result read
        // as one continuous interaction. The panel itself must nevertheless
        // be opaque: this renderer has no backdrop blur, so a translucent
        // dark fill turns bright terminal text behind it into illegible noise.
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, panel.height());
        ui::chrome::shell::panel_shadow(scene, transform, panel, radius, theme, 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &RoundedRect::from_rect(panel, radius),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &RoundedRect::from_rect(panel, radius),
        );
        text_renderer.draw_text_weighted(
            scene,
            "Session",
            theme.tokens.typography.scale.lg,
            colors.text_primary,
            at(transform, (panel.x0 + 16.0, panel.y0 + 18.0)),
            None,
            TextWeight::Semibold,
        );
        for (index, (_, label, detail, icon)) in ACTIONS.iter().enumerate() {
            let row = action_rect(viewport, index);
            let selected = self.selected == index;
            scene.fill(
                Fill::NonZero,
                transform,
                if selected {
                    colors.accent[5].with_alpha_factor(0.16)
                } else {
                    colors.neutral[2]
                },
                None,
                &RoundedRect::from_rect(row, 12.0),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    row.x0 + 12.0,
                    row.y0 + (row.height() - 18.0) * 0.5,
                    row.x0 + 30.0,
                    row.y0 + (row.height() - 18.0) * 0.5 + 18.0,
                ),
                *icon,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                at(transform, (row.x0 + 42.0, row.y0 + 8.0)),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                detail,
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                at(transform, (row.x0 + 42.0, row.y0 + 27.0)),
                None,
            );
        }
    }
}

#[derive(Clone, Copy)]
enum PowerMenuAction {
    Lock,
    External(os_actions::SessionAction),
}

impl PowerMenuAction {
    const fn semantic_name(self) -> &'static str {
        match self {
            Self::Lock | Self::External(os_actions::SessionAction::Lock) => "power_lock",
            Self::External(os_actions::SessionAction::Logout) => "power_logout",
            Self::External(os_actions::SessionAction::SwitchUser) => "power_switch_user",
            Self::External(os_actions::SessionAction::Suspend) => "power_suspend",
            Self::External(os_actions::SessionAction::Restart) => "power_restart",
            Self::External(os_actions::SessionAction::ShutDown) => "power_shut_down",
        }
    }
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

fn panel_rect(viewport: Rect) -> Rect {
    // This is a system-menu flyout, not a confirmation dialog. It shares the
    // Quick Settings top-right anchor so choosing Power does not send the
    // pointer across the display or open an oversized detached sheet.
    // Two columns keep the six session actions scannable on the 800px and
    // 1280px review surfaces. Narrow windows fall back to the original single
    // column while retaining the same semantic hit targets.
    let columns = action_columns(viewport);
    let preferred_width: f64 = if columns == 2 { 424.0 } else { 336.0 };
    let width = preferred_width.min((viewport.width() - 16.0).max(1.0));
    let height = menu_height(viewport, columns).min((viewport.height() - 64.0).max(1.0));
    // Leave only a 4px edge inset: the underlying app's scrollbar otherwise
    // peeks through a 16px gap beside this opaque flyout and reads as a second
    // power-menu affordance in screenshots.
    let right = viewport.x1 - 4.0;
    let top = viewport.y0 + 48.0;
    Rect::new(
        right - width,
        top,
        right,
        (top + height).min(viewport.y1 - 8.0),
    )
}

fn action_rect(viewport: Rect, index: usize) -> Rect {
    let panel = panel_rect(viewport);
    let columns = action_columns(viewport);
    let gap = action_gap(columns);
    let width = (panel.width() - 24.0 - small_count_to_f64(columns.saturating_sub(1)) * gap)
        / small_count_to_f64(columns);
    let column = index % columns;
    let row = index / columns;
    let x0 = panel.x0 + 12.0 + small_count_to_f64(column) * (width + gap);
    let row_height = action_height(viewport, columns);
    let y0 = panel.y0 + 52.0 + small_count_to_f64(row) * (row_height + gap);
    Rect::new(x0, y0, x0 + width, y0 + row_height)
}

fn action_columns(viewport: Rect) -> usize {
    // At normal heights narrow windows retain the roomy single-column list.
    // A short viewport needs the compact two-column arrangement so all six
    // actions remain visible and clickable instead of being clipped below
    // the panel's clamped bottom edge.
    usize::from(
        viewport.width() >= 640.0 || (viewport.width() >= 420.0 && viewport.height() < 400.0),
    ) + 1
}

fn action_height(viewport: Rect, columns: usize) -> f64 {
    if columns == 2 && viewport.height() < 400.0 {
        44.0
    } else if columns == 2 {
        56.0
    } else {
        44.0
    }
}

fn action_gap(columns: usize) -> f64 {
    if columns == 2 { 8.0 } else { 4.0 }
}

fn menu_height(viewport: Rect, columns: usize) -> f64 {
    let rows = ACTIONS.len().div_ceil(columns);
    52.0 + small_count_to_f64(rows) * action_height(viewport, columns)
        + small_count_to_f64(rows.saturating_sub(1)) * action_gap(columns)
        + 12.0
}

/// Widens a row index to `f64` for draw-time row-position math. Every
/// call site is a row index into the fixed [`ACTIONS`] table, so
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
    use ui::ColorCompat;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    #[test]
    fn open_menu_exports_the_same_six_semantic_rows_it_paints() {
        let mut menu = PowerMenu::default();
        assert!(menu.action_rects(VIEWPORT).is_empty());
        assert!(menu.accessibility_snapshot().is_none());
        menu.open();
        let targets = menu.action_rects(VIEWPORT);
        assert_eq!(
            targets.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            [
                "power_lock",
                "power_logout",
                "power_switch_user",
                "power_suspend",
                "power_restart",
                "power_shut_down",
            ]
        );
        assert!(targets.iter().all(|(_, rect)| rect.area() > 0.0));
        assert_eq!(
            menu.accessibility_snapshot()
                .expect("open session menu")
                .children
                .len(),
            ACTIONS.len()
        );
    }
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
            assert_eq!(
                menu.click(VIEWPORT, action_rect(VIEWPORT, index).center()),
                expected
            );
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
            let point = action_rect(VIEWPORT, index).center();
            assert_eq!(
                menu.click(VIEWPORT, point),
                PowerMenuClick::Execute(expected)
            );
            assert!(!menu.open);
        }
    }

    #[test]
    fn opening_animation_hit_test_tracks_the_painted_logout_row() {
        let mut menu = PowerMenu::default();
        menu.open();
        let now = Instant::now();
        let panel = panel_rect(VIEWPORT);
        let stable = action_rect(VIEWPORT, 1).center();
        let (_, dy, scale) = modal_transition_curve(&menu.transition, now, 0.94);
        let centre = panel.center();
        let painted = Point::new(
            centre.x + (stable.x - centre.x) * scale,
            centre.y + dy + (stable.y - centre.y) * scale,
        );
        let logical = menu.untransform_pointer(VIEWPORT, painted, now);
        assert!((logical.x - stable.x).abs() < 0.001);
        assert!((logical.y - stable.y).abs() < 0.001);
        // The production click samples a fresh instant. Freeze the transition
        // at this same intermediate value so the regression remains exact.
        assert_eq!(
            (0..ACTIONS.len()).find(|&index| action_rect(VIEWPORT, index).contains(logical)),
            Some(1)
        );
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
        for viewport in [VIEWPORT, Rect::new(0.0, 0.0, 960.0, 540.0)] {
            let panel = panel_rect(viewport);
            assert_eq!(panel.x1, viewport.x1 - 4.0);
            assert_eq!(panel.y0, viewport.y0 + 48.0);
            assert!(panel.y1 <= viewport.y1 - 8.0);
            for index in 0..ACTIONS.len() {
                assert!(
                    panel.contains(action_rect(viewport, index).center()),
                    "action {index} must remain inside the anchored menu at {viewport:?}"
                );
            }
        }
    }

    #[test]
    fn compact_flyout_is_materially_smaller_than_the_old_modal_sheet() {
        let panel = panel_rect(Rect::new(0.0, 0.0, 960.0, 540.0));
        assert_eq!(panel.width(), 424.0);
        assert_eq!(panel.height(), 248.0);
        assert!(panel.area() < 420.0 * 446.0 * 0.65);
    }

    #[test]
    fn two_column_layout_fits_both_visual_audit_viewports() {
        for viewport in [
            Rect::new(0.0, 0.0, 800.0, 600.0),
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        ] {
            let panel = panel_rect(viewport);
            assert_eq!(action_columns(viewport), 2);
            assert_eq!(panel.height(), 248.0);
            assert!(panel.x1 >= viewport.x1 - 4.0);
            let rows = (0..ACTIONS.len())
                .map(|index| action_rect(viewport, index))
                .collect::<Vec<_>>();
            assert!(rows.iter().all(|row| panel.contains(row.center())));
            assert!(rows.iter().all(|row| row.width() >= 180.0));
            for (index, row) in rows.iter().enumerate() {
                for earlier in &rows[..index] {
                    let overlap = row.intersect(*earlier);
                    assert!(
                        overlap.width() <= 0.0 || overlap.height() <= 0.0,
                        "action {index} overlaps an earlier action at {viewport:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn narrow_layout_falls_back_to_one_column_without_clipping_actions() {
        let viewport = Rect::new(0.0, 0.0, 480.0, 600.0);
        let panel = panel_rect(viewport);
        assert_eq!(action_columns(viewport), 1);
        assert_eq!(panel.height(), 348.0);
        for index in 0..ACTIONS.len() {
            let row = action_rect(viewport, index);
            assert!(panel.contains(row.center()));
            assert!(row.width() >= 44.0 && row.height() >= 44.0);
        }
    }

    #[test]
    fn tiny_viewport_uses_compact_two_columns_for_all_actions() {
        let viewport = Rect::new(0.0, 0.0, 480.0, 300.0);
        let panel = panel_rect(viewport);
        assert_eq!(action_columns(viewport), 2);
        assert!(panel.y1 <= viewport.y1 - 8.0);
        for index in 0..ACTIONS.len() {
            let row = action_rect(viewport, index);
            assert!(
                row.x0 >= panel.x0
                    && row.y0 >= panel.y0
                    && row.x1 <= panel.x1
                    && row.y1 <= panel.y1,
                "action {index} must be fully inside the compact panel: {row:?} / {panel:?}"
            );
            assert!(
                row.x0 >= viewport.x0
                    && row.y0 >= viewport.y0
                    && row.x1 <= viewport.x1
                    && row.y1 <= viewport.y1,
                "action {index} must be fully inside the viewport: {row:?}"
            );
        }
    }

    #[test]
    fn tiny_viewport_all_six_actions_are_reachable_by_click() {
        let viewport = Rect::new(0.0, 0.0, 480.0, 300.0);
        let expected = [
            PowerMenuClick::Lock,
            PowerMenuClick::Execute(os_actions::SessionAction::Logout),
            PowerMenuClick::Execute(os_actions::SessionAction::SwitchUser),
            PowerMenuClick::Execute(os_actions::SessionAction::Suspend),
            PowerMenuClick::Execute(os_actions::SessionAction::Restart),
            PowerMenuClick::Execute(os_actions::SessionAction::ShutDown),
        ];
        for (index, expected) in expected.into_iter().enumerate() {
            let mut menu = PowerMenu::default();
            menu.open();
            assert_eq!(
                menu.click(viewport, action_rect(viewport, index).center()),
                expected
            );
            assert!(!menu.open);
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

    #[test]
    fn power_flyout_surface_is_opaque_on_every_shipped_contrast_mode() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            assert_eq!(
                ui::chrome::shell::panel_ground(&theme).a(),
                255,
                "power flyout must fully cover bright app content"
            );
        }
    }
}
