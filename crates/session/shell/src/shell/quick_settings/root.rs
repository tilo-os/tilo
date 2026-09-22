//! Compact, configurable root page for the Quick Settings popover.
//!
//! The panel presents current controls, not a dashboard of every capability.
//! Arrangement is durable user intent while hardware availability is applied
//! only when the panel is rendered.

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::quick_settings::{PowerProfile, QsLevel};
use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use ui::ColorCompat;
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
    #[allow(
        clippy::cast_possible_truncation,
        reason = "quick-settings geometry is bounded to desktop pixel dimensions representable as f32"
    )]
    {
        value as f32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootAction {
    None,
    OpenSettings,
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

struct ControlPaint {
    control: QuickSettingsControl,
    rect: Rect,
    on: bool,
}

/// Root interaction state.  Editing uses the shared Tilo reorder controller;
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
    const PAD: f64 = 14.0;
    // Quick Settings is opened to change state, so controls start immediately
    // under the panel edge. Destinations live in the restrained utility rail
    // at the bottom instead of consuming the first third of the popover.
    const SLIDER_FOCUS_START: usize = 0;
    const CONTROL_FOCUS_START: usize = Self::SLIDER_FOCUS_START + 2;
    const SLIDERS: f64 = 58.0;
    const CARD: f64 = 58.0;
    const GAP: f64 = 7.0;
    const POWER: f64 = 44.0;
    const UTILITY: f64 = 44.0;
    const EDIT_HEADER: f64 = 42.0;
    const EDIT_ROW: f64 = 44.0;
    const EDIT_ACTION_WIDTH: f64 = 76.0;
    // Edit mode has three distinct header lines: title, a concise interaction
    // hint, and the active-controls section label.  Keeping this as a real
    // measured band prevents the hint and section label from sharing the same
    // line box at ordinary text sizes (and from colliding outright with large
    // text).
    const EDIT_HELP: f64 = 38.0;
    const EDIT_FOOTER: f64 = 36.0;
    const EDIT_HELP_COPY: &'static str = "Drag to reorder · Ctrl + Up/Down";

    fn header_baseline_offset(&self) -> f64 {
        // Spatial chrome may compact below 1× on a short display, but the
        // title keeps the user's full text scale. Preserve a font-aware
        // baseline so large text never collides with the action row.
        (Self::PAD + 22.0) * self.layout_scale.max(1.0)
    }

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
        // Opening the popover does not itself move keyboard focus. The first
        // Tab selects the first visible control (Edit), matching ordinary
        // desktop traversal instead of silently preselecting it and making
        // that same Tab skip to Settings.
        self.keyboard_focus = None;
    }

    /// Named geometry for every visible root affordance. Quick Settings is
    /// retained chrome rather than a child-widget tree, so this projection is
    /// the semantic equivalent of `Widget::test_rects()`.
    pub fn semantic_rects(&self, state: &QuickSettingsState) -> Vec<(String, Rect)> {
        if self.edit_mode {
            let active = self.active_edit_controls(state);
            let available = self.available_edit_controls(state);
            let mut rects = active
                .iter()
                .enumerate()
                .map(|(index, control)| {
                    (
                        format!(
                            "quick_settings_edit_{}",
                            Self::semantic_control_name(*control)
                        ),
                        self.card(self.panel, index, true),
                    )
                })
                .collect::<Vec<_>>();
            rects.extend(available.iter().enumerate().map(|(index, control)| {
                (
                    format!(
                        "quick_settings_add_{}",
                        Self::semantic_control_name(*control)
                    ),
                    self.available_edit_chip(state, index),
                )
            }));
            rects.push(("quick_settings_reset".into(), self.edit_reset(state)));
            rects.push(("quick_settings_done".into(), self.edit_done(state)));
            return rects;
        }

        let mut rects = Vec::new();
        for (index, name) in ["brightness", "volume"].into_iter().enumerate() {
            let (bar, _) = self.slider_geometry_for(self.panel, index);
            rects.push((format!("quick_settings_{name}"), bar.inflate(6.0, 10.0)));
        }
        let controls = self.visible_controls(state);
        for (index, control) in controls.iter().copied().enumerate() {
            let card = self.control_card(self.panel, index, controls.len());
            let name = Self::semantic_control_name(control);
            let primary = if Self::has_details(control, state) {
                Rect::new(
                    card.x0,
                    card.y0,
                    self.control_details_rect(card).x0,
                    card.y1,
                )
            } else {
                card
            };
            rects.push((format!("quick_settings_{name}"), primary));
            if Self::has_details(control, state) {
                rects.push((
                    format!("quick_settings_{name}_details"),
                    self.control_details_rect(card),
                ));
            }
        }
        rects.push((
            "quick_settings_power_profile".into(),
            self.power_geometry(self.panel, state),
        ));
        rects.extend([
            (
                "quick_settings_edit".into(),
                self.utility(self.panel, state, 0),
            ),
            (
                "quick_settings_settings".into(),
                self.utility(self.panel, state, 1),
            ),
            (
                "quick_settings_power".into(),
                self.utility(self.panel, state, 2),
            ),
        ]);
        rects
    }

    const fn semantic_control_name(control: QuickSettingsControl) -> &'static str {
        match control {
            QuickSettingsControl::Wifi => "network",
            QuickSettingsControl::Bluetooth => "bluetooth",
            QuickSettingsControl::Vpn => "vpn",
            QuickSettingsControl::NightLight => "night_light",
            QuickSettingsControl::AirplaneMode => "airplane_mode",
            QuickSettingsControl::MobileData => "mobile_data",
        }
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
    /// Fit root-page spatial chrome to the available popover height while
    /// leaving text itself at the user's full theme scale. The same effective
    /// factor drives drawing and hit testing through every geometry helper.
    pub fn fit_root_layout_to_height(&mut self, state: &QuickSettingsState, height: f64) {
        let controls = self.visible_controls(state).len();
        let rows = controls.div_ceil(2);
        let unscaled = Self::PAD * 2.0
            + Self::SLIDERS
            + Self::POWER
            + Self::UTILITY
            + Self::GAP * 3.0
            + n(rows) * Self::CARD
            + n(rows.saturating_sub(1)) * Self::GAP;
        self.layout_scale = self.layout_scale.min((height / unscaled).clamp(0.88, 1.2));
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
            // Stop part-way through the destination slot rather than exactly
            // centred on it. That exposes the same lifted ghost, insertion
            // rail and sibling reflow a person sees while dragging, instead
            // of making a visual regression scene resemble mere focus.
            .pointer_moved(
                Point::new(start.x, start.y + metrics.pitch() + self.scaled(8.)),
                &metrics,
            );
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
                // Airplane mode is only meaningful with a radio, except for
                // a person who explicitly kept it in their saved layout. In
                // that migration case retain it so it can be removed rather
                // than silently changing their configuration.
                QuickSettingsControl::AirplaneMode => {
                    state.radios.wifi_available
                        || state.radios.wwan_available
                        || self
                            .config
                            .controls
                            .contains(&QuickSettingsControl::AirplaneMode)
                }
                QuickSettingsControl::Vpn => !state.vpns.is_empty(),
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
                QuickSettingsControl::Wifi | QuickSettingsControl::NightLight => true,
                QuickSettingsControl::Bluetooth => state.bluetooth_available,
                QuickSettingsControl::Vpn => !state.vpns.is_empty(),
                QuickSettingsControl::AirplaneMode => {
                    state.radios.wifi_available
                        || state.radios.wwan_available
                        || self
                            .config
                            .controls
                            .contains(&QuickSettingsControl::AirplaneMode)
                }
                QuickSettingsControl::MobileData => state.radios.wwan_available,
            })
            .collect()
    }
    fn active_edit_controls(&self, state: &QuickSettingsState) -> Vec<QuickSettingsControl> {
        self.edit_controls(state)
            .into_iter()
            .filter(|control| self.config.controls.contains(control))
            .collect()
    }
    fn available_edit_controls(&self, state: &QuickSettingsState) -> Vec<QuickSettingsControl> {
        self.edit_controls(state)
            .into_iter()
            .filter(|control| !self.config.controls.contains(control))
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
            self.active_edit_controls(state).len()
        } else {
            count.div_ceil(2)
        };
        if self.edit_mode {
            self.scaled(
                Self::PAD * 2.0
                    + Self::EDIT_HEADER
                    + Self::EDIT_HELP
                    + Self::GAP * 2.0
                    + n(rows) * Self::EDIT_ROW
                    + n(rows.saturating_sub(1)) * 6.0
                    + if self.available_edit_controls(state).is_empty() {
                        0.0
                    } else {
                        let available_rows = self.available_edit_controls(state).len().div_ceil(2);
                        24.0 + n(available_rows) * 38.0
                    }
                    + Self::EDIT_FOOTER,
            )
        } else {
            self.scaled(
                Self::PAD * 2.0
                    + Self::SLIDERS
                    + Self::POWER
                    + Self::UTILITY
                    + Self::GAP * 3.0
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
        panel.y0 + self.scaled(Self::PAD + Self::SLIDERS + Self::GAP)
    }
    fn edit_grid_top(&self, panel: Rect) -> f64 {
        panel.y0
            + self.scaled(Self::PAD)
            + self.scaled(Self::EDIT_HEADER)
            + self.scaled(Self::EDIT_HELP)
            + self.scaled(Self::GAP)
    }

    fn edit_help_baseline(&self) -> f64 {
        self.panel.y0 + self.header_baseline_offset() + self.scaled(20.0)
    }

    fn edit_section_baseline(&self) -> f64 {
        self.edit_help_baseline() + self.scaled(20.0)
    }
    fn available_edit_top(&self, state: &QuickSettingsState) -> f64 {
        let active = self.active_edit_controls(state).len();
        self.edit_grid_top(self.panel)
            + n(active) * self.scaled(Self::EDIT_ROW + 6.0)
            + self.scaled(10.0)
    }

    fn available_edit_chip(&self, state: &QuickSettingsState, index: usize) -> Rect {
        let available = self.available_edit_controls(state);
        let count = available.len();
        if count == 0 || index >= count {
            return Rect::ZERO;
        }
        let gap = self.scaled(6.0);
        let x0 = self.panel.x0 + self.scaled(Self::PAD);
        let width = (self.panel.width() - self.scaled(Self::PAD * 2.0) - gap) / 2.0;
        let col = index % 2;
        let row = index / 2;
        let x = x0 + n(col) * (width + gap);
        let y = self.available_edit_top(state) + self.scaled(24.0 + n(row) * 38.0);
        Rect::new(x, y, x + width, y + self.scaled(32.0))
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
    fn edit_action_rect(&self, row: Rect) -> Rect {
        Rect::new(
            row.x1 - self.scaled(Self::EDIT_ACTION_WIDTH + 6.0),
            row.y0 + self.scaled(7.0),
            row.x1 - self.scaled(6.0),
            row.y1 - self.scaled(7.0),
        )
    }
    fn control_card(&self, panel: Rect, i: usize, count: usize) -> Rect {
        let row = i / 2;
        let pad = self.scaled(Self::PAD);
        let gap = self.scaled(Self::GAP);
        let card = self.scaled(Self::CARD);
        let y = self.grid_top(panel) + n(row) * (card + gap);
        if count % 2 == 1 && i.checked_add(1) == Some(count) {
            return Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + card);
        }
        let w = (panel.width() - 2.0 * pad - gap) / 2.0;
        let x = panel.x0 + pad + n(i % 2) * (w + gap);
        Rect::new(x, y, x + w, y + card)
    }
    fn has_details(control: QuickSettingsControl, state: &QuickSettingsState) -> bool {
        matches!(
            control,
            QuickSettingsControl::Bluetooth | QuickSettingsControl::Vpn
        ) || (control == QuickSettingsControl::Wifi && state.radios.wifi_available)
    }
    fn control_details_rect(&self, card: Rect) -> Rect {
        Rect::new(card.x1 - self.scaled(38.0), card.y0, card.x1, card.y1)
    }
    fn control_toggle_rect(&self, card: Rect) -> Rect {
        Rect::new(
            card.x1 - self.scaled(78.0),
            card.y0 + self.scaled(8.0),
            card.x1 - self.scaled(42.0),
            card.y0 + self.scaled(28.0),
        )
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
        let y = panel.y0 + (Self::PAD + 9.0 + n(i) * 27.0) * scale;
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
    fn power_focus_index(control_count: usize) -> usize {
        // Keep the trailing power-profile row after the two sliders and all
        // visible controls. This is also the index used by keyboard
        // activation; using a separate offset here makes the painted focus
        // ring target a different control from Enter.
        Self::CONTROL_FOCUS_START.saturating_add(control_count)
    }
    fn utility(&self, panel: Rect, state: &QuickSettingsState, index: usize) -> Rect {
        let power = self.power_geometry(panel, state);
        let top = power.y1 + self.scaled(Self::GAP);
        let gap = self.scaled(6.0);
        let width = (panel.width() - self.scaled(Self::PAD * 2.0) - gap * 2.0) / 3.0;
        let x = panel.x0 + self.scaled(Self::PAD) + n(index) * (width + gap);
        Rect::new(x, top, x + width, top + self.scaled(Self::UTILITY))
    }
    fn edit_done(&self, _state: &QuickSettingsState) -> Rect {
        Rect::new(
            self.panel.x1 - self.scaled(Self::PAD + 68.0),
            self.panel.y0 + self.scaled(Self::PAD + 3.0),
            self.panel.x1 - self.scaled(Self::PAD),
            self.panel.y0 + self.scaled(Self::PAD + 35.0),
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
    fn subline(c: QuickSettingsControl, s: &QuickSettingsState) -> &str {
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
            QuickSettingsControl::Bluetooth if s.bluetooth => s
                .bt_devices
                .iter()
                .find(|v| v.connected)
                .map_or("Not connected", |v| v.name.as_str()),
            QuickSettingsControl::Wifi | QuickSettingsControl::Bluetooth => "Off",
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
        }
        None
    }

    pub fn focus_next(&mut self, state: &QuickSettingsState, backwards: bool) -> bool {
        let count = if self.edit_mode {
            // Editable rows, Reset defaults, Done. The header no longer
            // duplicates Done with an unexplained checkmark.
            self.edit_controls(state).len().saturating_add(2)
        } else {
            // Brightness, Volume, controls, power profile, then three utilities.
            Self::CONTROL_FOCUS_START
                .saturating_add(self.visible_controls(state).len())
                .saturating_add(4)
        };
        if count == 0 {
            return false;
        }
        self.keyboard_focus = Some(match (self.keyboard_focus, backwards) {
            (Some(i), true) => i.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1)),
            (Some(i), false) => i
                .checked_add(1)
                .and_then(|next| next.checked_rem(count))
                .unwrap_or_default(),
            (None, true) => count.saturating_sub(1),
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
            // The visual and keyboard order is active rows, available chips,
            // Reset, Done.  A focus stop must always name the control that is
            // visibly outlined rather than an item in a hidden combined list.
            let active = self.active_edit_controls(state);
            let available = self.available_edit_controls(state);
            if let Some(c) = active.get(i).copied() {
                if c.required() {
                    return RootAction::None;
                }
                self.config.controls.retain(|x| *x != c);
                self.save_config();
                return RootAction::LayoutChanged;
            }
            let available_index = i.saturating_sub(active.len());
            if let Some(c) = available.get(available_index).copied() {
                self.config.controls.push(c);
                self.save_config();
                return RootAction::LayoutChanged;
            }
            let edit_count = active.len().saturating_add(available.len());
            if i == edit_count {
                self.config = QuickSettingsConfig::default();
                self.save_config();
                return RootAction::LayoutChanged;
            }
            if edit_count.checked_add(1) == Some(i) {
                self.edit_mode = false;
                // Done is removed by the structural transition. Do not leave
                // its edit-mode index pointing at an unrelated root action
                // (for example History/Notifications) on the next frame.
                self.keyboard_focus = None;
                return RootAction::LayoutChanged;
            }
            return RootAction::None;
        }
        let controls = self.visible_controls(state);
        if i == Self::SLIDER_FOCUS_START || i == Self::SLIDER_FOCUS_START + 1 {
            return RootAction::None;
        }
        if let Some(c) = i
            .checked_sub(Self::CONTROL_FOCUS_START)
            .and_then(|index| controls.get(index))
            .copied()
        {
            let mut p = PendingRootActions::default();
            return Self::toggle(c, state, &mut p).unwrap_or(RootAction::Changed(p));
        }
        let base = Self::power_focus_index(controls.len());
        if i == base {
            return Self::cycle_power(state);
        }
        if i == base.saturating_add(1) {
            self.edit_mode = true;
            self.keyboard_focus = None;
            return RootAction::LayoutChanged;
        }
        if i == base.saturating_add(2) {
            return RootAction::OpenSettings;
        }
        if i == base.saturating_add(3) {
            return RootAction::OpenPowerMenu;
        }
        RootAction::None
    }
    pub fn reorder_focused(&mut self, state: &QuickSettingsState, direction: i8) -> bool {
        if !self.edit_mode {
            return false;
        }
        let active = self.active_edit_controls(state);
        let Some(i) = self.keyboard_focus.filter(|i| *i < active.len()) else {
            return false;
        };
        let Some(c) = active.get(i).copied() else {
            return false;
        };
        let Some(from) = self.config.controls.iter().position(|x| *x == c) else {
            return false;
        };
        // `all` also contains available-but-disabled controls. Reorder against
        // the adjacent visible row, not the numeric row index: otherwise a
        // disabled control before the focused item can make the persisted
        // order diverge from what the person sees and what accessibility
        // publishes.
        let neighbour = if direction < 0 {
            i.checked_sub(1)
        } else {
            i.checked_add(1).filter(|next| *next < active.len())
        };
        let Some(neighbour) = neighbour else {
            return false;
        };
        let Some(neighbour_control) = active.get(neighbour).copied() else {
            return false;
        };
        let Some(to) = self
            .config
            .controls
            .iter()
            .position(|candidate| *candidate == neighbour_control)
        else {
            return false;
        };
        let c = self.config.controls.remove(from);
        self.config.controls.insert(to, c);
        self.keyboard_focus = Some(neighbour);
        self.save_config();
        true
    }
    pub fn adjust_focused_slider(
        &self,
        state: &mut QuickSettingsState,
        direction: f32,
    ) -> Option<SliderCommit> {
        let available = match self.keyboard_focus? {
            Self::SLIDER_FOCUS_START => state.brightness_available,
            i if i == Self::SLIDER_FOCUS_START + 1 => state.volume_available,
            _ => return None,
        };
        if !available {
            return None;
        }
        let value = match self.keyboard_focus? {
            Self::SLIDER_FOCUS_START => &mut state.brightness,
            i if i == Self::SLIDER_FOCUS_START + 1 => &mut state.volume,
            _ => return None,
        };
        *value = (*value + direction * 0.05).clamp(0., 1.);
        Some(if self.keyboard_focus == Some(Self::SLIDER_FOCUS_START) {
            SliderCommit::Brightness(*value * 100.)
        } else {
            SliderCommit::Volume(*value * 100.)
        })
    }
    fn cycle_power(s: &mut QuickSettingsState) -> RootAction {
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
        if self.edit_mode {
            return self.edit_click(point, state);
        }
        for i in 0..2 {
            let (bar, _) = self.slider_geometry_for(panel, i);
            if Rect::new(bar.x0 - 6., bar.y0 - 10., bar.x1 + 6., bar.y1 + 10.).contains(point) {
                let available = if i == 0 {
                    state.brightness_available
                } else {
                    state.volume_available
                };
                if !available {
                    return RootAction::None;
                }
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
            if Self::has_details(c, state) && self.control_details_rect(card).contains(point) {
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
            return Self::cycle_power(state);
        }
        if self.utility(panel, state, 0).contains(point) {
            self.edit_mode = true;
            self.keyboard_focus = None;
            return RootAction::LayoutChanged;
        }
        if self.utility(panel, state, 1).contains(point) {
            return RootAction::OpenSettings;
        }
        if self.utility(panel, state, 2).contains(point) {
            return RootAction::OpenPowerMenu;
        }
        RootAction::None
    }
    fn edit_reset(&self, state: &QuickSettingsState) -> Rect {
        let available = self.available_edit_controls(state);
        let y = if available.is_empty() {
            self.available_edit_top(state)
        } else {
            let rows = available.len().div_ceil(2);
            self.available_edit_top(state) + self.scaled(24.0 + n(rows) * 38.0)
        };
        Rect::new(
            self.panel.x0 + self.scaled(Self::PAD),
            y,
            self.panel.x1 - self.scaled(Self::PAD),
            y + self.scaled(Self::EDIT_FOOTER),
        )
    }
    fn edit_click(&mut self, point: Point, state: &QuickSettingsState) -> RootAction {
        let active = self.active_edit_controls(state);
        for (i, c) in active.iter().copied().enumerate() {
            let row = self.card(self.panel, i, true);
            if !row.contains(point) {
                continue;
            }
            let present = self.config.controls.contains(&c);
            let action = self.edit_action_rect(row);
            if !c.required() && action.contains(point) {
                self.config.controls.retain(|x| *x != c);
                self.save_config();
                return RootAction::LayoutChanged;
            }
            if present {
                self.edit_drag.begin(c, i, point.y - row.y0, point);
                return RootAction::None;
            }
            return RootAction::None;
        }
        let available = self.available_edit_controls(state);
        if !available.is_empty() {
            for (i, control) in available.into_iter().enumerate() {
                let chip = self.available_edit_chip(state, i);
                if chip.contains(point) {
                    self.config.controls.push(control);
                    self.save_config();
                    return RootAction::LayoutChanged;
                }
            }
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

    #[allow(
        clippy::too_many_arguments,
        reason = "the retained-rendering entry point receives the scene, transform, text, theme, input, and current model together to produce one coherent frame"
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
        self.panel = panel;
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(panel, 18.),
        );
        if self.edit_mode {
            self.header(scene, transform, text, theme, cursor, state);
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
        state: &QuickSettingsState,
    ) {
        let colors = &theme.tokens.colors;
        text.draw_text_weighted(
            scene,
            "Customize controls",
            theme.tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate(
                (
                    self.panel.x0 + self.scaled(Self::PAD),
                    self.panel.y0 + self.header_baseline_offset(),
                )
                    .into(),
            ),
            None,
            TextWeight::Semibold,
        );
        let done = self.edit_done(state);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(done, "Done")
                .primary()
                .hovered(cursor.is_some_and(|point| done.contains(point)))
                .focused(
                    self.keyboard_focus == Some(self.edit_controls(state).len().saturating_add(1)),
                )
                .accessible_name("Finish customizing Quick Settings"),
        );
    }
    #[allow(
        clippy::too_many_lines,
        reason = "root controls share ordered retained painting and hit-region setup; splitting risks visual and accessibility divergence"
    )]
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
        let sliders = Rect::new(
            self.panel.x0 + self.scaled(Self::PAD),
            self.panel.y0 + self.scaled(Self::PAD),
            self.panel.x1 - self.scaled(Self::PAD),
            self.panel.y0 + self.scaled(Self::PAD + Self::SLIDERS),
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
            let available = if i == 0 {
                state.brightness_available
            } else {
                state.volume_available
            };
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
                .hovered(available && cursor.is_some_and(|p| face.contains(p)))
                .focused(self.keyboard_focus == Some(Self::SLIDER_FOCUS_START.saturating_add(i)))
                .dragging(self.slider_drag == Some(i))
                .disabled(!available),
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
        let power_focus = Self::power_focus_index(self.visible_controls(state).len());
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
        let utility_focus = power_focus.saturating_add(1);
        for (index, (label, icon, accessible)) in [
            (
                "Customize",
                ui::icons::Icon::Pencil,
                "Customize Quick Settings",
            ),
            ("", ui::icons::Icon::Settings2, "Open Settings"),
            ("", ui::icons::Icon::Power, "Open power menu"),
        ]
        .into_iter()
        .enumerate()
        {
            let utility = self.utility(self.panel, state, index);
            let mut face = ButtonFace::new(utility, label)
                .ghost()
                .hovered(cursor.is_some_and(|point| utility.contains(point)))
                .focused(self.keyboard_focus == Some(utility_focus.saturating_add(index)))
                .accessible_name(accessible)
                .text_size(theme.tokens.typography.scale.xs);
            if index == 0 {
                face = face.icon(icon);
                Button::paint(scene, transform, text, theme, &face);
            } else {
                Button::paint(scene, transform, text, theme, &face);
                let side = self.scaled(17.0);
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        utility.center().x - side / 2.0,
                        utility.center().y - side / 2.0,
                        utility.center().x + side / 2.0,
                        utility.center().y + side / 2.0,
                    ),
                    icon,
                    if index == 2 {
                        colors.semantic.danger
                    } else {
                        colors.text_dim
                    },
                    1.5,
                );
            }
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "a control's retained rendering needs its shared renderer context plus the explicit indexed control data for stable focus and geometry"
    )]
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
                .focused(self.keyboard_focus == Some(Self::CONTROL_FOCUS_START.saturating_add(i)))
                .accessible_name(Self::rendered_label(c, s)),
        );
        self.draw_control_content(
            scene,
            transform,
            text,
            theme,
            s,
            &ControlPaint {
                control: c,
                rect: r,
                on,
            },
        );
        if Self::has_details(c, s) {
            let details = self.control_details_rect(r);
            let details_name = format!("{} details", Self::rendered_label(c, s));
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(details, "")
                    .ghost()
                    .hovered(cursor.is_some_and(|point| details.contains(point)))
                    .accessible_name(&details_name),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                colors.border_standard,
                None,
                &vello::kurbo::Line::new(
                    Point::new(details.x0, details.y0 + self.scaled(9.0)),
                    Point::new(details.x0, details.y1 - self.scaled(9.0)),
                ),
            );
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    details.center().x - self.scaled(8.),
                    details.center().y - self.scaled(8.),
                    details.center().x + self.scaled(8.),
                    details.center().y + self.scaled(8.),
                ),
                ui::icons::Icon::ChevronRight,
                colors.text_dim,
                1.5,
            );
            if c != QuickSettingsControl::Vpn {
                Toggle::paint(
                    scene,
                    transform,
                    theme,
                    &ToggleFace::new(self.control_toggle_rect(r), on),
                );
            }
        } else {
            Toggle::paint(
                scene,
                transform,
                theme,
                &ToggleFace::new(
                    Rect::new(
                        r.x1 - self.scaled(50.),
                        r.y0 + self.scaled(8.),
                        r.x1 - self.scaled(14.),
                        r.y0 + self.scaled(28.),
                    ),
                    on,
                ),
            );
        }
    }

    fn draw_control_content(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        state: &QuickSettingsState,
        paint: &ControlPaint,
    ) {
        let &ControlPaint { control, rect, on } = paint;
        let colors = &theme.tokens.colors;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(
                rect.x0 + self.scaled(14.),
                rect.y0 + self.scaled(9.),
                rect.x0 + self.scaled(32.),
                rect.y0 + self.scaled(27.),
            ),
            Self::icon(control, state),
            if on {
                colors.accent[5]
            } else {
                colors.text_primary
            },
            1.5,
        );
        text.draw_text_weighted(
            scene,
            Self::rendered_label(control, state),
            theme.tokens.typography.scale.sm,
            colors.text_primary,
            transform
                .pre_translate((rect.x0 + self.scaled(14.), rect.y0 + self.scaled(34.)).into()),
            None,
            TextWeight::Semibold,
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let sub = text.elide_to_width(
            Self::subline(control, state),
            theme.tokens.typography.scale.xs,
            (rect.width() - self.scaled(70.)) as f32,
        );
        text.draw_text(
            scene,
            &sub,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform
                .pre_translate((rect.x0 + self.scaled(14.), rect.y0 + self.scaled(48.)).into()),
            None,
        );
    }
    #[allow(
        clippy::too_many_lines,
        reason = "edit-mode painting keeps list, drag ghost, insertion rail, and matching accessibility geometry in one ordered rendering pass"
    )]
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
            Self::EDIT_HELP_COPY,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate(
                (
                    self.panel.x0 + self.scaled(Self::PAD),
                    self.edit_help_baseline(),
                )
                    .into(),
            ),
            None,
        );
        text.draw_text(
            scene,
            "ACTIVE CONTROLS",
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate(
                (
                    self.panel.x0 + self.scaled(Self::PAD),
                    self.edit_section_baseline(),
                )
                    .into(),
            ),
            None,
        );
        let active = self.active_edit_controls(state);
        let metrics = ListMetrics::uniform(
            self.edit_grid_top(self.panel),
            self.scaled(Self::EDIT_ROW),
            self.scaled(6.),
            active.len(),
        );
        self.edit_drag.sync_insertion(&metrics);
        for (i, c) in active.iter().copied().enumerate() {
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
                    .focused(self.keyboard_focus == Some(i))
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
            let action = self.edit_action_rect(r);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
            )]
            let label = text.elide_to_width(
                Self::label(c),
                theme.tokens.typography.scale.sm,
                (action.x0 - r.x0 - self.scaled(50.)).max(1.0) as f32,
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
            let action_face = ButtonFace::new(action, if c.required() { "" } else { "Remove" })
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
                action_face.icon(ui::icons::Icon::Shield).ghost()
            } else {
                action_face
            };
            Button::paint(scene, transform, text, theme, &action_face);
        }
        if let Some(ghost) = self.edit_drag.ghost_rect(
            self.panel.x0 + self.scaled(Self::PAD),
            self.panel.x1 - self.scaled(Self::PAD),
            &metrics,
        ) {
            // The floating item retains its identity, not merely a coloured
            // insertion hole. A small physical shadow and a restrained
            // accent wash make it read as an object lifted above the list,
            // not a keyboard-focus outline that happened to move.
            // Offset the presentation very slightly from the pointer's
            // exact hit geometry. Otherwise a slow vertical drag can land
            // exactly on a row and look indistinguishable from focus even
            // with a border. The controller and drop calculation retain the
            // unmodified ghost rectangle above.
            let destination_top = self
                .edit_drag
                .insertion()
                .map(|index| metrics.slot_lead(index) + self.scaled(4.));
            let ghost_y0 = destination_top.map_or_else(
                || ghost.y0 - self.scaled(6.),
                |top| (ghost.y0 - self.scaled(6.)).max(top),
            );
            let ghost = Rect::new(
                ghost.x0 + self.scaled(6.),
                ghost_y0,
                ghost.x1 - self.scaled(6.),
                ghost_y0 + metrics.item_extent,
            );
            let shadow = ghost
                .inflate(self.scaled(5.), self.scaled(5.))
                .with_origin((ghost.x0 + self.scaled(4.), ghost.y0 + self.scaled(8.)));
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.base.with_alpha_factor(0.96),
                None,
                &RoundedRect::from_rect(shadow, self.scaled(10.)),
            );
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(ghost, "")
                    .focused(true)
                    .accessible_name("Quick Settings control being reordered"),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5].with_alpha_factor(0.20),
                None,
                &RoundedRect::from_rect(ghost, self.scaled(10.)),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &RoundedRect::from_rect(
                    Rect::new(
                        ghost.x0,
                        ghost.y0 + self.scaled(10.),
                        ghost.x0 + self.scaled(4.),
                        ghost.y1 - self.scaled(10.),
                    ),
                    self.scaled(1.5),
                ),
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
                let action = self.edit_action_rect(ghost);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
                )]
                let label = text.elide_to_width(
                    Self::label(control),
                    theme.tokens.typography.scale.sm,
                    (action.x0 - ghost.x0 - self.scaled(50.)).max(1.0) as f32,
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
                let ghost_face =
                    ButtonFace::new(action, if control.required() { "" } else { "Remove" })
                        .text_size(theme.tokens.typography.scale.xs)
                        .accessible_name(if control.required() {
                            "Required Quick Settings control"
                        } else {
                            "Remove Quick Settings control"
                        });
                let ghost_face = if control.required() {
                    ghost_face.ghost()
                } else {
                    ghost_face
                };
                Button::paint(scene, transform, text, theme, &ghost_face);
            }
            if let Some(insertion) = self.edit_drag.insertion().filter(|index| *index > 0) {
                // A drop seam is a second, independent cue: the lifted
                // item follows the pointer while this rail states exactly
                // where release will persist it. The rail belongs in the
                // measured inter-row gap, never on the ghost or its label;
                // anchoring it to the target's leading edge caused a
                // large-text regression where it crossed the action button.
                let y = metrics.slot_lead(insertion) - metrics.gap / 2.0;
                let rail_half = (metrics.gap * 0.22).clamp(1.0, 2.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(
                            self.panel.x0 + self.scaled(Self::PAD + 8.),
                            y - rail_half,
                            self.panel.x1 - self.scaled(Self::PAD + 8.),
                            y + rail_half,
                        ),
                        rail_half,
                    ),
                );
            }
        }
        let available = self.available_edit_controls(state);
        if !available.is_empty() {
            let top = self.available_edit_top(state);
            text.draw_text(
                scene,
                "AVAILABLE",
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate(
                    (
                        self.panel.x0 + self.scaled(Self::PAD),
                        top + self.scaled(12.0),
                    )
                        .into(),
                ),
                None,
            );
            for (i, control) in available.iter().copied().enumerate() {
                let chip = self.available_edit_chip(state, i);
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(chip, Self::label(control))
                        .icon(ui::icons::Icon::Plus)
                        .text_size(theme.tokens.typography.scale.xs)
                        .hovered(cursor.is_some_and(|point| chip.contains(point)))
                        .focused(self.keyboard_focus == Some(active.len().saturating_add(i)))
                        .accessible_name(Self::label(control)),
                );
            }
        }
        let reset = self.edit_reset(state);
        let edit_count = active.len().saturating_add(available.len());
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(reset, "Reset defaults")
                .hovered(cursor.is_some_and(|p| reset.contains(p)))
                .focused(self.keyboard_focus == Some(edit_count))
                .accessible_name("Reset Quick Settings defaults"),
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
        if !self.edit_mode {
            for (name, value, available) in [
                ("Brightness", state.brightness, state.brightness_available),
                ("Volume", state.volume, state.volume_available),
            ] {
                root.children.push(
                    AccessNode::new(Role::Slider)
                        .with_name(name)
                        .with_value(if available {
                            format!("{:.0}%", value * 100.0)
                        } else {
                            "Unavailable".into()
                        })
                        .with_description(if available {
                            "Use Left and Right Arrow to adjust"
                        } else {
                            "No controllable device is available"
                        })
                        .with_state(AccessState::new().focusable(available).disabled(!available)),
                );
            }
        }
        let controls = if self.edit_mode {
            self.active_edit_controls(state)
                .into_iter()
                .chain(self.available_edit_controls(state))
                .collect()
        } else {
            self.visible_controls(state)
        };
        for c in controls {
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
        if !self.edit_mode {
            root.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Power profile")
                    .with_value(state.power_profile.label())
                    .with_description("Activate to use the next power profile")
                    .with_state(AccessState::new().focusable(true)),
            );
            for name in [
                "Customize Quick Settings",
                "Open Settings",
                "Open power menu",
            ] {
                root.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_state(AccessState::new().focusable(true)),
                );
            }
        }
        if self.edit_mode {
            root.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Reset defaults")
                    .with_state(AccessState::new().focusable(true)),
            );
            root.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Done")
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        root
    }
    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.panel),
            [0] if !self.edit_mode => Some(self.slider_geometry_for(self.panel, 0).0),
            [1] if !self.edit_mode => Some(self.slider_geometry_for(self.panel, 1).0),
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
    fn semantic_targets_cover_every_visible_root_control_and_edit_action() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.panel = PANEL;

        let targets = root.semantic_rects(&state);
        for required in [
            "quick_settings_edit",
            "quick_settings_settings",
            "quick_settings_power",
            "quick_settings_brightness",
            "quick_settings_volume",
            "quick_settings_network",
            "quick_settings_bluetooth",
            "quick_settings_night_light",
            "quick_settings_power_profile",
        ] {
            assert!(
                targets
                    .iter()
                    .any(|(name, rect)| name == required && rect.area() > 0.0),
                "missing semantic target {required}"
            );
        }

        root.edit_mode = true;
        let edit_targets = root.semantic_rects(&state);
        assert!(
            edit_targets
                .iter()
                .any(|(name, _)| name == "quick_settings_reset")
        );
        assert!(
            edit_targets
                .iter()
                .any(|(name, _)| name == "quick_settings_done")
        );
        assert!(
            edit_targets
                .iter()
                .any(|(name, _)| name == "quick_settings_edit_network")
        );
    }
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
    fn saved_airplane_choice_remains_removable_without_current_radio_hardware() {
        let root = QuickSettingsRoot::with_config(QuickSettingsConfig {
            controls: vec![
                QuickSettingsControl::Wifi,
                QuickSettingsControl::AirplaneMode,
                QuickSettingsControl::NightLight,
            ],
        });
        let state = QuickSettingsState::default();

        // Do not surface a dead default tile, but never strand a persisted
        // explicit choice where a person cannot remove it after hardware
        // changes.
        assert!(
            root.visible_controls(&state)
                .contains(&QuickSettingsControl::AirplaneMode)
        );
        assert!(
            root.edit_controls(&state)
                .contains(&QuickSettingsControl::AirplaneMode)
        );
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
    #[allow(
        clippy::field_reassign_with_default,
        reason = "the test varies one hardware capability from the default fixture to isolate accessibility visibility"
    )]
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
    fn opening_has_no_hidden_focus_and_first_tab_selects_brightness() {
        let mut root = QuickSettingsRoot::default();
        let state = QuickSettingsState::default();
        root.keyboard_focus = Some(4);

        root.reset_focus();
        assert_eq!(root.keyboard_focus, None);
        assert!(root.focus_next(&state, false));
        assert_eq!(root.keyboard_focus, Some(0));
        assert_eq!(
            root.activate_focused(PANEL, &mut QuickSettingsState::default()),
            RootAction::None
        );
        assert!(!root.edit_mode());
        assert_eq!(root.keyboard_focus, Some(0));
    }
    #[test]
    fn normal_focus_order_matches_visible_actions_and_accessibility_paths() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();

        let utility_start =
            QuickSettingsRoot::power_focus_index(root.visible_controls(&state).len()) + 1;
        for (path, expected) in [
            (utility_start, RootAction::LayoutChanged),
            (utility_start + 1, RootAction::OpenSettings),
            (utility_start + 2, RootAction::OpenPowerMenu),
        ] {
            assert!(root.focus_accessibility_path(&[path], &state));
            assert_eq!(root.keyboard_focus, Some(path));
            assert_eq!(root.activate_focused(PANEL, &mut state), expected);
            if path == utility_start {
                root.edit_mode = false;
            }
        }
    }
    #[test]
    fn power_profile_focus_matches_its_enter_action() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        let control_count = root.visible_controls(&state).len();
        let focus = QuickSettingsRoot::power_focus_index(control_count);

        // The renderer uses this same index for the visible Power profile
        // row. Enter must therefore cycle that row, not the preceding control.
        root.keyboard_focus = Some(focus);
        let previous = state.power_profile;
        let action = root.activate_focused(PANEL, &mut state);
        assert_eq!(
            focus,
            QuickSettingsRoot::CONTROL_FOCUS_START + control_count
        );
        assert_eq!(
            action,
            RootAction::Changed(PendingRootActions {
                power: Some(state.power_profile),
                ..Default::default()
            })
        );
        assert_ne!(state.power_profile, previous);
    }

    #[test]
    fn finishing_edit_clears_removed_done_focus_before_root_activation() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        root.edit_mode = true;
        let edit_count = root.edit_controls(&state).len();
        root.keyboard_focus = Some(edit_count.saturating_add(1));

        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::LayoutChanged
        );
        assert!(!root.edit_mode());
        assert_eq!(root.keyboard_focus, None);
    }
    #[test]
    fn edit_focus_order_covers_visible_rows_available_controls_and_footer() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.edit_mode = true;

        let active = root.active_edit_controls(&state);
        let available = root.available_edit_controls(&state);
        assert!(!active.is_empty());
        assert!(!available.is_empty());

        // The first available chip follows every active row and adds exactly
        // that visible control when activated.
        root.keyboard_focus = Some(active.len());
        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::LayoutChanged
        );
        assert!(root.config.controls.contains(&available[0]));

        // Done is the final focus stop, never an alias for a row or an
        // available chip.
        let edit_count = root.edit_controls(&state).len();
        root.keyboard_focus = Some(edit_count.saturating_add(1));
        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::LayoutChanged
        );
        assert!(!root.edit_mode());

        // Reset is immediately before Done and may change which optional
        // controls are active. Assert it independently so the test never
        // carries a now-stale focus index across that structural change.
        root.edit_mode = true;
        let edit_count = root.edit_controls(&state).len();
        root.keyboard_focus = Some(edit_count);
        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::LayoutChanged
        );
        assert_eq!(root.config, QuickSettingsConfig::default());
    }
    #[test]
    fn edit_add_remove_and_keyboard_reorder_change_model() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.edit_mode = true;
        root.keyboard_focus = Some(0);
        assert!(root.reorder_focused(&state, 1));
        assert_eq!(root.config.controls[1], QuickSettingsControl::Wifi);
        root.panel = PANEL;
        let airplane = root
            .available_edit_controls(&state)
            .iter()
            .position(|c| *c == QuickSettingsControl::AirplaneMode)
            .unwrap_or_default();
        let row = root.available_edit_chip(&state, airplane);
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
    #[allow(
        clippy::field_reassign_with_default,
        reason = "the drag test builds its controller fixture incrementally to isolate the manipulated controls"
    )]
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
    fn edit_header_keeps_help_section_and_first_control_visually_separate() {
        let mut root = QuickSettingsRoot::default();
        root.edit_mode = true;
        let state = QuickSettingsState::default();
        for scale in [1.0, 1.2] {
            root.set_layout_text_scale(scale);
            root.panel = Rect::new(0.0, 48.0, 400.0, 48.0 + root.preferred_height(&state));

            let help = root.edit_help_baseline();
            let section = root.edit_section_baseline();
            let title = root.panel.y0 + root.header_baseline_offset();
            let first_control = root.card(root.panel, 0, true);
            assert!(help - title >= root.scaled(18.0));
            assert!(section - help >= root.scaled(20.0));
            assert!(first_control.y0 - section >= root.scaled(10.0));
        }
    }

    #[test]
    fn edit_help_copy_uses_font_safe_keyboard_names() {
        assert_eq!(
            QuickSettingsRoot::EDIT_HELP_COPY,
            "Drag to reorder · Ctrl + Up/Down"
        );
        assert!(!QuickSettingsRoot::EDIT_HELP_COPY.contains('↑'));
        assert!(!QuickSettingsRoot::EDIT_HELP_COPY.contains('↓'));
    }

    #[test]
    fn edit_remove_action_has_a_clear_wide_target() {
        let mut root = QuickSettingsRoot::default();
        root.edit_mode = true;
        root.panel = PANEL;
        let row = root.card(PANEL, 0, true);
        let action = root.edit_action_rect(row);
        assert_eq!(action.width(), QuickSettingsRoot::EDIT_ACTION_WIDTH);
        assert!(action.x0 > row.x0);
        assert!(action.x1 < row.x1);
        assert!(action.y0 > row.y0 && action.y1 < row.y1);
    }
    #[test]
    fn edit_row_body_reorders_and_only_explicit_action_removes() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.edit_mode = true;
        root.panel = PANEL;

        let row = root.card(PANEL, 1, true);
        let bluetooth = QuickSettingsControl::Bluetooth;
        assert!(root.config.controls.contains(&bluetooth));
        assert_eq!(
            root.edit_click(Point::new(row.center().x, row.center().y), &state),
            RootAction::None
        );
        assert!(root.config.controls.contains(&bluetooth));
        let _ = root.finish_drag(&state);

        assert_eq!(
            root.edit_click(root.edit_action_rect(row).center(), &state),
            RootAction::LayoutChanged
        );
        assert!(!root.config.controls.contains(&bluetooth));
    }
    #[test]
    fn odd_control_count_uses_a_balanced_full_width_last_card() {
        let panel = Rect::new(0., 48., 400., 520.);
        let root = QuickSettingsRoot::default();
        let full = root.control_card(panel, 2, 3);
        assert_eq!(full.x0, panel.x0 + QuickSettingsRoot::PAD);
        assert_eq!(full.x1, panel.x1 - QuickSettingsRoot::PAD);
        assert_eq!(full.width(), panel.width() - QuickSettingsRoot::PAD * 2.0);
    }
    #[allow(
        clippy::field_reassign_with_default,
        reason = "the touch-target test sets only the panel geometry on an otherwise default root fixture"
    )]
    #[test]
    fn bottom_utilities_keep_independent_touch_targets() {
        let mut root = QuickSettingsRoot::default();
        let state = QuickSettingsState::default();
        root.panel = Rect::new(0., 48., 400., 520.);
        let actions: Vec<_> = (0..3)
            .map(|index| root.utility(root.panel, &state, index))
            .collect();
        for action in &actions {
            assert!(action.width() >= 44. && action.height() >= 44.);
        }
        for (index, action) in actions.iter().enumerate() {
            for earlier in &actions[..index] {
                let overlap = action.intersect(*earlier);
                assert!(overlap.width() <= 0. || overlap.height() <= 0.);
            }
        }
        assert!(actions[0].width() > 100.0);
    }

    #[test]
    fn compact_root_reclaims_space_without_clipping_the_power_profile() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        state.vpns.push(os_actions::VpnConnectionInfo {
            name: "Test VPN".into(),
            uuid: "test-vpn".into(),
            active: false,
            kind: "vpn".into(),
        });
        let height = root.preferred_height(&state);
        assert!(
            height <= 320.0,
            "control-first root should stay materially below the old 416px panel"
        );
        let panel = Rect::new(0.0, 48.0, 400.0, 48.0 + height);
        root.panel = panel;
        assert!(root.power_geometry(panel, &state).y1 <= panel.y1);
    }
    #[test]
    fn power_utility_keeps_a_clear_root_target() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        let panel = Rect::new(0., 48., 400., 520.);
        root.panel = panel;
        assert_eq!(
            root.handle_click(root.utility(panel, &state, 2).center(), panel, &mut state),
            RootAction::OpenPowerMenu
        );
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
    fn large_text_fits_every_root_control_on_a_960_by_540_desktop() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.set_layout_text_scale(1.3);
        let available = 540.0 - 48.0 - 16.0;
        root.fit_root_layout_to_height(&state, available);
        let panel = Rect::new(0.0, 48.0, 400.0, 48.0 + available);

        assert!(root.preferred_height(&state) <= available + f64::EPSILON);
        assert!(root.power_geometry(panel, &state).y1 <= panel.y1 + f64::EPSILON);
        for i in 0..root.visible_controls(&state).len() {
            assert!(
                root.control_card(panel, i, root.visible_controls(&state).len())
                    .y1
                    <= panel.y1
            );
        }
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
