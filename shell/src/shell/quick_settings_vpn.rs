//! VPN drill-in for quick settings.
//!
//! Owns its painted geometry, shared toggle faces, hit-testing, optimistic
//! local state and the OS dispatch. The overlay host only supplies the panel
//! bounds and maps `Back`/`Changed` into navigation and chrome invalidation.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ButtonStyle, Toggle, ToggleFace};

use crate::shell::qs_actions::{QsActions, log_qs_result};

const HEADER_HEIGHT: f64 = 48.0;
const LIST_TOP: f64 = 56.0;
const ROW_HEIGHT: f64 = 56.0;
const ROW_INSET: f64 = 12.0;

#[allow(clippy::cast_precision_loss, reason = "VPN profile counts are tiny")]
fn usize_to_f64_lossy(value: usize) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded screen-space widths retain sufficient f32 precision"
)]
fn f64_to_f32_lossy(value: f64) -> f32 {
    value as f32
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VpnPaneOutcome {
    #[default]
    None,
    Back,
    Changed,
}

#[derive(Default)]
pub struct QuickSettingsVpnPane {
    back_rect: Rect,
    row_rects: Vec<Rect>,
}

impl QuickSettingsVpnPane {
    fn layout(&mut self, panel: Rect, row_count: usize) {
        self.back_rect = Rect::new(
            panel.x0 + 8.0,
            panel.y0 + 8.0,
            panel.x0 + 40.0,
            panel.y0 + 40.0,
        );
        self.row_rects.clear();
        self.row_rects.extend((0..row_count).map(|index| {
            let y = panel.y0 + LIST_TOP + usize_to_f64_lossy(index) * ROW_HEIGHT;
            Rect::new(
                panel.x0 + ROW_INSET,
                y,
                panel.x1 - ROW_INSET,
                y + ROW_HEIGHT - 4.0,
            )
        }));
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        profiles: &mut [os_actions::VpnConnectionInfo],
        actions: &dyn QsActions,
    ) -> VpnPaneOutcome {
        self.layout(panel, profiles.len());
        if self.back_rect.contains(point) {
            return VpnPaneOutcome::Back;
        }
        let Some(index) = self.row_rects.iter().position(|row| row.contains(point)) else {
            return VpnPaneOutcome::None;
        };
        let Some(profile) = profiles.get_mut(index) else {
            return VpnPaneOutcome::None;
        };
        let next = !profile.active;
        log_qs_result(
            "set_vpn_connection",
            actions.set_vpn_connection(&profile.name, next),
        );
        // Preserve the existing optimistic UI contract: the asynchronous OS
        // refresh reconciles a rejected transition on its next reading.
        profile.active = next;
        VpnPaneOutcome::Changed
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cursor: Option<Point>,
        profiles: &[os_actions::VpnConnectionInfo],
    ) {
        self.layout(panel, profiles.len());
        let colors = &theme.tokens.colors;
        let back_hovered = cursor.is_some_and(|point| self.back_rect.contains(point));
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(self.back_rect, "")
                .style(ButtonStyle::Ghost)
                .icon(ui::icons::Icon::ChevronLeft)
                .hovered(back_hovered)
                .accessible_name("Back"),
        );

        let title = "VPN";
        let (title_w, title_h) = text.measure(title, theme.tokens.typography.scale.sm, None);
        text.draw_text_weighted(
            scene,
            title,
            theme.tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate(
                (
                    panel.center().x - f64::from(title_w) / 2.0,
                    panel.y0 + (HEADER_HEIGHT - f64::from(title_h)) / 2.0,
                )
                    .into(),
            ),
            None,
            TextWeight::Semibold,
        );

        if profiles.is_empty() {
            let message = "No VPN profiles yet · add one in Settings";
            let (w, h) = text.measure(message, theme.tokens.typography.scale.sm, None);
            text.draw_text(
                scene,
                message,
                theme.tokens.typography.scale.sm,
                colors.text_dim,
                transform.pre_translate(
                    (
                        panel.center().x - f64::from(w) / 2.0,
                        panel.y0 + LIST_TOP + 24.0 - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
            return;
        }

        for (profile, row) in profiles.iter().zip(&self.row_rects) {
            let hovered = cursor.is_some_and(|point| row.contains(point));
            let fill = if profile.active {
                colors.accent[5].with_alpha_factor(if hovered { 0.24 } else { 0.18 })
            } else if hovered {
                colors.neutral[3]
            } else {
                colors.neutral[2]
            };
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(*row, 12.0),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(row.x0 + 12.0, row.y0 + 16.0, row.x0 + 32.0, row.y0 + 36.0),
                ui::icons::Icon::Shield,
                if profile.active {
                    colors.accent[5]
                } else {
                    colors.text_dim
                },
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            let toggle = Rect::new(
                row.x1 - 56.0,
                row.center().y - 12.0,
                row.x1 - 12.0,
                row.center().y + 12.0,
            );
            Toggle::paint(
                scene,
                transform,
                theme,
                &ToggleFace::new(toggle, profile.active).hovered(hovered),
            );
            let text_width = (toggle.x0 - 12.0 - (row.x0 + 44.0)).max(1.0);
            let name = text.elide_to_width(
                &profile.name,
                theme.tokens.typography.scale.sm,
                f64_to_f32_lossy(text_width),
            );
            text.draw_text_weighted(
                scene,
                &name,
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((row.x0 + 44.0, row.y0 + 9.0).into()),
                None,
                TextWeight::Medium,
            );
            text.draw_text(
                scene,
                if profile.active {
                    "Connected"
                } else {
                    "Disconnected"
                },
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((row.x0 + 44.0, row.y0 + 29.0).into()),
                Some(f64_to_f32_lossy(text_width)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingActions(RefCell<Vec<(String, bool)>>);

    impl os_actions::Dispatcher for RecordingActions {
        fn set_brightness(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
        fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
            self.0.borrow_mut().push((name.to_owned(), enabled));
            Ok(())
        }
    }

    fn profile(active: bool) -> os_actions::VpnConnectionInfo {
        os_actions::VpnConnectionInfo {
            name: "Work".into(),
            active,
            kind: "wireguard".into(),
        }
    }

    #[test]
    fn row_click_dispatches_and_updates_the_matching_profile() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(false)];
        let actions = RecordingActions::default();
        pane.layout(panel, profiles.len());
        let outcome = pane.handle_click(pane.row_rects[0].center(), panel, &mut profiles, &actions);
        assert_eq!(outcome, VpnPaneOutcome::Changed);
        assert!(profiles[0].active);
        assert_eq!(actions.0.borrow().as_slice(), [("Work".into(), true)]);
    }

    #[test]
    fn back_and_empty_space_do_not_dispatch_hardware_actions() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(true)];
        let actions = RecordingActions::default();
        pane.layout(panel, profiles.len());
        assert_eq!(
            pane.handle_click(pane.back_rect.center(), panel, &mut profiles, &actions),
            VpnPaneOutcome::Back
        );
        assert_eq!(
            pane.handle_click(panel.center(), panel, &mut profiles, &actions),
            VpnPaneOutcome::None
        );
        assert!(actions.0.borrow().is_empty());
        assert!(profiles[0].active);
    }
}
