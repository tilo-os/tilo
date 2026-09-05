//! Wi-Fi network-list slice of quick settings.
//!
//! Owns list geometry, painting, row hit-testing and join routing. The
//! password editor remains a separate, deliberately specialised overlay-host
//! slice until the shared text input can bind an ephemeral secret directly.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::ColorCompat;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, Toggle, ToggleFace};

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::qs_actions::{QsActions, log_qs_result};
#[cfg(test)]
use crate::shell::quick_settings::WifiNetwork;
use crate::shell::quick_settings::WifiPasswordPrompt;
use crate::shell::quick_settings_drill_in::{DrillInList, DrillInOutcome};

#[derive(Default)]
pub struct QuickSettingsWifiPane {
    list: DrillInList,
    hotspot_rect: Rect,
}

impl QuickSettingsWifiPane {
    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> DrillInOutcome {
        self.list
            .layout_from(panel, state.wifi_networks.len(), 116.0);
        if self.list.back_contains(point) {
            return DrillInOutcome::Back;
        }
        if self.hotspot_rect.contains(point) {
            state.radios.hotspot_active = !state.radios.hotspot_active;
            log_qs_result(
                "set_hotspot",
                actions.set_hotspot(state.radios.hotspot_active),
            );
            return DrillInOutcome::Changed;
        }
        let Some(index) = self.list.row_at(point) else {
            return DrillInOutcome::None;
        };
        let Some(network) = state.wifi_networks.get(index) else {
            return DrillInOutcome::None;
        };
        let ssid = network.ssid.clone();
        if network.secure && !network.connected {
            state.wifi_password = Some(WifiPasswordPrompt::new(ssid));
            DrillInOutcome::Changed
        } else {
            log_qs_result("connect_wifi", actions.connect_wifi(&ssid, None));
            DrillInOutcome::None
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching the sibling \
                  bluetooth/vpn drill-in panes this is called alongside"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cursor: Option<Point>,
        state: &QuickSettingsState,
    ) {
        let networks = &state.wifi_networks;
        self.list.layout_from(panel, networks.len(), 116.0);
        self.hotspot_rect = Rect::new(
            panel.x0 + 12.0,
            panel.y0 + 56.0,
            panel.x1 - 12.0,
            panel.y0 + 108.0,
        );
        let colors = &theme.tokens.colors;
        self.list
            .draw_header(scene, transform, text, theme, panel, cursor, "Wi-Fi");
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(self.hotspot_rect, "")
                .hovered(cursor.is_some_and(|point| self.hotspot_rect.contains(point)))
                .accessible_name("Wi-Fi hotspot"),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                self.hotspot_rect.x0 + 12.0,
                self.hotspot_rect.y0 + 16.0,
                self.hotspot_rect.x0 + 32.0,
                self.hotspot_rect.y0 + 36.0,
            ),
            ui::icons::Icon::Wifi,
            colors.text_primary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text.draw_text_weighted(
            scene,
            "Hotspot",
            theme.tokens.typography.scale.sm,
            colors.text_primary,
            transform
                .pre_translate((self.hotspot_rect.x0 + 44.0, self.hotspot_rect.y0 + 22.0).into()),
            None,
            ui::text_render::TextWeight::Medium,
        );
        text.draw_text(
            scene,
            if state.radios.hotspot_active {
                "Sharing this connection"
            } else {
                "Off"
            },
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform
                .pre_translate((self.hotspot_rect.x0 + 44.0, self.hotspot_rect.y0 + 39.0).into()),
            None,
        );
        Toggle::paint(
            scene,
            transform,
            theme,
            &ToggleFace::new(
                Rect::new(
                    self.hotspot_rect.x1 - 48.0,
                    self.hotspot_rect.y0 + 16.0,
                    self.hotspot_rect.x1 - 12.0,
                    self.hotspot_rect.y0 + 36.0,
                ),
                state.radios.hotspot_active,
            )
            .hovered(cursor.is_some_and(|point| self.hotspot_rect.contains(point))),
        );
        if networks.is_empty() {
            let message = "Scanning for networks…";
            let (width, _) = text.measure(message, theme.tokens.typography.scale.sm, None);
            text.draw_text(
                scene,
                message,
                theme.tokens.typography.scale.sm,
                colors.text_dim,
                transform.pre_translate(
                    (panel.center().x - f64::from(width) / 2.0, panel.y0 + 144.0).into(),
                ),
                None,
            );
            return;
        }
        for (index, (network, row)) in networks.iter().zip(self.list.rows()).enumerate() {
            let glyph = DrillInList::draw_discovery_row(
                scene,
                transform,
                text,
                theme,
                *row,
                index,
                cursor,
                &network.ssid,
            );
            ui::icons::draw_icon(
                scene,
                transform,
                glyph,
                ui::icons::Icon::Wifi,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            let baseline = row.y0 + row.height() / 2.0 + 6.0;
            for bar in 0..4_u8 {
                let x = row.x1 - 62.0 + f64::from(bar) * 4.0;
                let height = f64::from(bar.saturating_add(1)) * 3.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if bar < network.signal {
                        colors.text_primary.with_alpha_factor(0.85)
                    } else {
                        colors.text_primary.with_alpha_factor(0.20)
                    },
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(x, baseline - height, x + 3.0, baseline),
                        1.0,
                    ),
                );
            }
            if network.connected {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &Circle::new(Point::new(row.x1 - 14.0, row.center().y), 5.0),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingActions(RefCell<Vec<(String, Option<String>)>>);

    impl QsActions for RecordingActions {
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
        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.0
                .borrow_mut()
                .push((ssid.to_owned(), password.map(str::to_owned)));
            Ok(())
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }

    fn network(ssid: &str, secure: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal: 3,
            secure,
            connected: false,
        }
    }

    #[test]
    fn protected_and_open_rows_take_distinct_paths() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi_networks: vec![network("Cafe", true), network("Open", false)],
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list.layout(panel, 2);
        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut state, &actions),
            DrillInOutcome::Changed
        );
        assert_eq!(state.wifi_password.as_ref().unwrap().ssid, "Cafe");
        state.wifi_password = None;
        assert_eq!(
            pane.handle_click(pane.list.rows()[1].center(), panel, &mut state, &actions),
            DrillInOutcome::None
        );
        assert_eq!(actions.0.borrow().as_slice(), [("Open".into(), None)]);
    }

    #[test]
    fn back_and_empty_space_never_join_a_network() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi_networks: vec![network("Cafe", true)],
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list.layout(panel, 1);
        assert_eq!(
            pane.handle_click(pane.list.back_center(), panel, &mut state, &actions),
            DrillInOutcome::Back
        );
        assert_eq!(
            pane.handle_click(panel.center(), panel, &mut state, &actions),
            DrillInOutcome::None
        );
        assert!(actions.0.borrow().is_empty());
        assert!(state.wifi_password.is_none());
    }
}
