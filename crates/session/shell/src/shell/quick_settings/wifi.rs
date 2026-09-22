//! Wi-Fi network-list slice of quick settings.
//!
//! Owns list geometry, painting, row hit-testing and join routing. The
//! password editor remains a separate, deliberately specialised overlay-host
//! slice until the shared text input can bind an ephemeral secret directly.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::keyboard::{Key, NamedKey};

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

fn f32v(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "quick-settings geometry is bounded to desktop pixel dimensions representable as f32"
    )]
    {
        value as f32
    }
}

#[derive(Default)]
pub struct QuickSettingsWifiPane {
    list: DrillInList,
    rescan_requested: bool,
    keyboard_focus: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiPaneOutcome {
    DrillIn(DrillInOutcome),
    /// A synchronous radio mutation succeeded; refresh authoritative OS state.
    Reconcile,
    ActionFailed {
        action: &'static str,
        error: String,
    },
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

        let busy = state.wifi_joining.is_some();
        if Self::wifi_toggle_rect(panel).contains(point) {
            if busy {
                return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
            }
            let next = !state.wifi;
            let result = actions.set_wifi(next);
            if let Err(error) = result {
                log_qs_result("set_wifi", Err(error.clone()));
                return WifiPaneOutcome::ActionFailed {
                    action: if next {
                        "Turn on Wi-Fi"
                    } else {
                        "Turn off Wi-Fi"
                    },
                    error,
                };
            }
            log_qs_result("set_wifi", Ok(()));
            state.wifi = next;
            state.radios.wifi_enabled = next;
            state.wifi_password = None;
            if next {
                self.rescan_requested = true;
            } else {
                state.wifi_networks.clear();
            }
            return WifiPaneOutcome::Reconcile;
        }

        if Self::rescan_rect(panel).contains(point) {
            if state.wifi && !busy && !state.wifi_scanning {
                self.rescan_requested = true;
                return WifiPaneOutcome::DrillIn(DrillInOutcome::Changed);
            }
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        }

        let hotspot = Self::hotspot_rect(panel);
        if hotspot.contains(point) {
            if !state.wifi || busy {
                return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
            }
            let next = !state.radios.hotspot_active;
            let result = actions.set_hotspot(next);
            if let Err(error) = result {
                log_qs_result("set_hotspot", Err(error.clone()));
                return WifiPaneOutcome::ActionFailed {
                    action: if next {
                        "Turn on Wi-Fi hotspot"
                    } else {
                        "Turn off Wi-Fi hotspot"
                    },
                    error,
                };
            }
            log_qs_result("set_hotspot", Ok(()));
            state.radios.hotspot_active = next;
            return WifiPaneOutcome::Reconcile;
        }

        if !state.wifi || busy {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        }
        let Some(index) = self.list.row_at(point) else {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let Some(network) = state.wifi_networks.get(index) else {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let ssid = network.ssid.clone();
        if network.connected {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        }
        if network.secure {
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
            if Self::displayed_network_count(state) == 0 {
                return;
            }
        }
        for (index, row) in self.list.visible_indices().zip(self.list.rows().iter()) {
            let Some(network) = networks.get(index) else {
                continue;
            };
            let row_cursor = if network.connected || busy {
                None
            } else {
                cursor
            };
            let glyph = DrillInList::draw_discovery_row(
                scene,
                transform,
                text,
                theme,
                *row,
                index,
                row_cursor,
                100.0,
                &network.ssid,
            );
            if self.keyboard_focus == Some(Self::NETWORK_FOCUS_START.saturating_add(index)) {
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(*row, "")
                        .ghost()
                        .focused(true)
                        .disabled(network.connected || busy)
                        .accessible_name(&network.ssid),
                );
            }
            if state.wifi_joining.as_deref() == Some(network.ssid.as_str()) {
                text.draw_text(
                    scene,
                    "Connecting…",
                    theme.tokens.typography.scale.xs,
                    colors.accent[5],
                    transform.pre_translate((row.x0 + 44.0, row.y0 + 38.0).into()),
                    Some(f32v((row.width() - 120.0).max(1.0))),
                );
            } else if network.connected {
                text.draw_text(
                    scene,
                    "Connected",
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
    struct RecordingActions {
        joins: RefCell<Vec<(String, Option<String>)>>,
        wifi: RefCell<Vec<bool>>,
        reject_wifi: bool,
    }

    impl QsActions for RecordingActions {
        fn set_brightness(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.wifi.borrow_mut().push(enabled);
            if self.reject_wifi {
                Err("radio rejected change".into())
            } else {
                Ok(())
            }
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.joins
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
            actions.joins.borrow().is_empty(),
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
        assert!(actions.joins.borrow().is_empty());
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
        assert!(actions.joins.borrow().is_empty());
    }

    #[test]
    fn connected_rows_are_inert_instead_of_rejoining() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut connected = network("Studio", true);
        connected.connected = true;
        let mut state = QuickSettingsState {
            wifi_networks: vec![connected],
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list
            .layout_from(panel, 1, QuickSettingsWifiPane::LIST_TOP);

        assert_eq!(
            pane.handle_click(pane.list.rows()[0].center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert!(state.wifi_password.is_none());
        assert!(actions.joins.borrow().is_empty());
    }

    #[test]
    fn any_in_flight_join_blocks_every_other_network() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi_networks: vec![network("First", false), network("Second", false)],
            wifi_joining: Some("First".into()),
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list
            .layout_from(panel, 2, QuickSettingsWifiPane::LIST_TOP);

        assert_eq!(
            pane.handle_click(pane.list.rows()[1].center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert_eq!(state.wifi_joining.as_deref(), Some("First"));
        assert!(actions.joins.borrow().is_empty());
    }

    #[test]
    fn header_toggle_changes_radio_only_after_a_successful_action() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi: false,
            ..Default::default()
        };
        let actions = RecordingActions::default();

        assert_eq!(
            pane.handle_click(
                QuickSettingsWifiPane::wifi_toggle_rect(panel).center(),
                panel,
                &mut state,
                &actions,
            ),
            WifiPaneOutcome::Reconcile
        );
        assert!(state.wifi);
        assert!(state.radios.wifi_enabled);
        assert_eq!(actions.wifi.borrow().as_slice(), [true]);
        assert!(pane.take_rescan_request());
        assert!(!pane.take_rescan_request(), "the scan request is one-shot");

        let mut rejected_state = QuickSettingsState {
            wifi: false,
            ..Default::default()
        };
        let rejecting = RecordingActions {
            reject_wifi: true,
            ..Default::default()
        };
        let failure = pane.handle_click(
            QuickSettingsWifiPane::wifi_toggle_rect(panel).center(),
            panel,
            &mut rejected_state,
            &rejecting,
        );
        assert!(matches!(
            failure,
            WifiPaneOutcome::ActionFailed {
                action: "Turn on Wi-Fi",
                ..
            }
        ));
        assert!(
            !rejected_state.wifi,
            "a failed OS write must not lie in the UI"
        );
        assert!(!pane.take_rescan_request());
    }

    #[test]
    fn refresh_is_named_and_requests_one_host_owned_scan() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState::default();
        let actions = RecordingActions::default();
        let targets = QuickSettingsWifiPane::action_rects(panel);
        let (_, refresh) = targets
            .iter()
            .find(|(name, _)| *name == "quick_settings_wifi_rescan")
            .copied()
            .expect("named rescan target");

        assert_eq!(
            pane.handle_click(refresh.center(), panel, &mut state, &actions),
            WifiPaneOutcome::DrillIn(DrillInOutcome::Changed)
        );
        assert!(pane.take_rescan_request());
    }

    #[test]
    fn wifi_off_hides_stale_rows_and_disables_refresh() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi: false,
            wifi_networks: vec![network("Stale", false)],
            ..Default::default()
        };
        let actions = RecordingActions::default();
        pane.list
            .layout_from(panel, 0, QuickSettingsWifiPane::LIST_TOP);

        assert_eq!(
            pane.handle_click(
                QuickSettingsWifiPane::rescan_rect(panel).center(),
                panel,
                &mut state,
                &actions,
            ),
            WifiPaneOutcome::DrillIn(DrillInOutcome::None)
        );
        assert!(!pane.take_rescan_request());
        assert!(pane.list.rows().is_empty());
    }

    #[test]
    fn keyboard_traverses_visible_actions_and_activates_the_focused_row() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let mut state = QuickSettingsState {
            wifi_networks: vec![network("Cafe", true)],
            ..Default::default()
        };
        let actions = RecordingActions::default();

        // Shift+Tab from no focus wraps to the final visible network.
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Tab),
                true,
                panel,
                &mut state,
                &actions,
            ),
            Some(WifiPaneOutcome::DrillIn(DrillInOutcome::None))
        );
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Enter),
                false,
                panel,
                &mut state,
                &actions,
            ),
            Some(WifiPaneOutcome::DrillIn(DrillInOutcome::Changed))
        );
        assert_eq!(
            state
                .wifi_password
                .as_ref()
                .map(|prompt| prompt.ssid.as_str()),
            Some("Cafe")
        );

        // Reset + Enter on the first focus stop is the visible Back action.
        state.wifi_password = None;
        pane.reset_focus();
        let _ = pane.handle_key(
            &Key::Named(NamedKey::ArrowDown),
            false,
            panel,
            &mut state,
            &actions,
        );
        assert_eq!(
            pane.handle_key(
                &Key::Named(NamedKey::Space),
                false,
                panel,
                &mut state,
                &actions,
            ),
            Some(WifiPaneOutcome::DrillIn(DrillInOutcome::Back))
        );
    }

    #[test]
    fn semantic_projection_matches_the_visible_pane() {
        let pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
        let state = QuickSettingsState {
            wifi_networks: vec![network("Cafe", true), network("Open", false)],
            ..Default::default()
        };
        let names = pane
            .semantic_rects(panel, &state)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "quick_settings_wifi_back",
                "quick_settings_wifi_toggle",
                "quick_settings_wifi_rescan",
                "quick_settings_wifi_hotspot",
                "quick_settings_wifi_network[0]",
                "quick_settings_wifi_network[1]",
            ]
        );

        let snapshot = pane.accessibility_snapshot(&state);
        assert_eq!(snapshot.name.as_deref(), Some("Wi-Fi"));
        assert_eq!(snapshot.children.len(), names.len());
        assert_eq!(snapshot.children[4].name.as_deref(), Some("Cafe"));
        assert_eq!(snapshot.children[4].description.as_deref(), Some("Secured"));
    }

    #[test]
    fn traversal_excludes_disabled_controls_and_connected_rows() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(600.0, 40.0, 948.0, 232.0);
        let mut connected = network("Already here", true);
        connected.connected = true;
        let state = QuickSettingsState {
            wifi_networks: vec![connected, network("Available", false)],
            ..Default::default()
        };

        let snapshot = pane.accessibility_snapshot(&state);
        let names = snapshot
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"Already here"));
        assert!(names.contains(&"Available"));

        // Backwards from no focus lands on the last actionable network, not
        // the connected row that intentionally has no activation.
        pane.focus_next(panel, &state, true);
        assert_eq!(
            pane.keyboard_focus,
            Some(QuickSettingsWifiPane::NETWORK_FOCUS_START + 1)
        );
        assert_eq!(pane.list.visible_indices(), 1..2);

        let busy = QuickSettingsState {
            wifi_joining: Some("Available".into()),
            ..state
        };
        assert_eq!(
            QuickSettingsWifiPane::focusable_indices(&busy),
            [QuickSettingsWifiPane::BACK_FOCUS],
            "an in-flight join leaves only Back in traversal"
        );
    }

    #[test]
    fn compact_wheel_exposes_only_the_scrolled_network_window() {
        let mut pane = QuickSettingsWifiPane::default();
        let panel = Rect::new(600.0, 40.0, 948.0, 232.0);
        let state = QuickSettingsState {
            wifi_networks: (0..8)
                .map(|index| network(&format!("Network {index}"), false))
                .collect(),
            ..Default::default()
        };
        pane.list.layout_from(
            panel,
            state.wifi_networks.len(),
            QuickSettingsWifiPane::LIST_TOP,
        );
        assert_eq!(pane.list.visible_indices(), 0..1);
        assert!(pane.scroll_wheel(panel, &state, 4.0));
        assert_eq!(pane.list.visible_indices(), 4..5);
        let names = pane
            .semantic_rects(panel, &state)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"quick_settings_wifi_network[4]".to_string()));
        assert!(!names.contains(&"quick_settings_wifi_network[0]".to_string()));
    }

    #[test]
    fn off_loading_empty_and_error_have_distinct_truthful_status() {
        let mut state = QuickSettingsState {
            wifi: false,
            ..Default::default()
        };
        assert_eq!(
            QuickSettingsWifiPane::status_message(&state),
            Some("Wi-Fi is off")
        );

        state.wifi = true;
        state.wifi_scanning = true;
        assert_eq!(
            QuickSettingsWifiPane::status_message(&state),
            Some("Scanning for networks…")
        );

        state.wifi_scanning = false;
        assert_eq!(
            QuickSettingsWifiPane::status_message(&state),
            Some("No networks found")
        );

        state.wifi_scan_error = Some("NetworkManager unavailable".into());
        assert_eq!(
            QuickSettingsWifiPane::status_message(&state),
            Some("Couldn’t scan · Refresh to try again")
        );
        assert_eq!(
            QuickSettingsWifiPane::default()
                .accessibility_snapshot(&state)
                .description
                .as_deref(),
            Some("Couldn’t scan · Refresh to try again")
        );
    }
}
