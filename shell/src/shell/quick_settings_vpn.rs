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
use ui::widget::{Toggle, ToggleFace};

use crate::shell::qs_actions::{QsActions, log_qs_result};
use crate::shell::quick_settings_drill_in::{DrillInList, DrillInOutcome};

#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded screen-space widths retain sufficient f32 precision"
)]
fn f64_to_f32_lossy(value: f64) -> f32 {
    value as f32
}

#[derive(Default)]
pub struct QuickSettingsVpnPane {
    list: DrillInList,
}

impl QuickSettingsVpnPane {
    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        profiles: &mut [os_actions::VpnConnectionInfo],
        actions: &dyn QsActions,
    ) -> DrillInOutcome {
        self.list.layout(panel, profiles.len());
        if self.list.back_contains(point) {
            return DrillInOutcome::Back;
        }
        let Some(index) = self.list.row_at(point) else {
            return DrillInOutcome::None;
        };
        let Some(profile) = profiles.get_mut(index) else {
            return DrillInOutcome::None;
        };
        let next = !profile.active;
        log_qs_result(
            "set_vpn_connection",
            actions.set_vpn_connection(&profile.name, next),
        );
        // Preserve the existing optimistic UI contract: the asynchronous OS
        // refresh reconciles a rejected transition on its next reading.
        profile.active = next;
        DrillInOutcome::Changed
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching the sibling \
                  wifi/bluetooth drill-in panes this is called alongside"
    )]
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
        self.list.layout(panel, profiles.len());
        let colors = &theme.tokens.colors;
        self.list
            .draw_header(scene, transform, text, theme, panel, cursor, "VPN");
        if profiles.is_empty() {
            DrillInList::draw_empty(
                scene,
                transform,
                text,
                theme,
                panel,
                "No VPN profiles yet · add one in Settings",
            );
            return;
        }

        for (profile, row) in profiles.iter().zip(self.list.rows()) {
            let hovered = cursor.is_some_and(|point| row.contains(point));
            // A discovery row is not a selected tab. Wi-Fi and Bluetooth
            // stay quiet and reserve blue for an actual live state; VPN is
            // a control row, so its enabled state belongs to the trailing
            // switch while the text reads "Connected". Keeping the surface
            // neutral avoids saying the same thing three different ways.
            let fill = if hovered {
                colors.neutral[3].with_alpha_factor(0.72)
            } else {
                colors.neutral[2].with_alpha_factor(0.42)
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
                colors.text_dim,
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
                if profile.active {
                    colors.accent[5]
                } else {
                    colors.text_dim
                },
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
        pane.list.layout(panel, profiles.len());
        let outcome =
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut profiles, &actions);
        assert_eq!(outcome, DrillInOutcome::Changed);
        assert!(profiles[0].active);
        assert_eq!(actions.0.borrow().as_slice(), [("Work".into(), true)]);
    }

    #[test]
    fn back_and_empty_space_do_not_dispatch_hardware_actions() {
        let mut pane = QuickSettingsVpnPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut profiles = vec![profile(true)];
        let actions = RecordingActions::default();
        pane.list.layout(panel, profiles.len());
        assert_eq!(
            pane.handle_click(pane.list.back_center(), panel, &mut profiles, &actions),
            DrillInOutcome::Back
        );
        assert_eq!(
            pane.handle_click(panel.center(), panel, &mut profiles, &actions),
            DrillInOutcome::None
        );
        assert!(actions.0.borrow().is_empty());
        assert!(profiles[0].active);
    }
}
