//! Session power modal: state, layout, paint, animation, and click policy.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::shell::overlay_host::{
    MODAL_OPEN_ANIM_MS, MODAL_SCRIM_ALPHA, fill_scrim, modal_transition_curve,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerMenuClick {
    Ignored,
    Consumed,
    Execute(os_actions::SessionAction),
}

pub(crate) struct PowerMenu {
    open: bool,
    transition: ui::Transition,
    armed: Option<usize>,
    selected: usize,
}

impl Default for PowerMenu {
    fn default() -> Self {
        Self {
            open: false,
            transition: ui::Transition::closed(),
            armed: None,
            selected: 0,
        }
    }
}

impl PowerMenu {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.armed = None;
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
        self.armed = None;
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

    pub(crate) fn click(&mut self, viewport: Rect, point: Point) -> PowerMenuClick {
        if !self.open {
            return PowerMenuClick::Ignored;
        }
        if !panel_rect(viewport).contains(point) {
            self.close();
            return PowerMenuClick::Consumed;
        }
        let Some(index) =
            (0..ACTIONS.len()).find(|&index| action_rect(viewport, index).contains(point))
        else {
            return PowerMenuClick::Consumed;
        };
        self.selected = index;
        self.activate(index)
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
                self.armed = None;
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                self.selected = if self.selected.saturating_add(1) >= ACTIONS.len() {
                    0
                } else {
                    self.selected.saturating_add(1)
                };
                self.armed = None;
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
                self.armed = None;
                PowerMenuClick::Consumed
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate(self.selected),
            _ => PowerMenuClick::Consumed,
        }
    }

    fn activate(&mut self, index: usize) -> PowerMenuClick {
        if index >= 3 && self.armed != Some(index) {
            self.armed = Some(index);
            return PowerMenuClick::Consumed;
        }
        self.close();
        // `index` is drawn from `0..ACTIONS.len()` above, so this is
        // always present; `.get()` keeps the lookup panic-free regardless.
        ACTIONS
            .get(index)
            .map_or(PowerMenuClick::Consumed, |action| {
                PowerMenuClick::Execute(action.0)
            })
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
        let openness = self.transition.value(now);
        fill_scrim(
            scene,
            transform,
            viewport,
            MODAL_SCRIM_ALPHA * openness,
            theme,
        );
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
        scene.fill(
            Fill::NonZero,
            transform,
            colors.neutral[1],
            None,
            &RoundedRect::from_rect(panel, 24.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            "Session",
            theme.tokens.typography.scale.lg,
            colors.text_primary,
            at(transform, (panel.x0 + 22.0, panel.y0 + 20.0)),
            None,
            TextWeight::Semibold,
        );
        for (index, (_, label, detail, icon)) in ACTIONS.iter().enumerate() {
            let row = action_rect(viewport, index);
            let armed = self.armed == Some(index);
            let selected = self.selected == index;
            scene.fill(
                Fill::NonZero,
                transform,
                if armed {
                    colors.semantic.danger.with_alpha_factor(0.18)
                } else if selected {
                    colors.accent[5].with_alpha_factor(0.16)
                } else {
                    colors.neutral[2]
                },
                None,
                &RoundedRect::from_rect(row, 14.0),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(row.x0 + 14.0, row.y0 + 14.0, row.x0 + 36.0, row.y0 + 36.0),
                *icon,
                if armed {
                    colors.semantic.danger
                } else {
                    colors.text_primary
                },
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            text_renderer.draw_text_weighted(
                scene,
                if armed {
                    "Click again to confirm"
                } else {
                    label
                },
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                at(transform, (row.x0 + 50.0, row.y0 + 8.0)),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                detail,
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                at(transform, (row.x0 + 50.0, row.y0 + 28.0)),
                None,
            );
        }
    }
}

const ACTIONS: [(os_actions::SessionAction, &str, &str, ui::icons::Icon); 5] = [
    (
        os_actions::SessionAction::Lock,
        "Lock",
        "Keep apps running",
        ui::icons::Icon::Shield,
    ),
    (
        os_actions::SessionAction::Logout,
        "Log out",
        "End this session",
        ui::icons::Icon::ArrowRight,
    ),
    (
        os_actions::SessionAction::Suspend,
        "Suspend",
        "Resume where you left off",
        ui::icons::Icon::Sun,
    ),
    (
        os_actions::SessionAction::Restart,
        "Restart",
        "Restart the computer",
        ui::icons::Icon::RotateCw,
    ),
    (
        os_actions::SessionAction::ShutDown,
        "Shut down",
        "Turn off the computer",
        ui::icons::Icon::Power,
    ),
];

fn panel_rect(viewport: Rect) -> Rect {
    let width = 420.0_f64.min(viewport.width() - 48.0);
    let height = 388.0_f64.min(viewport.height() - 48.0);
    let centre = viewport.center();
    Rect::new(
        centre.x - width / 2.0,
        centre.y - height / 2.0,
        centre.x + width / 2.0,
        centre.y + height / 2.0,
    )
}

fn action_rect(viewport: Rect, index: usize) -> Rect {
    let panel = panel_rect(viewport);
    let y0 = panel.y0 + 62.0 + small_count_to_f64(index) * 58.0;
    Rect::new(panel.x0 + 18.0, y0, panel.x1 - 18.0, y0 + 50.0)
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
    fn ordinary_actions_execute_on_first_click() {
        let mut menu = PowerMenu::default();
        menu.open();
        assert_eq!(
            menu.click(VIEWPORT, action_rect(VIEWPORT, 0).center()),
            PowerMenuClick::Execute(os_actions::SessionAction::Lock)
        );
        assert!(!menu.open);
    }

    #[test]
    fn destructive_actions_require_two_clicks() {
        let mut menu = PowerMenu::default();
        menu.open();
        let point = action_rect(VIEWPORT, 4).center();
        assert_eq!(menu.click(VIEWPORT, point), PowerMenuClick::Consumed);
        assert!(menu.open);
        assert_eq!(
            menu.click(VIEWPORT, point),
            PowerMenuClick::Execute(os_actions::SessionAction::ShutDown)
        );
    }

    #[test]
    fn switching_destructive_rows_rearms_confirmation() {
        let mut menu = PowerMenu::default();
        menu.open();
        assert_eq!(
            menu.click(VIEWPORT, action_rect(VIEWPORT, 3).center()),
            PowerMenuClick::Consumed
        );
        assert_eq!(
            menu.click(VIEWPORT, action_rect(VIEWPORT, 4).center()),
            PowerMenuClick::Consumed
        );
        assert!(menu.open);
    }

    #[test]
    fn outside_click_closes_without_an_action() {
        let mut menu = PowerMenu::default();
        menu.open();
        assert_eq!(menu.click(VIEWPORT, Point::ZERO), PowerMenuClick::Consumed);
        assert!(!menu.open);
    }

    #[test]
    fn keyboard_navigation_reaches_actions_and_keeps_confirmation() {
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
        menu.selected = 4;
        assert_eq!(
            menu.key(&Key::Named(NamedKey::Space), false),
            PowerMenuClick::Consumed
        );
        assert!(menu.open, "first activation only arms Shut down");
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
}
