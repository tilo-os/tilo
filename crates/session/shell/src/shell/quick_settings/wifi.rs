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
#[cfg(test)]
use crate::shell::quick_settings::WifiNetwork;
use crate::shell::quick_settings::WifiPasswordPrompt;
use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use crate::shell::quick_settings::drill_in::{DrillInList, DrillInOutcome};

#[derive(Default)]
pub struct QuickSettingsWifiPane {
    list: DrillInList,
    hotspot_rect: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiPaneOutcome {
    DrillIn(DrillInOutcome),
    JoinOpen(String),
}

impl QuickSettingsWifiPane {
    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> WifiPaneOutcome {
        self.list
            .layout_from(panel, state.wifi_networks.len(), 116.0);
        if self.list.back_contains(point) {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::Back);
        }
        if self.hotspot_rect.contains(point) {
            state.radios.hotspot_active = !state.radios.hotspot_active;
            log_qs_result(
                "set_hotspot",
                actions.set_hotspot(state.radios.hotspot_active),
            );
            return WifiPaneOutcome::DrillIn(DrillInOutcome::Changed);
        }
        let Some(index) = self.list.row_at(point) else {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let Some(network) = state.wifi_networks.get(index) else {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let ssid = network.ssid.clone();
        if state.wifi_joining.as_deref() == Some(ssid.as_str()) {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        }
        if network.connected {
            // The active row is status, not an invitation to repeat a
            // potentially slow NetworkManager activation. Switching away is
            // done by choosing another row; turning Wi-Fi off remains on the
            // root card.
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        } else if network.secure {
            state.wifi_password = Some(WifiPasswordPrompt::new(ssid));
            WifiPaneOutcome::DrillIn(DrillInOutcome::Changed)
        } else {
            // Joining can wait on NetworkManager for tens of seconds. Return
            // intent to the host, which owns the asynchronous worker and the
            // resulting visible progress/error state.
            WifiPaneOutcome::JoinOpen(ssid)
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching the sibling \
                  bluetooth/vpn drill-in panes this is called alongside"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "The drill-in's paint, scroll, and Wi-Fi hit targets share one panel coordinate system and must remain synchronized."
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
            if state.wifi_joining.as_deref() == Some(network.ssid.as_str()) {
                text.draw_text(
                    scene,
                    "Connecting…",
                    theme.tokens.typography.scale.xs,
                    colors.accent[5],
                    transform.pre_translate((row.x0 + 44.0, row.y0 + 38.0).into()),
                    Some((row.width() - 120.0).max(1.0) as f32),
                );
            }
            ui::icons::draw_icon(
                scene,
                transform,
                glyph,
                ui::icons::Icon::Wifi,
                colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            if network.secure {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        row.x1 - 86.0,
                        row.center().y - 7.0,
                        row.x1 - 72.0,
                        row.center().y + 7.0,
                    ),
                    ui::icons::Icon::Lock,
                    colors.text_dim,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
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
        // The pane reserves its hotspot card before discovery rows; prepare
        // the same geometry that `handle_click` will use.
        pane.list.layout_from(panel, 2, 116.0);
        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::Changed)
        );
        assert_eq!(state.wifi_password.as_ref().unwrap().ssid, "Cafe");
        state.wifi_password = None;
        assert_eq!(
            pane.handle_click(pane.list.rows()[1].center(), panel, &mut state, &actions),
            WifiPaneOutcome::JoinOpen("Open".into())
        );
        assert!(
            actions.0.borrow().is_empty(),
            "the pane never blocks on a join"
        );
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
            WifiPaneOutcome::DrillIn(DrillInOutcome::Back)
        );
        assert_eq!(
            pane.handle_click(panel.center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert!(actions.0.borrow().is_empty());
        assert!(state.wifi_password.is_none());
    }

    #[test]
    fn an_in_flight_open_network_join_is_not_dispatched_twice() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi_networks: vec![network("Open", false)],
            wifi_joining: Some("Open".into()),
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list.layout_from(panel, 1, 116.0);
        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert!(actions.0.borrow().is_empty());
    }

    #[test]
    fn clicking_the_connected_row_is_a_no_op() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut connected = network("Home", true);
        connected.connected = true;
        let mut state = QuickSettingsState {
            wifi_networks: vec![connected],
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list.layout_from(panel, 1, 116.0);

        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert!(state.wifi_password.is_none());
        assert!(state.wifi_joining.is_none());
        assert!(actions.0.borrow().is_empty());
    }
}
