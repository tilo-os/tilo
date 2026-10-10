//! Wi-Fi network-list slice of quick settings.
//!
//! Owns list geometry, painting, row hit-testing and join routing. The
//! password editor remains a separate, deliberately specialised overlay-host
//! slice until the shared text input can bind an ephemeral secret directly.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::keyboard::Key;

use ui::ColorCompat;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, Toggle, ToggleFace};

use crate::shell::i18n::t;

use crate::shell::chrome_state::QuickSettingsState;
#[cfg(test)]
use crate::shell::quick_settings::WifiNetwork;
use crate::shell::quick_settings::WifiPasswordPrompt;
use crate::shell::quick_settings::actions::{QsActions, log_qs_result};
use tilo_desktop::drill_in::{
    CONTROL_INSET, DrillInList, DrillInOutcome, GLYPH, LIST_TOP, PANEL_PAD, RELATED_GAP,
    ROW_HEIGHT, STATUS_DOT, TOGGLE_H, TOGGLE_W, centred_text_top, leading_glyph, paint_control,
    trailing_dot_center, trailing_status_glyph, trailing_toggle,
};
use ui::TextRole;
use ui::widget::ButtonRole;

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
    /// The host admits this typed operation and owns its completion.
    RequestWifi(bool),
    /// A synchronous radio mutation succeeded; refresh authoritative OS state.
    Reconcile,
    ActionFailed {
        action: String,
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

    const BACK_FOCUS: usize = 0;
    const WIFI_FOCUS: usize = 1;
    const RESCAN_FOCUS: usize = 2;
    const HOTSPOT_FOCUS: usize = 3;
    const NETWORK_FOCUS_START: usize = 4;

    fn back_rect(panel: Rect) -> Rect {
        Rect::new(
            panel.x0 + 8.0,
            panel.y0 + 8.0,
            panel.x0 + 40.0,
            panel.y0 + 40.0,
        )
    }

    fn wifi_toggle_rect(panel: Rect) -> Rect {
        Rect::new(
            panel.x1 - 48.0,
            panel.y0 + 14.0,
            panel.x1 - 12.0,
            panel.y0 + 34.0,
        )
    }

    fn rescan_rect(panel: Rect) -> Rect {
        Rect::new(
            panel.x1 - 88.0,
            panel.y0 + 8.0,
            panel.x1 - 52.0,
            panel.y0 + 40.0,
        )
    }

    fn hotspot_rect(panel: Rect) -> Rect {
        Self::hotspot_rect_for(panel)
    }

    fn displayed_network_count(state: &QuickSettingsState) -> usize {
        if state.wifi && !state.wifi_scanning && state.wifi_scan_error.is_none() {
            state.wifi_networks.len()
        } else {
            0
        }
    }

    fn status_message(state: &QuickSettingsState) -> Option<String> {
        let key = if state.radio_change_pending {
            "display-confirm-applying"
        } else if !state.wifi {
            "qs-wifi-off"
        } else if state.wifi_scanning {
            "qs-wifi-scanning"
        } else if state.wifi_scan_error.is_some() {
            "qs-wifi-scan-failed"
        } else if state.wifi_networks.is_empty() {
            "qs-wifi-none"
        } else {
            return None;
        };
        Some(t(key))
    }

    /// Stable visual ids that are currently actionable. Disabled controls and
    /// an already-connected network remain visible, but keyboard and
    /// accessibility traversal must not land on inert targets.
    fn focusable_indices(state: &QuickSettingsState) -> Vec<usize> {
        let busy = state.wifi_joining.is_some() || state.radio_change_pending;
        let mut indices = vec![Self::BACK_FOCUS];
        if !busy {
            indices.push(Self::WIFI_FOCUS);
        }
        if state.wifi && !busy && !state.wifi_scanning {
            indices.push(Self::RESCAN_FOCUS);
        }
        if state.wifi && !busy {
            indices.push(Self::HOTSPOT_FOCUS);
            indices.extend(
                state
                    .wifi_networks
                    .iter()
                    .take(Self::displayed_network_count(state))
                    .enumerate()
                    .filter_map(|(index, network)| {
                        (!network.connected)
                            .then_some(Self::NETWORK_FOCUS_START.saturating_add(index))
                    }),
            );
        }
        indices
    }

    /// Consume a request raised by the header refresh button or by enabling
    /// Wi-Fi in this pane. The overlay host owns the existing off-thread scan,
    /// so the retained pane only publishes the intent.
    pub fn take_rescan_request(&mut self) -> bool {
        std::mem::take(&mut self.rescan_requested)
    }

    /// Named pane-owned actions for test control and future accessibility
    /// projection. Network rows remain data-dependent and are owned by the
    /// list model rather than receiving unstable index-only identities here.
    #[cfg(test)]
    pub fn action_rects(panel: Rect) -> [(&'static str, Rect); 3] {
        [
            ("quick_settings_wifi_toggle", Self::wifi_toggle_rect(panel)),
            ("quick_settings_wifi_rescan", Self::rescan_rect(panel)),
            ("quick_settings_wifi_hotspot", Self::hotspot_rect(panel)),
        ]
    }

    /// Every currently visible action in visual and keyboard order.
    pub fn semantic_rects(&self, panel: Rect, state: &QuickSettingsState) -> Vec<(String, Rect)> {
        let visible =
            self.list
                .visible_rects(panel, Self::displayed_network_count(state), Self::LIST_TOP);
        Self::focusable_indices(state)
            .into_iter()
            .filter_map(|index| {
                let (name, rect) = match index {
                    Self::BACK_FOCUS => ("quick_settings_wifi_back".into(), Self::back_rect(panel)),
                    Self::WIFI_FOCUS => (
                        "quick_settings_wifi_toggle".into(),
                        Self::wifi_toggle_rect(panel),
                    ),
                    Self::RESCAN_FOCUS => (
                        "quick_settings_wifi_rescan".into(),
                        Self::rescan_rect(panel),
                    ),
                    Self::HOTSPOT_FOCUS => (
                        "quick_settings_wifi_hotspot".into(),
                        Self::hotspot_rect(panel),
                    ),
                    network => {
                        let network = network.checked_sub(Self::NETWORK_FOCUS_START)?;
                        let rect = visible
                            .iter()
                            .find_map(|(index, rect)| (*index == network).then_some(*rect))?;
                        (format!("quick_settings_wifi_network[{network}]"), rect)
                    }
                };
                Some((name, rect))
            })
            .collect()
    }

    pub fn accessibility_rect_for_path(
        &self,
        panel: Rect,
        path: &[usize],
        state: &QuickSettingsState,
    ) -> Option<Rect> {
        let [0, child] = path else {
            return None;
        };
        let index = *Self::focusable_indices(state).get(*child)?;
        match index {
            Self::BACK_FOCUS => Some(Self::back_rect(panel)),
            Self::WIFI_FOCUS => Some(Self::wifi_toggle_rect(panel)),
            Self::RESCAN_FOCUS => Some(Self::rescan_rect(panel)),
            Self::HOTSPOT_FOCUS => Some(Self::hotspot_rect(panel)),
            network => {
                let network = network.checked_sub(Self::NETWORK_FOCUS_START)?;
                self.list
                    .visible_rects(panel, Self::displayed_network_count(state), Self::LIST_TOP)
                    .into_iter()
                    .find_map(|(index, rect)| (index == network).then_some(rect))
            }
        }
    }

    pub fn accessibility_snapshot(&self, state: &QuickSettingsState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let focusable = Self::focusable_indices(state);
        let children = focusable
            .iter()
            .filter_map(|index| match *index {
                Self::BACK_FOCUS => Some(
                    AccessNode::new(Role::Button)
                        .with_name(t("qs-back"))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.keyboard_focus == Some(*index)),
                        ),
                ),
                Self::WIFI_FOCUS => Some(
                    AccessNode::new(Role::Switch)
                        .with_name(t("qs-wifi"))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.keyboard_focus == Some(*index))
                                .checked(state.wifi),
                        ),
                ),
                Self::RESCAN_FOCUS => Some(
                    AccessNode::new(Role::Button)
                        .with_name(t("qs-wifi-rescan-a11y"))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.keyboard_focus == Some(*index)),
                        ),
                ),
                Self::HOTSPOT_FOCUS => Some(
                    AccessNode::new(Role::Switch)
                        .with_name(t("qs-wifi-hotspot-a11y"))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.keyboard_focus == Some(*index))
                                .checked(state.radios.hotspot_active),
                        ),
                ),
                network => {
                    let network = network.checked_sub(Self::NETWORK_FOCUS_START)?;
                    let item = state.wifi_networks.get(network)?;
                    Some(
                        AccessNode::new(Role::Button)
                            .with_name(item.ssid.clone())
                            .with_description(t(if item.secure {
                                "qs-wifi-secured"
                            } else {
                                "qs-wifi-open"
                            }))
                            .with_position(network.saturating_add(1), state.wifi_networks.len())
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.keyboard_focus == Some(*index)),
                            ),
                    )
                }
            })
            .collect();
        AccessNode::new(Role::Dialog)
            .with_name(t("qs-wifi"))
            .with_description(
                Self::status_message(state).unwrap_or_else(|| t("qs-wifi-choose-a11y")),
            )
            .with_state(AccessState::new().modal(true))
            .with_children(children)
    }

    pub fn reset_focus(&mut self) {
        self.keyboard_focus = None;
    }

    pub fn focus_accessibility_path(
        &mut self,
        path: &[usize],
        panel: Rect,
        state: &QuickSettingsState,
    ) -> bool {
        let [0, index] = path else {
            return false;
        };
        let Some(focus) = Self::focusable_indices(state).get(*index).copied() else {
            return false;
        };
        self.keyboard_focus = Some(focus);
        if let Some(network) = focus.checked_sub(Self::NETWORK_FOCUS_START) {
            self.list.ensure_visible(
                panel,
                Self::displayed_network_count(state),
                Self::LIST_TOP,
                network,
            );
        }
        true
    }

    pub fn focus_next(&mut self, panel: Rect, state: &QuickSettingsState, backwards: bool) {
        let order = Self::focusable_indices(state);
        let current = self
            .keyboard_focus
            .and_then(|focus| order.iter().position(|candidate| *candidate == focus));
        let last = order.len().saturating_sub(1);
        let position = match (current, backwards) {
            (Some(position), true) => position.checked_sub(1).unwrap_or(last),
            (Some(position), false) => position
                .saturating_add(1)
                .checked_rem(order.len())
                .unwrap_or_default(),
            (None, true) => last,
            (None, false) => 0,
        };
        let Some(focus) = order.get(position).copied() else {
            return;
        };
        self.keyboard_focus = Some(focus);
        if let Some(network) = focus.checked_sub(Self::NETWORK_FOCUS_START) {
            self.list.ensure_visible(
                panel,
                Self::displayed_network_count(state),
                Self::LIST_TOP,
                network,
            );
        }
    }

    pub fn scroll_wheel(&mut self, panel: Rect, state: &QuickSettingsState, rows: f64) -> bool {
        self.list.scroll_wheel(
            panel,
            Self::displayed_network_count(state),
            Self::LIST_TOP,
            rows,
        )
    }

    pub fn activate_focused(
        &mut self,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> WifiPaneOutcome {
        let Some(index) = self.keyboard_focus else {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
        };
        let point = match index {
            Self::BACK_FOCUS => Self::back_rect(panel).center(),
            Self::WIFI_FOCUS => Self::wifi_toggle_rect(panel).center(),
            Self::RESCAN_FOCUS => Self::rescan_rect(panel).center(),
            Self::HOTSPOT_FOCUS => Self::hotspot_rect(panel).center(),
            network => {
                self.list
                    .layout_from(panel, Self::displayed_network_count(state), Self::LIST_TOP);
                let network = network.saturating_sub(Self::NETWORK_FOCUS_START);
                self.list.ensure_visible(
                    panel,
                    Self::displayed_network_count(state),
                    Self::LIST_TOP,
                    network,
                );
                let Some(rect) = self.list.row_for(network) else {
                    return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
                };
                rect.center()
            }
        };
        self.handle_click(point, panel, state, actions)
    }

    /// Keyboard contract for host routing. Returning `Some` means the pane
    /// consumed the key, including focus-only movement whose outcome is
    /// `DrillIn(None)`.
    pub fn handle_key(
        &mut self,
        logical_key: &Key,
        backwards: bool,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> Option<WifiPaneOutcome> {
        match super::PaneKey::of(logical_key, backwards)? {
            super::PaneKey::Focus(backwards) => {
                self.focus_next(panel, state, backwards);
                Some(WifiPaneOutcome::DrillIn(DrillInOutcome::None))
            }
            super::PaneKey::Activate => Some(self.activate_focused(panel, state, actions)),
        }
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
        actions: &dyn QsActions,
    ) -> WifiPaneOutcome {
        self.list
            .layout_from(panel, Self::displayed_network_count(state), Self::LIST_TOP);
        // The same geometry paint uses, so a click before the first frame
        // still lands on the row that will be drawn.
        if self.list.back_contains(point) {
            return WifiPaneOutcome::DrillIn(DrillInOutcome::Back);
        }

        let busy = state.wifi_joining.is_some() || state.radio_change_pending;
        if Self::wifi_toggle_rect(panel).contains(point) {
            if busy {
                return WifiPaneOutcome::DrillIn(DrillInOutcome::None);
            }
            let next = !state.wifi;
            // Preparation is pure: the owning host starts the same typed
            // operation and keeps its receipt. Explicit synchronous fixture
            // dispatchers retain their original immediate contract.
            if actions
                .prepare_radio_change(os_actions::RadioChange::Wifi(next))
                .is_some()
            {
                return WifiPaneOutcome::RequestWifi(next);
            }
            let result = actions.set_wifi(next);
            if let Err(error) = result {
                log_qs_result("set_wifi", Err(error.clone()));
                return WifiPaneOutcome::ActionFailed {
                    action: t(if next {
                        "qs-wifi-turn-on"
                    } else {
                        "qs-wifi-turn-off"
                    }),
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
                    action: t(if next {
                        "qs-wifi-hotspot-turn-on"
                    } else {
                        "qs-wifi-hotspot-turn-off"
                    }),
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
        let busy = state.wifi_joining.is_some() || state.radio_change_pending;
        let displayed_networks = Self::displayed_network_count(state);
        self.list
            .layout_from(panel, displayed_networks, Self::LIST_TOP);
        let colors = &theme.tokens.colors;
        self.list
            .draw_header(scene, transform, text, theme, panel, cursor, &t("qs-wifi"));
        // The Hotspot row is a Control (HIG §9.1/§9.5): glyph, two-line
        // label and switch all on its centre line and the shared inset.
        let row = Self::hotspot_rect_for(panel);
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
        let label_style = theme.text(TextRole::Caption);
        let detail_style = theme.text(TextRole::Caption);
        let detail = t(if state.radios.hotspot_active {
            "qs-wifi-hotspot-sharing"
        } else {
            "qs-off"
        });
        let hotspot_label = t("qs-wifi-hotspot");
        let (_, label_h) = text.measure_style(
            &hotspot_label,
            None,
            &label_style
                .clone()
                .with_weight(ui::text_render::TextWeight::Medium),
        );
        let (_, detail_h) = text.measure_style(&detail, None, &detail_style);
        let block_top = centred_text_top(row, f64::from(label_h) + f64::from(detail_h));
        let text_x = glyph.x1 + CONTROL_INSET;
        text.draw_style(
            scene,
            &hotspot_label,
            colors.text_primary,
            transform.pre_translate((text_x, block_top).into()),
            None,
            &label_style.with_weight(ui::text_render::TextWeight::Medium),
        );
        text.draw_style(
            scene,
            &detail,
            colors.text_dim,
            transform.pre_translate((text_x, block_top + f64::from(label_h)).into()),
            None,
            &detail_style,
        );
        Toggle::paint(
            scene,
            transform,
            theme,
            &ToggleFace::new(
                trailing_toggle(row, CONTROL_INSET, TOGGLE_W, TOGGLE_H),
                state.radios.hotspot_active,
            )
            .hovered(hovered)
            .disabled(busy),
        );
        if let Some(message) = Self::status_message(state) {
            DrillInList::draw_empty_from(
                scene,
                transform,
                text,
                theme,
                panel,
                &message,
                Self::LIST_TOP,
            );
            if displayed_networks == 0 {
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
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                *row,
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
                        .role(ButtonRole::Plain)
                        .focused(true)
                        .disabled(network.connected || busy)
                        .accessible_name(&network.ssid),
                );
            }
            if state.wifi_joining.as_deref() == Some(network.ssid.as_str()) {
                text.draw_style(
                    scene,
                    &t("qs-connecting"),
                    colors.intent.inverted,
                    transform.pre_translate((row.x0 + 44.0, row.y0 + 38.0).into()),
                    Some(f32v((row.width() - 120.0).max(1.0))),
                    &theme.text(TextRole::Caption),
                );
            } else if network.connected {
                text.draw_style(
                    scene,
                    &t("qs-connected"),
                    colors.intent.inverted,
                    transform.pre_translate((row.x0 + 44.0, row.y0 + 38.0).into()),
                    Some(f32v((row.width() - 120.0).max(1.0))),
                    &theme.text(TextRole::Caption),
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
                    colors.intent.inverted,
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
    use winit::keyboard::NamedKey;

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
            security: if secure {
                os_actions::WifiSecurity::Wpa2
            } else {
                os_actions::WifiSecurity::Open
            },
            connected: false,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wifi_rows_join_only_joinable_networks_exactly_once() {
        // Phase `protected_and_open_rows_take_distinct_paths`.
        {
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

        // Phase `back_and_empty_space_never_join_a_network`.
        {
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

        // Phase `an_in_flight_open_network_join_is_not_dispatched_twice`.
        {
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

        // Phase `connected_rows_are_inert_instead_of_rejoining`.
        {
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

        // Phase `any_in_flight_join_blocks_every_other_network`.
        {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wifi_radio_refresh_and_status_tell_the_truth() {
        // Phase `header_toggle_changes_radio_only_after_a_successful_action`.
        {
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
                WifiPaneOutcome::ActionFailed { ref action, .. } if action == "Turn on Wi-Fi"
            ));
            assert!(
                !rejected_state.wifi,
                "a failed OS write must not lie in the UI"
            );
            assert!(!pane.take_rescan_request());
        }

        // Phase `refresh_is_named_and_requests_one_host_owned_scan`.
        {
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

        // Phase `wifi_off_hides_stale_rows_and_disables_refresh`.
        {
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

        // Phase `off_loading_empty_and_error_have_distinct_truthful_status`.
        {
            let mut state = QuickSettingsState {
                wifi: false,
                ..Default::default()
            };
            assert_eq!(
                QuickSettingsWifiPane::status_message(&state).as_deref(),
                Some("Wi-Fi is off")
            );

            state.wifi = true;
            state.wifi_scanning = true;
            assert_eq!(
                QuickSettingsWifiPane::status_message(&state).as_deref(),
                Some("Scanning for networks…")
            );

            state.wifi_scanning = false;
            assert_eq!(
                QuickSettingsWifiPane::status_message(&state).as_deref(),
                Some("No networks found")
            );

            state.wifi_scan_error = Some("NetworkManager unavailable".into());
            assert_eq!(
                QuickSettingsWifiPane::status_message(&state).as_deref(),
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wifi_keyboard_and_semantics_match_the_visible_actionable_rows() {
        // Phase `keyboard_traverses_visible_actions_and_activates_the_focused_row`.
        {
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

        // Phase `semantic_projection_matches_the_visible_pane`.
        {
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

        // Phase `traversal_excludes_disabled_controls_and_connected_rows`.
        {
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

        // Phase `compact_wheel_exposes_only_the_scrolled_network_window`.
        {
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
    }
}
