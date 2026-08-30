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
}

impl Default for PowerMenu {
    fn default() -> Self {
        Self {
            open: false,
            transition: ui::Transition::closed(),
            armed: None,
        }
    }
}

impl PowerMenu {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.armed = None;
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
        if index >= 3 && self.armed != Some(index) {
            self.armed = Some(index);
            return PowerMenuClick::Consumed;
        }
        self.close();
        PowerMenuClick::Execute(ACTIONS[index].0)
    }

    pub(crate) fn draw(
        &mut self,
        viewport: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
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
            transform * Affine::translate((panel.x0 + 22.0, panel.y0 + 20.0)),
            None,
            TextWeight::Semibold,
        );
        for (index, (_, label, detail, icon)) in ACTIONS.iter().enumerate() {
            let row = action_rect(viewport, index);
            let armed = self.armed == Some(index);
            scene.fill(
                Fill::NonZero,
                transform,
                if armed {
                    colors.semantic.danger.with_alpha_factor(0.18)
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
                transform * Affine::translate((row.x0 + 50.0, row.y0 + 8.0)),
                None,
                TextWeight::Semibold,
            );
            text_renderer.draw_text(
                scene,
                detail,
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                transform * Affine::translate((row.x0 + 50.0, row.y0 + 28.0)),
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
    let y0 = panel.y0 + 62.0 + index as f64 * 58.0;
    Rect::new(panel.x0 + 18.0, y0, panel.x1 - 18.0, y0 + 50.0)
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
    fn opening_requests_animation_frames_without_waiting_for_draw() {
        let mut menu = PowerMenu::default();
        menu.open();
        assert!(menu.is_animating(Instant::now()));
    }
}
