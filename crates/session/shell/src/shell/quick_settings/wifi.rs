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
use ui::widget::{Toggle, ToggleFace};

use crate::shell::chrome_state::QuickSettingsState;
#[cfg(test)]
use crate::shell::quick_settings::WifiNetwork;
use crate::shell::quick_settings::WifiPasswordPrompt;
use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use crate::shell::quick_settings::drill_in::{
    CONTROL_INSET, DrillInList, DrillInOutcome, GLYPH, LIST_TOP, PANEL_PAD, RELATED_GAP,
    ROW_HEIGHT, STATUS_DOT, TOGGLE_H, TOGGLE_W, centred_text_top, leading_glyph, paint_control,
    trailing_dot_center, trailing_status_glyph, trailing_toggle, type_size,
};

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
    /// Discovery rows start under the Hotspot row: one related step below
    /// it, on the shared list rhythm.
    pub(crate) const LIST_TOP: f64 = LIST_TOP + ROW_HEIGHT + RELATED_GAP;

    /// The Hotspot row: the first Control-height row on the panel padding.
    fn hotspot_rect_for(panel: Rect) -> Rect {
        Rect::new(
            panel.x0 + PANEL_PAD,
            panel.y0 + LIST_TOP,
            panel.x1 - PANEL_PAD,
            panel.y0 + LIST_TOP + ROW_HEIGHT,
        )
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> WifiPaneOutcome {
        self.list
            .layout_from(panel, state.wifi_networks.len(), Self::LIST_TOP);
        // The same geometry paint uses, so a click before the first frame
        // still lands on the row that will be drawn.
        self.hotspot_rect = Self::hotspot_rect_for(panel);
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
        if network.secure && !network.connected {
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
        self.list.layout_from(panel, networks.len(), Self::LIST_TOP);
        self.hotspot_rect = Self::hotspot_rect_for(panel);
        let colors = &theme.tokens.colors;
        self.list
            .draw_header(scene, transform, text, theme, panel, cursor, "Wi-Fi");
        // The Hotspot row is a Control (HIG §9.1/§9.5): glyph, two-line
        // label and switch all on its centre line and the shared inset.
        let row = self.hotspot_rect;
        let hovered = cursor.is_some_and(|point| row.contains(point));
        paint_control(scene, transform, theme, row, hovered, false);
        let glyph = leading_glyph(row, CONTROL_INSET, GLYPH);
        ui::icons::draw_icon(
            scene,
            transform,
            glyph,
            ui::icons::Icon::Wifi,
            colors.text_primary,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let label_size = type_size(text, theme.tokens.typography.scale.sm);
        let detail_size = type_size(text, theme.tokens.typography.scale.xs);
        let detail = if state.radios.hotspot_active {
            "Sharing this connection"
        } else {
            "Off"
        };
        let (_, label_h) = text.measure_weighted(
            "Hotspot",
            label_size,
            None,
            ui::text_render::TextWeight::Medium,
        );
        let (_, detail_h) = text.measure(detail, detail_size, None);
        let block_top = centred_text_top(row, f64::from(label_h) + f64::from(detail_h));
        let text_x = glyph.x1 + CONTROL_INSET;
        text.draw_text_weighted(
            scene,
            "Hotspot",
            label_size,
            colors.text_primary,
            transform.pre_translate((text_x, block_top).into()),
            None,
            ui::text_render::TextWeight::Medium,
        );
        text.draw_text(
            scene,
            detail,
            detail_size,
            colors.text_dim,
            transform.pre_translate((text_x, block_top + f64::from(label_h)).into()),
            None,
        );
        Toggle::paint(
            scene,
            transform,
            theme,
            &ToggleFace::new(
                trailing_toggle(row, CONTROL_INSET, TOGGLE_W, TOGGLE_H),
                state.radios.hotspot_active,
            )
            .hovered(hovered),
        );
        if networks.is_empty() {
            DrillInList::draw_empty_from(
                scene,
                transform,
                text,
                theme,
                panel,
                "Scanning for networks…",
                Self::LIST_TOP,
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
            // Four signal bars fill the trailing status slot on a pitch of a
            // fifth of it, rising by 15 % of its height per bar, so the
            // tallest bar's top and their shared foot sit symmetric about
            // the row's centre line.
            let slot = trailing_status_glyph(*row, CONTROL_INSET, GLYPH);
            let pitch = slot.width() / 5.0;
            let step = slot.height() * 0.15;
            let baseline = row.center().y + 2.0 * step;
            for bar in 0..4_u8 {
                let x = slot.x0 + pitch * f64::from(bar.saturating_add(1));
                let height = f64::from(bar.saturating_add(1)) * step;
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
                        Rect::new(x, baseline - height, x + pitch * 0.75, baseline),
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
                    &Circle::new(trailing_dot_center(*row, CONTROL_INSET), STATUS_DOT / 2.0),
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
        pane.list
            .layout_from(panel, 2, QuickSettingsWifiPane::LIST_TOP);
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
}
