//! Compact, configurable root page for the Quick Settings popover.
//!
//! The panel presents current controls, not a dashboard of every capability.
//! Arrangement is durable user intent while hardware availability is applied
//! only when the panel is rendered.

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::quick_settings::{PowerProfile, QsLevel};
use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::drag::{DragController, ListMetrics};
use ui::widget::{Button, ButtonFace, Slider, SliderFace, Toggle, ToggleFace};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;

fn n(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}
fn f32v(value: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as f32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootAction {
    None,
    OpenSettings,
    Privacy,
    OpenNotifications,
    OpenPowerMenu,
    Enter(QsLevel),
    Changed(PendingRootActions),
    LayoutChanged,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingRootActions {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
    pub wired: Option<bool>,
    pub airplane: Option<bool>,
    pub hotspot: Option<bool>,
    pub mobile: Option<bool>,
    pub night_light: Option<bool>,
    pub power: Option<PowerProfile>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliderCommit {
    Brightness(f32),
    Volume(f32),
}

/// Root interaction state.  Editing uses the shared qOS reorder controller;
/// it never serialises paint geometry, only stable control identifiers.
pub struct QuickSettingsRoot {
    panel: Rect,
    // Text scale changes the amount of vertical room a two-line control
    // needs.  The host supplies this before it resolves the popover rect,
    // so large-text screenshots and the live frame use the same geometry.
    layout_scale: f64,
    slider_drag: Option<usize>,
    keyboard_focus: Option<usize>,
    config: QuickSettingsConfig,
    // Hosts that inject a model (tests, screenshots and embedders) must not
    // silently rewrite the desktop user's persisted arrangement. Only the
    // normal session constructor owns that file.
    persist_config: bool,
    edit_mode: bool,
    edit_drag: DragController<QuickSettingsControl>,
}

impl Default for QuickSettingsRoot {
    /// Deterministic for tests. Production construction calls
    /// [`Self::load_persisted`].
    fn default() -> Self {
        Self::with_config(QuickSettingsConfig::default())
    }
}

impl QuickSettingsRoot {
    const PAD: f64 = 16.0;
    // Root-page rhythm: every major section is separated by 8px, and every
    // control has a 16px leading/trailing inset. These are intentionally
    // shared rather than nudged per widget so 1:1 rendering stays aligned.
    const HEADER: f64 = 40.0;
    // Utilities deliberately remain in a labelled top action area.  A
    // footer of bare glyphs made core actions (especially Power) hard to
    // discover and visually disconnected from the controls they affect.
    // The compact icon-over-label treatment keeps their meaning visible
    // without turning them into a second dashboard.
    const ACTIONS: f64 = 58.0;
    const SLIDERS: f64 = 72.0;
    const CARD: f64 = 70.0;
    const GAP: f64 = 8.0;
    const POWER: f64 = 50.0;
    const EDIT_ROW: f64 = 50.0;
    const EDIT_HELP: f64 = 22.0;
    const EDIT_FOOTER: f64 = 38.0;

    pub fn load_persisted() -> Self {
        Self::from_config(QuickSettingsConfig::load(), true)
    }
    /// Construct a deterministic, non-persisting root. This is for an
    /// embedding host that supplies its own model; production sessions use
    /// [`Self::load_persisted`] and are the only ones permitted to save.
    pub fn with_config(config: QuickSettingsConfig) -> Self {
        Self::from_config(config, false)
    }
    fn from_config(config: QuickSettingsConfig, persist_config: bool) -> Self {
        Self {
            panel: Rect::ZERO,
            layout_scale: 1.0,
            slider_drag: None,
            keyboard_focus: None,
            config: config.normalised(),
            persist_config,
            edit_mode: false,
            edit_drag: DragController::vertical(),
        }
    }
    pub fn reset_focus(&mut self) {
        self.keyboard_focus = Some(0);
    }
    pub fn set_layout_text_scale(&mut self, scale: f32) {
        // Text itself follows the user's full scale.  The surrounding shell
        // geometry deliberately stops growing at 120%: scaling every pad,
        // gap and card to 130% or 200% pushes the last essential control
        // below an 800×600 desktop even though its label still fits.  The
        // retained 20% row headroom keeps the two-line cards legible while
        // preserving the whole popover as a single, usable panel.
        self.layout_scale = f64::from(scale).clamp(1.0, 1.2);
    }
    fn scaled(&self, value: f64) -> f64 {
        value * self.layout_scale
    }
    #[cfg(test)]
    pub fn config(&self) -> &QuickSettingsConfig {
        &self.config
    }
    pub fn edit_mode(&self) -> bool {
        self.edit_mode
    }
    /// Deterministic state injection for the production-faithful screenshot
    /// driver. This does not participate in normal compositor input.
    pub fn set_edit_mode_for_visual_test(&mut self, edit_mode: bool) {
        self.edit_mode = edit_mode;
        self.keyboard_focus = Some(0);
    }
    /// Show the same shared drag-controller reflow/ghost state that pointer
    /// input reaches, without making a screenshot depend on wall-clock input.
    pub fn start_edit_drag_for_visual_test(&mut self, panel: Rect) {
        self.panel = panel;
        self.edit_mode = true;
        let Some(control) = self.config.controls.get(1).copied() else {
            return;
        };
        let row = self.card(panel, 1, true);
        let start = Point::new(row.x0 + 20.0, row.center().y);
        self.edit_drag.begin(control, 1, start.y - row.y0, start);
        let metrics = ListMetrics::uniform(
            self.edit_grid_top(panel),
            self.scaled(Self::EDIT_ROW),
            self.scaled(6.0),
            self.config.controls.len(),
        );
        let _ = self
            .edit_drag
            .pointer_moved(Point::new(start.x, start.y + 76.0), &metrics);
    }

    fn visible_controls(&self, state: &QuickSettingsState) -> Vec<QuickSettingsControl> {
        self.config
            .controls
            .iter()
            .copied()
            .filter(|c| match c {
                // `Wifi` is the durable identifier of the required recovery
                // slot. It renders as Wi-Fi when a radio exists and as a
                // wired-network affordance on Ethernet-only machines; no
                // network hardware means no inert root tile.
                QuickSettingsControl::Wifi => {
                    state.radios.wifi_available
                        || state
                            .wired
                            .as_ref()
                            .is_some_and(|wired| wired.device.is_some())
                }
                // Powered off and unavailable are distinct states. Only
                // BlueZ's successful adapter read may expose this switch.
                QuickSettingsControl::Bluetooth => state.bluetooth_available,
                QuickSettingsControl::NightLight => true,
                QuickSettingsControl::Vpn => !state.vpns.is_empty(),
                QuickSettingsControl::AirplaneMode => true,
                QuickSettingsControl::MobileData => state.radios.wwan_available,
            })
            .collect()
    }
    /// Controls usable on this machine right now. Persisted optional controls
    /// are deliberately not shown as addable when the backing capability is
    /// absent; if hardware appears later the saved preference remains intact.
    fn edit_controls(&self, state: &QuickSettingsState) -> Vec<QuickSettingsControl> {
        let mut all = self.config.controls.clone();
        for c in [
            QuickSettingsControl::Wifi,
            QuickSettingsControl::Bluetooth,
            QuickSettingsControl::Vpn,
            QuickSettingsControl::NightLight,
            QuickSettingsControl::AirplaneMode,
            QuickSettingsControl::MobileData,
        ] {
            if !all.contains(&c) {
                all.push(c);
            }
        }
        all.into_iter()
            .filter(|c| match c {
                QuickSettingsControl::Wifi => true,
                QuickSettingsControl::Bluetooth => state.bluetooth_available,
                QuickSettingsControl::Vpn => !state.vpns.is_empty(),
                QuickSettingsControl::NightLight => true,
                QuickSettingsControl::AirplaneMode => {
                    state.radios.wifi_available || state.radios.wwan_available
                }
                QuickSettingsControl::MobileData => state.radios.wwan_available,
            })
            .collect()
    }
    #[cfg(test)]
    pub fn control_count(&self, state: &QuickSettingsState) -> usize {
        self.visible_controls(state).len()
    }
    pub fn preferred_height(&self, state: &QuickSettingsState) -> f64 {
        let count = if self.edit_mode {
            self.edit_controls(state).len()
        } else {
            self.visible_controls(state).len()
        };
        let rows = if self.edit_mode {
            count
        } else {
            count.div_ceil(2)
        };
        if self.edit_mode {
            self.scaled(
                Self::PAD * 2.0
                    + Self::HEADER
                    + Self::EDIT_HELP
                    + Self::GAP * 2.0
                    + n(rows) * Self::EDIT_ROW
                    + n(rows.saturating_sub(1)) * 6.0
                    + Self::EDIT_FOOTER,
            )
        } else {
            self.scaled(
                Self::PAD * 2.0
                    + Self::HEADER
                    + Self::ACTIONS
                    + Self::SLIDERS
                    + Self::POWER
                    + Self::GAP * 4.0
                    + n(rows) * Self::CARD
                    + n(rows.saturating_sub(1)) * Self::GAP,
            )
        }
    }
    fn save_config(&mut self) {
        self.config = self.config.clone().normalised();
        if !self.persist_config {
            // Interaction tests and embedded previews exercise the same
            // model, but never write a developer's real desktop settings.
            // Their caller owns persistence, if any.
            return;
        }
        if let Err(error) = self.config.save() {
            tracing::warn!(%error, "could not persist Quick Settings layout");
        }
    }
    fn grid_top(&self, panel: Rect) -> f64 {
        panel.y0
            + self.scaled(Self::PAD)
            + self.scaled(Self::HEADER)
            + self.scaled(Self::GAP)
            + self.scaled(Self::ACTIONS)
            + self.scaled(Self::GAP)
            + self.scaled(Self::SLIDERS)
            + self.scaled(Self::GAP)
    }
    fn edit_grid_top(&self, panel: Rect) -> f64 {
        panel.y0
            + self.scaled(Self::PAD)
            + self.scaled(Self::HEADER)
            + self.scaled(Self::EDIT_HELP)
            + self.scaled(Self::GAP)
    }
    fn card(&self, panel: Rect, i: usize, edit: bool) -> Rect {
        let top = self.grid_top(panel);
        if edit {
            let top = self.edit_grid_top(panel);
            let y = top + n(i) * self.scaled(Self::EDIT_ROW + 6.0);
            return Rect::new(
                panel.x0 + self.scaled(Self::PAD),
                y,
                panel.x1 - self.scaled(Self::PAD),
                y + self.scaled(Self::EDIT_ROW),
            );
        }
        let col = i % 2;
        let row = i / 2;
        let pad = self.scaled(Self::PAD);
        let gap = self.scaled(Self::GAP);
        let w = (panel.width() - 2.0 * pad - gap) / 2.0;
        let x = panel.x0 + pad + n(col) * (w + gap);
        let y = top + n(row) * self.scaled(Self::CARD + Self::GAP);
        Rect::new(x, y, x + w, y + self.scaled(Self::CARD))
    }
    fn control_card(&self, panel: Rect, i: usize, count: usize) -> Rect {
        let row = i / 2;
        let pad = self.scaled(Self::PAD);
        let gap = self.scaled(Self::GAP);
        let card = self.scaled(Self::CARD);
        let y = self.grid_top(panel) + n(row) * (card + gap);
        if count % 2 == 1 && i + 1 == count {
            return Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + card);
        }
        let w = (panel.width() - 2.0 * pad - gap) / 2.0;
        let x = panel.x0 + pad + n(i % 2) * (w + gap);
        Rect::new(x, y, x + w, y + card)
    }
    /// Compatibility geometry for integration tests; the renderer uses the
    /// current control vector above.
    pub fn toggle_geometry(panel: Rect, index: usize) -> Rect {
        Self::default().card(panel, index, false)
    }
    pub fn slider_geometry(panel: Rect, i: usize) -> (Rect, f64) {
        Self::slider_geometry_scaled(panel, i, 1.0)
    }
    fn slider_geometry_for(&self, panel: Rect, i: usize) -> (Rect, f64) {
        Self::slider_geometry_scaled(panel, i, self.layout_scale)
    }
    fn slider_geometry_scaled(panel: Rect, i: usize, scale: f64) -> (Rect, f64) {
        let y = panel.y0
            + Self::PAD * scale
            + Self::HEADER * scale
            + Self::GAP * scale
            + Self::ACTIONS * scale
            + Self::GAP * scale
            + 12.0 * scale
            + n(i) * 28.0 * scale;
        (
            Rect::new(
                panel.x0 + 52.0 * scale,
                y + 8.0 * scale,
                panel.x1 - 28.0 * scale,
                y + 14.0 * scale,
            ),
            y,
        )
    }
    pub(crate) fn power_geometry(&self, panel: Rect, state: &QuickSettingsState) -> Rect {
        let count = if self.edit_mode {
            self.edit_controls(state).len()
        } else {
            self.visible_controls(state).len()
        };
        let rows = if self.edit_mode {
            count
        } else {
            count.div_ceil(2)
        };
        let y = self.grid_top(panel)
            + n(rows) * self.scaled(Self::CARD)
            + n(rows.saturating_sub(1)) * self.scaled(Self::GAP)
            + self.scaled(Self::GAP);
        Rect::new(
            panel.x0 + self.scaled(Self::PAD),
            y,
            panel.x1 - self.scaled(Self::PAD),
            y + self.scaled(Self::POWER),
        )
    }
    fn action(&self, i: usize) -> Rect {
        let pad = self.scaled(Self::PAD);
        let gap = self.scaled(Self::GAP);
        let w = (self.panel.width() - 2.0 * pad - 3.0 * gap) / 4.0;
        let x = self.panel.x0 + pad + n(i) * (w + gap);
        let y = self.panel.y0 + pad + self.scaled(Self::HEADER);
        Rect::new(x, y, x + w, y + self.scaled(Self::ACTIONS))
    }
    fn edit_rect(&self) -> Rect {
        Rect::new(
            self.panel.x1 - self.scaled(Self::PAD + 32.0),
            self.panel.y0 + self.scaled(Self::PAD + 4.0),
            self.panel.x1 - self.scaled(Self::PAD),
            self.panel.y0 + self.scaled(Self::PAD + 36.0),
        )
    }
    fn label(c: QuickSettingsControl) -> &'static str {
        match c {
            QuickSettingsControl::Wifi => "Network",
            QuickSettingsControl::Bluetooth => "Bluetooth",
            QuickSettingsControl::Vpn => "VPN",
            QuickSettingsControl::NightLight => "Night Light",
            QuickSettingsControl::AirplaneMode => "Airplane mode",
            QuickSettingsControl::MobileData => "Mobile data",
        }
    }
    fn rendered_label(c: QuickSettingsControl, s: &QuickSettingsState) -> &'static str {
        match c {
            QuickSettingsControl::Wifi if !s.radios.wifi_available => "Wired network",
            QuickSettingsControl::Wifi => "Wi-Fi",
            _ => Self::label(c),
        }
    }
    fn icon(c: QuickSettingsControl, s: &QuickSettingsState) -> ui::icons::Icon {
        match c {
            QuickSettingsControl::Wifi if !s.radios.wifi_available => ui::icons::Icon::EthernetPort,
            QuickSettingsControl::Wifi => ui::icons::Icon::Wifi,
            QuickSettingsControl::Bluetooth => ui::icons::Icon::Bluetooth,
            QuickSettingsControl::Vpn => ui::icons::Icon::Shield,
            QuickSettingsControl::NightLight => ui::icons::Icon::Sun,
            QuickSettingsControl::AirplaneMode => ui::icons::Icon::WifiOff,
            QuickSettingsControl::MobileData => ui::icons::Icon::Globe,
        }
    }
    fn on(c: QuickSettingsControl, s: &QuickSettingsState) -> bool {
        match c {
            QuickSettingsControl::Wifi if !s.radios.wifi_available => {
                s.wired.as_ref().is_some_and(|wired| wired.connected)
            }
            QuickSettingsControl::Wifi => s.wifi,
            QuickSettingsControl::Bluetooth => s.bluetooth,
            QuickSettingsControl::Vpn => s.vpns.iter().any(|v| v.active),
            QuickSettingsControl::NightLight => s.night_light,
            QuickSettingsControl::AirplaneMode => s.radios.airplane_mode,
            QuickSettingsControl::MobileData => s.radios.wwan_enabled,
        }
    }
    fn subline<'a>(c: QuickSettingsControl, s: &'a QuickSettingsState) -> &'a str {
        match c {
            QuickSettingsControl::Wifi if !s.radios.wifi_available => s
                .wired
                .as_ref()
                .and_then(|wired| wired.device.as_deref())
                .map_or("Not connected", |device| {
                    if s.wired.as_ref().is_some_and(|wired| wired.connected) {
                        device
                    } else {
                        "Cable unplugged"
                    }
                }),
            QuickSettingsControl::Wifi if s.wifi => s
                .wifi_networks
                .iter()
                .find(|v| v.connected)
                .map_or("Not connected", |v| v.ssid.as_str()),
            QuickSettingsControl::Wifi => "Off",
            QuickSettingsControl::Bluetooth if s.bluetooth => s
                .bt_devices
                .iter()
                .find(|v| v.connected)
                .map_or("Not connected", |v| v.name.as_str()),
            QuickSettingsControl::Bluetooth => "Off",
            QuickSettingsControl::Vpn => s
                .vpns
                .iter()
                .find(|v| v.active)
                .or_else(|| s.vpns.first())
                .map_or("No profiles", |v| v.name.as_str()),
            QuickSettingsControl::NightLight => {
                if s.night_light {
                    "On"
                } else {
                    "Off"
                }
            }
            QuickSettingsControl::AirplaneMode => {
                if s.radios.airplane_mode {
                    "On"
                } else {
                    "Off"
                }
            }
            QuickSettingsControl::MobileData => {
                if s.radios.wwan_enabled {
                    "On"
                } else {
                    "Off"
                }
            }
        }
    }
    fn toggle(
        c: QuickSettingsControl,
        s: &mut QuickSettingsState,
        pending: &mut PendingRootActions,
    ) -> Option<RootAction> {
        match c {
            QuickSettingsControl::Wifi => {
                if s.radios.wifi_available {
                    s.wifi = !s.wifi;
                    pending.wifi = Some(s.wifi);
                } else if let Some(wired) = &mut s.wired {
                    wired.connected = !wired.connected;
                    pending.wired = Some(wired.connected);
                }
            }
            QuickSettingsControl::Bluetooth => {
                s.bluetooth = !s.bluetooth;
                pending.bluetooth = Some(s.bluetooth);
            }
            QuickSettingsControl::Vpn => return Some(RootAction::Enter(QsLevel::Vpn)),
            QuickSettingsControl::NightLight => {
                s.night_light = !s.night_light;
                pending.night_light = Some(s.night_light);
            }
            QuickSettingsControl::AirplaneMode => {
                s.radios.airplane_mode = !s.radios.airplane_mode;
                pending.airplane = Some(s.radios.airplane_mode);
            }
            QuickSettingsControl::MobileData => {
                s.radios.wwan_enabled = !s.radios.wwan_enabled;
                pending.mobile = Some(s.radios.wwan_enabled);
            }
        };
        None
    }

    pub fn focus_next(&mut self, state: &QuickSettingsState, backwards: bool) -> bool {
        let count = if self.edit_mode {
            self.edit_controls(state).len() + 3
        } else {
            // Edit, labeled destinations, Brightness, Volume, controls, power.
            8 + self.visible_controls(state).len()
        };
        self.keyboard_focus = Some(match (self.keyboard_focus, backwards) {
            (Some(i), true) => i.checked_sub(1).unwrap_or(count - 1),
            (Some(i), false) => (i + 1) % count,
            (None, true) => count - 1,
            (None, false) => 0,
        });
        true
    }
    pub fn activate_focused(&mut self, panel: Rect, state: &mut QuickSettingsState) -> RootAction {
        self.panel = panel;
        let Some(i) = self.keyboard_focus else {
            return RootAction::None;
        };
        if self.edit_mode {
            let all = self.edit_controls(state);
            if i == 0 {
                self.edit_mode = false;
                return RootAction::LayoutChanged;
            }
            if let Some(c) = all.get(i - 1).copied() {
                if !c.required() {
                    if self.config.controls.contains(&c) {
                        self.config.controls.retain(|x| *x != c);
                    } else {
                        self.config.controls.push(c);
                    }
                    self.save_config();
                    return RootAction::LayoutChanged;
                }
            }
            if i == all.len() + 1 {
                self.config = QuickSettingsConfig::default();
                self.save_config();
                return RootAction::LayoutChanged;
            }
            if i == all.len() + 2 {
                self.edit_mode = false;
                return RootAction::LayoutChanged;
            }
            return RootAction::None;
        }
        if i == 0 {
            self.edit_mode = true;
            return RootAction::LayoutChanged;
        }
        if let Some(action) = match i {
            1 => Some(RootAction::OpenSettings),
            2 => Some(RootAction::Privacy),
            3 => Some(RootAction::OpenNotifications),
            4 => Some(RootAction::OpenPowerMenu),
            _ => None,
        } {
            return action;
        }
        let controls = self.visible_controls(state);
        if matches!(i, 5 | 6) {
            return RootAction::None;
        }
        if let Some(c) = controls.get(i - 7).copied() {
            let mut p = PendingRootActions::default();
            return Self::toggle(c, state, &mut p).unwrap_or(RootAction::Changed(p));
        }
        let base = 7 + controls.len();
        if i == base {
            return self.cycle_power(state);
        }
        RootAction::None
    }
    pub fn reorder_focused(&mut self, state: &QuickSettingsState, direction: i8) -> bool {
        if !self.edit_mode {
            return false;
        }
        let all = self.edit_controls(state);
        let Some(i) = self
            .keyboard_focus
            .and_then(|i| i.checked_sub(1))
            .filter(|i| *i < all.len())
        else {
            return false;
        };
        let c = all[i];
        let Some(from) = self.config.controls.iter().position(|x| *x == c) else {
            return false;
        };
        let to = if direction < 0 {
            from.checked_sub(1)
        } else {
            (from + 1 < self.config.controls.len()).then_some(from + 1)
        };
        let Some(to) = to else {
            return false;
        };
        let c = self.config.controls.remove(from);
        self.config.controls.insert(to, c);
        self.keyboard_focus = Some(to + 1);
        self.save_config();
        true
    }
    pub fn adjust_focused_slider(
        &self,
        state: &mut QuickSettingsState,
        direction: f32,
    ) -> Option<SliderCommit> {
        let value = match self.keyboard_focus? {
            5 => &mut state.brightness,
            6 => &mut state.volume,
            _ => return None,
        };
        *value = (*value + direction * 0.05).clamp(0., 1.);
        Some(if self.keyboard_focus == Some(5) {
            SliderCommit::Brightness(*value * 100.)
        } else {
            SliderCommit::Volume(*value * 100.)
        })
    }
    fn cycle_power(&self, s: &mut QuickSettingsState) -> RootAction {
        s.power_profile = match s.power_profile {
            PowerProfile::Saver => PowerProfile::Balanced,
            PowerProfile::Balanced => PowerProfile::Performance,
            PowerProfile::Performance => PowerProfile::Saver,
        };
        RootAction::Changed(PendingRootActions {
            power: Some(s.power_profile),
            ..Default::default()
        })
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
    ) -> RootAction {
        self.panel = panel;
        if self.edit_rect().contains(point) {
            self.edit_mode = !self.edit_mode;
            self.keyboard_focus = Some(0);
            return RootAction::LayoutChanged;
        }
        if self.edit_mode {
            return self.edit_click(point, state);
        }
        for i in 0..4 {
            if self.action(i).contains(point) {
                return match i {
                    0 => RootAction::OpenSettings,
                    1 => RootAction::Privacy,
                    2 => RootAction::OpenNotifications,
                    _ => RootAction::OpenPowerMenu,
                };
            }
        }
        for i in 0..2 {
            let (bar, _) = self.slider_geometry_for(panel, i);
            if Rect::new(bar.x0 - 6., bar.y0 - 10., bar.x1 + 6., bar.y1 + 10.).contains(point) {
                let value = f32v(((point.x - bar.x0) / bar.width()).clamp(0., 1.));
                if i == 0 {
                    state.brightness = value;
                } else {
                    state.volume = value;
                }
                self.slider_drag = Some(i);
                return RootAction::Changed(PendingRootActions::default());
            }
        }
        let controls = self.visible_controls(state);
        for (i, c) in controls.iter().copied().enumerate() {
            let card = self.control_card(panel, i, controls.len());
            if !card.contains(point) {
                continue;
            }
            if (matches!(c, QuickSettingsControl::Wifi) && state.radios.wifi_available
                || matches!(
                    c,
                    QuickSettingsControl::Bluetooth | QuickSettingsControl::Vpn
                ))
                && Rect::new(card.x1 - 36., card.y0 + 8., card.x1 - 4., card.y0 + 40.)
                    .contains(point)
            {
                return RootAction::Enter(match c {
                    QuickSettingsControl::Wifi => QsLevel::Wifi,
                    QuickSettingsControl::Bluetooth => QsLevel::Bluetooth,
                    _ => QsLevel::Vpn,
                });
            }
            let mut p = PendingRootActions::default();
            return Self::toggle(c, state, &mut p).unwrap_or(RootAction::Changed(p));
        }
        if self.power_geometry(panel, state).contains(point) {
            return self.cycle_power(state);
        }
        RootAction::None
    }
    fn edit_reset(&self, state: &QuickSettingsState) -> Rect {
        let y = self.edit_grid_top(self.panel)
            + n(self.edit_controls(state).len()) * self.scaled(Self::EDIT_ROW + 6.)
            + self.scaled(Self::GAP);
        Rect::new(
            self.panel.x0 + self.scaled(Self::PAD),
            y,
            self.panel.center().x - self.scaled(4.),
            y + self.scaled(Self::EDIT_FOOTER),
        )
    }
    fn edit_done(&self, state: &QuickSettingsState) -> Rect {
        let reset = self.edit_reset(state);
        Rect::new(
            self.panel.center().x + self.scaled(4.),
            reset.y0,
            self.panel.x1 - self.scaled(Self::PAD),
            reset.y1,
        )
    }
    fn edit_click(&mut self, point: Point, state: &QuickSettingsState) -> RootAction {
        for (i, c) in self.edit_controls(state).iter().copied().enumerate() {
            let row = self.card(self.panel, i, true);
            if !row.contains(point) {
                continue;
            }
            let present = self.config.controls.contains(&c);
            if present
                && Rect::new(row.x0 + 8., row.y0 + 4., row.x0 + 38., row.y1 - 4.).contains(point)
            {
                self.edit_drag.begin(c, i, point.y - row.y0, point);
                return RootAction::None;
            }
            if !c.required() {
                if present {
                    self.config.controls.retain(|x| *x != c);
                } else {
                    self.config.controls.push(c);
                }
                self.save_config();
                return RootAction::LayoutChanged;
            }
            return RootAction::None;
        }
        if self.edit_reset(state).contains(point) {
            self.config = QuickSettingsConfig::default();
            self.save_config();
            return RootAction::LayoutChanged;
        }
        if self.edit_done(state).contains(point) {
            self.edit_mode = false;
            return RootAction::LayoutChanged;
        }
        RootAction::None
    }
    pub fn update_drag(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
    ) -> bool {
        self.panel = panel;
        if let Some(i) = self.slider_drag {
            let (bar, _) = self.slider_geometry_for(panel, i);
            let v = f32v(((point.x - bar.x0) / bar.width()).clamp(0., 1.));
            if i == 0 {
                state.brightness = v;
            } else {
                state.volume = v;
            }
            return true;
        }
        if self.edit_drag.is_pending() {
            return self.edit_drag.pointer_moved(
                point,
                &ListMetrics::uniform(
                    self.edit_grid_top(panel),
                    self.scaled(Self::EDIT_ROW),
                    self.scaled(6.),
                    self.edit_controls(state).len(),
                ),
            );
        }
        false
    }
    pub fn finish_drag(&mut self, state: &QuickSettingsState) -> Option<SliderCommit> {
        if let Some(i) = self.slider_drag.take() {
            return Some(if i == 0 {
                SliderCommit::Brightness(state.brightness * 100.)
            } else {
                SliderCommit::Volume(state.volume * 100.)
            });
        }
        if let Some((from, to)) = self.edit_drag.commit()
            && from < self.config.controls.len()
            && to < self.config.controls.len()
        {
            let c = self.config.controls.remove(from);
            self.config.controls.insert(to, c);
            self.save_config();
        }
        None
    }
    pub fn drag_active(&self) -> bool {
        self.slider_drag.is_some() || self.edit_drag.is_pending()
    }

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
        self.panel = panel;
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(panel, 18.),
        );
        self.header(scene, transform, text, theme, cursor);
        if self.edit_mode {
            self.draw_edit(scene, transform, text, theme, cursor, state);
        } else {
            self.draw_root(scene, transform, text, theme, cursor, state);
        }
    }
    fn header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
    ) {
        let colors = &theme.tokens.colors;
        text.draw_text_weighted(
            scene,
            if self.edit_mode {
                "Edit controls"
            } else {
                "Quick Settings"
            },
            theme.tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate(
                (
                    self.panel.x0 + self.scaled(Self::PAD),
                    self.panel.y0 + self.scaled(Self::PAD + 23.),
                )
                    .into(),
            ),
            None,
            TextWeight::Semibold,
        );
        for (r, icon, name) in [(
            self.edit_rect(),
            if self.edit_mode {
                ui::icons::Icon::Check
            } else {
                ui::icons::Icon::Pencil
            },
            if self.edit_mode {
                "Finish editing controls"
            } else {
                "Edit controls"
            },
        )] {
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(r, "")
                    .ghost()
                    .hovered(cursor.is_some_and(|p| r.contains(p)))
                    .accessible_name(name),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    r.x0 + self.scaled(8.),
                    r.y0 + self.scaled(8.),
                    r.x1 - self.scaled(8.),
                    r.y1 - self.scaled(8.),
                ),
                icon,
                colors.text_primary,
                1.5,
            );
        }
    }
    fn draw_root(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
        state: &QuickSettingsState,
    ) {
        let colors = &theme.tokens.colors;
        // Destinations remain at the top, in the place established by the
        // original GNOME-inspired shell, rather than becoming unexplained
        // footer glyphs.  The visible label is part of the control: a
        // tooltip or accessibility tree is not sufficient discoverability.
        for (i, (icon, accessible, label)) in [
            (ui::icons::Icon::Settings2, "Open Settings", "Settings"),
            (ui::icons::Icon::Shield, "Privacy controls", "Privacy"),
            (ui::icons::Icon::History, "Notification history", "History"),
            (ui::icons::Icon::Power, "Power menu", "Power"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = self.action(i);
            let power = i == 3;
            let hovered = cursor.is_some_and(|p| r.contains(p));
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(r, "")
                    .hovered(hovered)
                    .focused(self.keyboard_focus == Some(i + 1))
                    .accessible_name(accessible),
            );
            let color = if power {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    r.center().x - 9.,
                    r.y0 + self.scaled(8.),
                    r.center().x + 9.,
                    r.y0 + self.scaled(26.),
                ),
                icon,
                color,
                1.5,
            );
            // These four labels are fixed, high-value shell destinations.
            // Keep every word intact; at an unusually large text scale we
            // reduce this one caption before we truncate its meaning.
            let max_label_width = (r.width() - self.scaled(8.)).max(1.);
            let label_size = theme.tokens.typography.scale.xs;
            let (unscaled_label_width, _) =
                text.measure_weighted(label, label_size, None, TextWeight::Medium);
            let label_size = if f64::from(unscaled_label_width) > max_label_width {
                label_size * f32v(max_label_width / f64::from(unscaled_label_width))
            } else {
                label_size
            };
            let (label_width, _) =
                text.measure_weighted(label, label_size, None, TextWeight::Medium);
            text.draw_text_weighted(
                scene,
                label,
                label_size,
                color,
                transform.pre_translate(
                    (
                        r.center().x - f64::from(label_width) / 2.,
                        r.y0 + self.scaled(46.),
                    )
                        .into(),
                ),
                None,
                TextWeight::Medium,
            );
        }
        let sliders = Rect::new(
            self.panel.x0 + self.scaled(Self::PAD),
            self.panel.y0
                + self.scaled(Self::PAD + Self::HEADER + Self::GAP + Self::ACTIONS + Self::GAP),
            self.panel.x1 - self.scaled(Self::PAD),
            self.panel.y0
                + self.scaled(
                    Self::PAD
                        + Self::HEADER
                        + Self::GAP
                        + Self::ACTIONS
                        + Self::GAP
                        + Self::SLIDERS,
                ),
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(sliders, "")
                .radius(12.)
                .accessible_name("Brightness and volume"),
        );
        for i in 0..2 {
            let (bar, y) = self.slider_geometry_for(self.panel, i);
            let face = Rect::new(
                bar.x0 - self.scaled(5.),
                y - self.scaled(4.),
                bar.x1 + self.scaled(5.),
                y + self.scaled(26.),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    self.panel.x0 + self.scaled(28.),
                    y + self.scaled(3.),
                    self.panel.x0 + self.scaled(44.),
                    y + self.scaled(19.),
                ),
                if i == 0 {
                    ui::icons::Icon::Sun
                } else {
                    ui::icons::Icon::Volume2
                },
                colors.text_dim,
                1.5,
            );
            Slider::paint(
                scene,
                transform,
                theme,
                &SliderFace::new(
                    face,
                    f64::from(if i == 0 {
                        state.brightness
                    } else {
                        state.volume
                    }),
                )
                .hovered(cursor.is_some_and(|p| face.contains(p)))
                .focused(self.keyboard_focus == Some(5 + i))
                .dragging(self.slider_drag == Some(i)),
            );
        }
        let controls = self.visible_controls(state);
        for (i, c) in controls.iter().copied().enumerate() {
            self.draw_control(
                scene,
                transform,
                text,
                theme,
                cursor,
                state,
                i,
                controls.len(),
                c,
            );
        }
        let r = self.power_geometry(self.panel, state);
        let power_focus = 7 + self.visible_controls(state).len();
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(r, "")
                .hovered(cursor.is_some_and(|p| r.contains(p)))
                .focused(self.keyboard_focus == Some(power_focus))
                .accessible_name("Change power profile"),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                r.x0 + self.scaled(12.),
                r.y0 + self.scaled(15.),
                r.x0 + self.scaled(30.),
                r.y0 + self.scaled(33.),
            ),
            ui::icons::Icon::Battery,
            colors.semantic.success,
            1.5,
        );
        text.draw_text_weighted(
            scene,
            state.power_profile.label(),
            theme.tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate((r.x0 + self.scaled(42.), r.y0 + self.scaled(17.)).into()),
            None,
            TextWeight::Semibold,
        );
        let battery_detail = if state.battery.present {
            format!("{}% battery", state.battery.capacity)
        } else {
            "AC power".to_string()
        };
        text.draw_text(
            scene,
            &battery_detail,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((r.x0 + self.scaled(42.), r.y0 + self.scaled(33.)).into()),
            None,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                r.x1 - self.scaled(28.),
                r.y0 + self.scaled(17.),
                r.x1 - self.scaled(12.),
                r.y0 + self.scaled(33.),
            ),
            ui::icons::Icon::ChevronRight,
            colors.text_dim,
            1.5,
        );
    }
    fn draw_control(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
        s: &QuickSettingsState,
        i: usize,
        count: usize,
        c: QuickSettingsControl,
    ) {
        let colors = &theme.tokens.colors;
        let r = self.control_card(self.panel, i, count);
        let on = Self::on(c, s);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(r, "")
                .hovered(cursor.is_some_and(|p| r.contains(p)))
                .focused(self.keyboard_focus == Some(7 + i))
                .accessible_name(Self::rendered_label(c, s)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                r.x0 + self.scaled(14.),
                r.y0 + self.scaled(11.),
                r.x0 + self.scaled(32.),
                r.y0 + self.scaled(29.),
            ),
            Self::icon(c, s),
            if on {
                colors.accent[5]
            } else {
                colors.text_primary
            },
            1.5,
        );
        text.draw_text_weighted(
            scene,
            Self::rendered_label(c, s),
            theme.tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate((r.x0 + self.scaled(14.), r.y0 + self.scaled(40.)).into()),
            None,
            TextWeight::Semibold,
        );
        let sub = text.elide_to_width(
            Self::subline(c, s),
            theme.tokens.typography.scale.xs,
            (r.width() - self.scaled(70.)) as f32,
        );
        text.draw_text(
            scene,
            &sub,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((r.x0 + self.scaled(14.), r.y0 + self.scaled(55.)).into()),
            None,
        );
        if (matches!(c, QuickSettingsControl::Wifi) && s.radios.wifi_available)
            || matches!(
                c,
                QuickSettingsControl::Bluetooth | QuickSettingsControl::Vpn
            )
        {
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    r.x1 - self.scaled(30.),
                    r.y0 + self.scaled(13.),
                    r.x1 - self.scaled(14.),
                    r.y0 + self.scaled(29.),
                ),
                ui::icons::Icon::ChevronRight,
                colors.text_dim,
                1.5,
            );
        } else {
            Toggle::paint(
                scene,
                transform,
                theme,
                &ToggleFace::new(
                    Rect::new(
                        r.x1 - self.scaled(50.),
                        r.y0 + self.scaled(11.),
                        r.x1 - self.scaled(14.),
                        r.y0 + self.scaled(31.),
                    ),
                    on,
                ),
            );
        }
    }
    fn draw_edit(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
        state: &QuickSettingsState,
    ) {
        let colors = &theme.tokens.colors;
        text.draw_text(
            scene,
            "Drag the grip to reorder · Ctrl+↑/↓ also works",
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate(
                (
                    self.panel.x0 + self.scaled(Self::PAD),
                    self.panel.y0 + self.scaled(Self::PAD + Self::HEADER + 14.),
                )
                    .into(),
            ),
            None,
        );
        let all = self.edit_controls(state);
        let metrics = ListMetrics::uniform(
            self.edit_grid_top(self.panel),
            self.scaled(Self::EDIT_ROW),
            self.scaled(6.),
            all.len(),
        );
        self.edit_drag.sync_insertion(&metrics);
        for (i, c) in all.iter().copied().enumerate() {
            // The dragged control is represented by the elevated ghost
            // below.  Painting its original row as well makes the item that
            // reflows into the vacated slot paint on top of it, which is why
            // a live drag could briefly show two labels in one row.
            if self.edit_drag.from_index() == Some(i) {
                continue;
            }
            let mut r = self.card(self.panel, i, true);
            r = r.with_origin((r.x0, r.y0 + self.edit_drag.item_shift(i, &metrics)));
            let present = self.config.controls.contains(&c);
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(r, "")
                    .hovered(cursor.is_some_and(|p| r.contains(p)))
                    .focused(self.keyboard_focus == Some(i + 1))
                    .accessible_name(Self::label(c)),
            );
            // A six-dot grip is a familiar, intentional reorder affordance;
            // the old horizontal ellipsis read as accidental punctuation.
            for x in [r.x0 + self.scaled(15.), r.x0 + self.scaled(21.)] {
                for y in [
                    r.center().y - self.scaled(6.),
                    r.center().y,
                    r.center().y + self.scaled(6.),
                ] {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        colors.text_dim,
                        None,
                        &Circle::new(Point::new(x, y), 1.25),
                    );
                }
            }
            let action = Rect::new(
                r.x1 - self.scaled(82.),
                r.y0 + self.scaled(9.),
                r.x1 - self.scaled(10.),
                r.y1 - self.scaled(9.),
            );
            let label = text.elide_to_width(
                Self::label(c),
                theme.tokens.typography.scale.sm,
                (action.x0 - r.x0 - self.scaled(52.)).max(1.0) as f32,
            );
            text.draw_text_weighted(
                scene,
                &label,
                theme.tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((r.x0 + self.scaled(42.), r.y0 + self.scaled(25.)).into()),
                None,
                TextWeight::Medium,
            );
            let action_label = if c.required() {
                "Required"
            } else if present {
                "Remove"
            } else {
                "Add"
            };
            let action_face = ButtonFace::new(action, action_label)
                .hovered(cursor.is_some_and(|p| action.contains(p)))
                .accessible_name(if c.required() {
                    "Required Quick Settings control"
                } else if present {
                    "Remove Quick Settings control"
                } else {
                    "Add Quick Settings control"
                })
                .text_size(theme.tokens.typography.scale.xs);
            let action_face = if c.required() {
                action_face.ghost()
            } else {
                action_face.icon(if present {
                    ui::icons::Icon::X
                } else {
                    ui::icons::Icon::Plus
                })
            };
            Button::paint(scene, transform, text, theme, &action_face);
        }
        if let Some(ghost) = self.edit_drag.ghost_rect(
            self.panel.x0 + self.scaled(Self::PAD),
            self.panel.x1 - self.scaled(Self::PAD),
            &metrics,
        ) {
            // The floating item retains its identity, not merely a coloured
            // insertion hole. This gives pointer/touch DnD a stable object
            // to follow and leaves the reflowed gap unambiguous.
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(ghost, "")
                    .focused(true)
                    .accessible_name("Quick Settings control being reordered"),
            );
            if let Some(control) = self.edit_drag.dragged_id().copied() {
                for x in [ghost.x0 + self.scaled(15.), ghost.x0 + self.scaled(21.)] {
                    for y in [
                        ghost.center().y - self.scaled(6.),
                        ghost.center().y,
                        ghost.center().y + self.scaled(6.),
                    ] {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            colors.text_dim,
                            None,
                            &Circle::new(Point::new(x, y), 1.25),
                        );
                    }
                }
                let action = Rect::new(
                    ghost.x1 - self.scaled(82.),
                    ghost.y0 + self.scaled(9.),
                    ghost.x1 - self.scaled(10.),
                    ghost.y1 - self.scaled(9.),
                );
                let label = text.elide_to_width(
                    Self::label(control),
                    theme.tokens.typography.scale.sm,
                    (action.x0 - ghost.x0 - self.scaled(52.)).max(1.0) as f32,
                );
                text.draw_text_weighted(
                    scene,
                    &label,
                    theme.tokens.typography.scale.sm,
                    colors.text_primary,
                    transform.pre_translate(
                        (ghost.x0 + self.scaled(42.), ghost.y0 + self.scaled(25.)).into(),
                    ),
                    None,
                    TextWeight::Medium,
                );
                let ghost_action = if control.required() {
                    "Required"
                } else {
                    "Remove"
                };
                let ghost_face = ButtonFace::new(action, ghost_action)
                    .text_size(theme.tokens.typography.scale.xs)
                    .accessible_name(if control.required() {
                        "Required Quick Settings control"
                    } else {
                        "Remove Quick Settings control"
                    });
                let ghost_face = if control.required() {
                    ghost_face.ghost()
                } else {
                    ghost_face.icon(ui::icons::Icon::X)
                };
                Button::paint(scene, transform, text, theme, &ghost_face);
            }
        }
        let reset = self.edit_reset(state);
        let done = self.edit_done(state);
        let edit_count = self.edit_controls(state).len();
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(reset, "Reset defaults")
                .hovered(cursor.is_some_and(|p| reset.contains(p)))
                .focused(self.keyboard_focus == Some(edit_count + 1))
                .accessible_name("Reset Quick Settings defaults"),
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(done, "Done")
                .primary()
                .hovered(cursor.is_some_and(|p| done.contains(p)))
                .focused(self.keyboard_focus == Some(edit_count + 2))
                .accessible_name("Finish editing Quick Settings"),
        );
    }
    pub fn accessibility_snapshot(&self, state: &QuickSettingsState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut root = AccessNode::new(Role::Dialog)
            .with_name("Quick Settings")
            .with_description(if self.edit_mode {
                "Edit controls; drag or use Control plus Arrow keys to reorder"
            } else {
                "Frequently used system controls"
            })
            .with_state(AccessState::new().modal(true));
        root.children.push(
            AccessNode::new(Role::Button)
                .with_name(if self.edit_mode {
                    "Finish editing controls"
                } else {
                    "Edit controls"
                })
                .with_state(AccessState::new().focusable(true)),
        );
        if !self.edit_mode {
            for name in ["Settings", "Privacy", "History", "Power"] {
                root.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_state(AccessState::new().focusable(true)),
                );
            }
            for (name, value) in [("Brightness", state.brightness), ("Volume", state.volume)] {
                root.children.push(
                    AccessNode::new(Role::Slider)
                        .with_name(name)
                        .with_value(format!("{:.0}%", value * 100.0))
                        .with_description("Use Left and Right Arrow to adjust")
                        .with_state(AccessState::new().focusable(true)),
                );
            }
        }
        for c in if self.edit_mode {
            self.edit_controls(state)
        } else {
            self.visible_controls(state)
        } {
            let role = if self.edit_mode || matches!(c, QuickSettingsControl::Vpn) {
                Role::Button
            } else {
                Role::Switch
            };
            let mut node = AccessNode::new(role)
                .with_name(Self::rendered_label(c, state))
                .with_description(if self.edit_mode {
                    if c.required() {
                        "Required; use Control plus Arrow keys to reorder"
                    } else {
                        "Select to add or remove; use Control plus Arrow keys to reorder"
                    }
                } else {
                    Self::subline(c, state)
                })
                .with_state(AccessState::new().focusable(true));
            if role == Role::Switch {
                node.state.checked = Some(Self::on(c, state));
            }
            root.children.push(node);
        }
        root
    }
    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.panel),
            [0] => Some(self.edit_rect()),
            _ => None,
        }
    }
    pub fn focus_accessibility_path(&mut self, path: &[usize], _: &QuickSettingsState) -> bool {
        let [i] = path else {
            return false;
        };
        self.keyboard_focus = Some(*i);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PANEL: Rect = Rect::new(100., 48., 500., 560.);
    #[test]
    fn default_omits_airplane_and_dead_radios() {
        let root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        assert_eq!(root.config().controls, QuickSettingsControl::DEFAULT);
        assert!(
            !root
                .config()
                .controls
                .contains(&QuickSettingsControl::AirplaneMode)
        );
        assert_eq!(root.control_count(&state), 3);
    }
    #[test]
    fn network_slot_is_hardware_truthful_and_unifies_wired() {
        let root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        // No Wi-Fi, Bluetooth, or wired hardware means no inert radio cards.
        assert_eq!(root.control_count(&state), 1);

        state.wired = Some(os_actions::WiredInfo {
            connected: true,
            device: Some("enp0s31f6".into()),
            ..Default::default()
        });
        assert_eq!(root.control_count(&state), 2);
        assert_eq!(
            QuickSettingsRoot::rendered_label(QuickSettingsControl::Wifi, &state),
            "Wired network"
        );

        state.radios.wifi_available = true;
        assert_eq!(
            QuickSettingsRoot::rendered_label(QuickSettingsControl::Wifi, &state),
            "Wi-Fi"
        );
    }
    #[test]
    fn accessibility_exposes_only_present_hardware_with_dynamic_network_name() {
        let root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.wired = Some(os_actions::WiredInfo {
            connected: true,
            device: Some("eno1".into()),
            ..Default::default()
        });
        let snapshot = root.accessibility_snapshot(&state);
        let names: Vec<_> = snapshot
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        assert!(names.contains(&"Wired network"));
        assert!(!names.contains(&"Bluetooth"));
        assert!(!names.contains(&"Airplane mode"));
    }
    #[test]
    fn config_migrates_duplicates_and_required_wifi() {
        let c = QuickSettingsConfig {
            controls: vec![
                QuickSettingsControl::NightLight,
                QuickSettingsControl::NightLight,
            ],
        }
        .normalised();
        assert_eq!(
            c.controls,
            vec![QuickSettingsControl::Wifi, QuickSettingsControl::NightLight]
        );
    }
    #[test]
    fn edit_add_remove_and_keyboard_reorder_change_model() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.edit_mode = true;
        root.keyboard_focus = Some(1);
        assert!(root.reorder_focused(&state, 1));
        assert_eq!(root.config.controls[1], QuickSettingsControl::Wifi);
        root.panel = PANEL;
        let airplane = root
            .edit_controls(&state)
            .iter()
            .position(|c| *c == QuickSettingsControl::AirplaneMode)
            .unwrap_or_default();
        let row = root.card(PANEL, airplane, true);
        assert_eq!(
            root.edit_click(row.center(), &state),
            RootAction::LayoutChanged
        );
        assert!(
            root.config
                .controls
                .contains(&QuickSettingsControl::AirplaneMode)
        );
    }
    #[test]
    fn pointer_drag_uses_shared_controller_and_commits() {
        let mut root = QuickSettingsRoot::default();
        root.edit_mode = true;
        root.panel = PANEL;
        let mut state = QuickSettingsState::default();
        state.bluetooth_available = true;
        let row = root.card(PANEL, 1, true);
        let start = Point::new(row.x0 + 20.0, row.center().y);
        assert_eq!(root.edit_click(start, &state), RootAction::None);
        assert!(root.update_drag(Point::new(start.x, start.y + 80.), PANEL, &mut state));
        assert!(root.edit_drag.is_active());
        let _ = root.finish_drag(&state);
        assert_eq!(root.config.controls[2], QuickSettingsControl::Bluetooth);
    }
    #[test]
    fn compact_geometry_never_overlaps() {
        let root = QuickSettingsRoot::default();
        let state = QuickSettingsState::default();
        let panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
        for i in 0..root.control_count(&state) {
            let r = root.control_card(panel, i, root.control_count(&state));
            for j in 0..i {
                let overlap = r.intersect(root.control_card(panel, j, root.control_count(&state)));
                assert!(overlap.width() <= 0.0 || overlap.height() <= 0.0);
            }
        }
    }
    #[test]
    fn odd_control_count_uses_a_balanced_full_width_last_card() {
        let panel = Rect::new(0., 48., 400., 520.);
        let root = QuickSettingsRoot::default();
        let full = root.control_card(panel, 2, 3);
        assert_eq!(full.x0, panel.x0 + QuickSettingsRoot::PAD);
        assert_eq!(full.x1, panel.x1 - QuickSettingsRoot::PAD);
        assert_eq!(full.width(), 368.);
    }
    #[test]
    fn labelled_top_utilities_keep_independent_touch_targets() {
        let mut root = QuickSettingsRoot::default();
        root.panel = Rect::new(0., 48., 400., 520.);
        let actions: Vec<_> = (0..4).map(|index| root.action(index)).collect();
        for action in &actions {
            assert!(action.width() >= 44. && action.height() >= 44.);
        }
        for (index, action) in actions.iter().enumerate() {
            for earlier in &actions[..index] {
                let overlap = action.intersect(*earlier);
                assert!(overlap.width() <= 0. || overlap.height() <= 0.);
            }
        }
    }
    #[test]
    fn large_text_keeps_the_complete_root_panel_on_an_800_by_600_desktop() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.set_layout_text_scale(1.3);
        // The panel begins below the 48px top bar and must leave the shell's
        // 16px lower margin intact.  This catches accidental scaling of
        // whitespace that hides the power profile at common laptop sizes.
        assert!(root.preferred_height(&state) <= 536.);
        let panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
        assert!(root.power_geometry(panel, &state).y1 <= panel.y1);
    }
    #[test]
    fn edit_catalogue_does_not_offer_unavailable_mobile_data() {
        let root = QuickSettingsRoot::default();
        let state = QuickSettingsState::default();
        assert!(
            !root
                .edit_controls(&state)
                .contains(&QuickSettingsControl::MobileData)
        );
    }
}
