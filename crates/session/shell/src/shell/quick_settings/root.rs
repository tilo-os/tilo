//! Compact, configurable root page for the Quick Settings popover.
//!
//! The panel presents current controls, not a dashboard of every capability.
//! Arrangement is durable user intent while hardware availability is applied
//! only when the panel is rendered.

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::quick_settings::drill_in::{
    CONTROL_INSET, CONTROL_PAD, GLYPH, PANEL_PAD, RELATED_GAP, ROW_HEIGHT, SECTION_GAP,
    SIBLING_GAP, TARGET, TOGGLE_H, TOGGLE_W, centred_text_top, control_radius, leading_glyph,
    paint_control, trailing_glyph, trailing_toggle, type_size,
};
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
    #[allow(
        clippy::cast_possible_truncation,
        reason = "text sizing ratios and clamped slider fractions use f32 rendering and control precision"
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

/// Root interaction state.  Editing uses the shared Tilo reorder controller;
/// it never serialises paint geometry, only stable control identifiers.
pub struct QuickSettingsRoot {
    panel: Rect,
    // Text scale changes the amount of vertical room a two-line control
    // needs.  The host supplies this before it resolves the popover rect,
    // so large-text screenshots and the live frame use the same geometry.
    layout_scale: f64,
    // The user's full text scale, unclamped. Chrome spacing stops growing
    // at `layout_scale`, but a row must still be tall enough for the type
    // it carries (HIG §7: no clipped glyphs or overlapping text), so row
    // heights read this and the toolkit's line metrics, never a literal.
    text_scale: f64,
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
    // One rhythm for the page (HIG §7b, §9.6), shared with every drill-in
    // through `quick_settings_drill_in`: the same relationship always gets
    // the same step, and every value is a token expression.
    /// The title band (HIG §9.6: the Bar height); the Edit button sits in it.
    const HEADER: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
    /// The gap under a tile's glyph before its caption (`icon_label_gap`).
    const GLYPH_GAP: f64 = ui::tokens::Metrics::BASE.icon_label_gap as f64;
    /// The edit-mode footer buttons: the shared `Button` height.
    const EDIT_FOOTER: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
    /// How far the dragged ghost lifts off its slot, and its rail's width
    /// (`s1`).
    const LIFT: f64 = ui::tokens::Spacing::BASE.s1 as f64;

    const PAD: f64 = 16.0;
    const ACTION_COUNT: usize = 4;
    const SLIDER_FOCUS_START: usize = 0;
    const CONTROL_FOCUS_START: usize = Self::SLIDER_FOCUS_START + 2;
    const EDIT_ACTION_WIDTH: f64 = 76.0; // Text affordance width, not a spacing step.
    const EDIT_HELP_COPY: &'static str = "Drag to reorder · Ctrl + Up/Down";

    /// Single-line height of a chrome type role at the user's text scale.
    ///
    /// Geometry is resolved before a frame has a renderer — the host sizes
    /// and hit-tests the popover from these helpers — so rows take their
    /// minimum height from the toolkit's own line metrics for the default
    /// type scale rather than from a literal. The draw path then centres
    /// the text it actually measured inside that row, so a theme with a
    /// different type scale still paints correctly.
    fn line_height(&self, base_px: f32) -> f64 {
        f64::from(ui::text_render::TextStyle::new(base_px * f32v(self.text_scale)).line_height_px())
    }
    /// Control label: `scale.sm` (HIG §9.6).
    fn label_line_height(&self) -> f64 {
        self.line_height(ui::tokens::TypeScale::default().sm)
    }
    /// Control sub-line / caption: `scale.xs` (HIG §9.6).
    fn caption_line_height(&self) -> f64 {
        self.line_height(ui::tokens::TypeScale::default().xs)
    }
    /// Tile: glyph, `icon_label_gap` and caption on the control padding.
    fn action_height_at(&self, layout_scale: f64) -> f64 {
        ((2.0 * CONTROL_PAD + Self::GLYPH_GAP) * layout_scale
            + Self::glyph_at(layout_scale)
            + self.caption_line_height())
        .max(44.0)
    }
    fn action_height(&self) -> f64 {
        self.action_height_at(self.layout_scale)
    }
    /// Card: the icon row (glyph or switch, whichever is taller), one
    /// related step, then the measured label and sub-line, on the control
    /// padding.
    fn card_height_at(&self, layout_scale: f64) -> f64 {
        (2.0 * CONTROL_PAD + RELATED_GAP) * layout_scale
            + Self::glyph_at(layout_scale)
            + self.label_line_height()
            + self.caption_line_height()
    }
    fn card_height(&self) -> f64 {
        self.card_height_at(self.layout_scale)
    }
    /// Power row: label and detail on the control padding, never under the
    /// Control height.
    fn power_height_at(&self, layout_scale: f64) -> f64 {
        (ROW_HEIGHT * layout_scale).max(
            2.0 * CONTROL_PAD * layout_scale
                + self.label_line_height()
                + self.caption_line_height(),
        )
    }
    fn power_height(&self) -> f64 {
        self.power_height_at(self.layout_scale)
    }
    /// Edit-mode row: its label on the control padding, never under the
    /// Control height.
    fn edit_row_height_at(&self, layout_scale: f64) -> f64 {
        (ROW_HEIGHT * layout_scale).max(2.0 * CONTROL_PAD * layout_scale + self.label_line_height())
    }
    fn edit_row_height(&self) -> f64 {
        self.edit_row_height_at(self.layout_scale)
    }
    /// A section caption band in edit mode ("ACTIVE CONTROLS", "AVAILABLE"):
    /// control padding above the caption line, one related step below it.
    fn caption_band_at(&self, layout_scale: f64) -> f64 {
        (CONTROL_PAD + RELATED_GAP) * layout_scale + self.caption_line_height()
    }
    fn caption_band(&self) -> f64 {
        self.caption_band_at(self.layout_scale)
    }
    /// The title band: panel padding down to the first action tile. The
    /// Edit button lives inside it, so a title centred here shares its
    /// centre line (HIG §9.6: Bar/Panel header height 40).
    fn header_rect(&self) -> Rect {
        let pad = self.scaled(PANEL_PAD);
        Rect::new(
            self.panel.x0 + pad,
            self.panel.y0 + pad,
            self.panel.x1 - pad,
            self.header_bottom(self.panel),
        )
    }
    /// Bottom edge of the title band for `panel` — the line every section
    /// below the header measures from. One definition, so the tiles, the
    /// slider block, the edit grid and the header itself cannot drift apart.
    /// Takes the panel explicitly because the compatibility geometry helpers
    /// are called before `self.panel` is laid out.
    fn header_bottom(&self, panel: Rect) -> f64 {
        panel.y0 + self.scaled(PANEL_PAD) + self.header_height_at(self.layout_scale)
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
            text_scale: 1.0,
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
        // Row heights still follow the full scale: see `line_height`.
        self.text_scale = f64::from(scale).clamp(0.5, 4.0);
    }
    /// Fit root-page spatial chrome to the available popover height while
    /// leaving text itself at the user's full theme scale. The same effective
    /// factor drives drawing and hit testing through every geometry helper.
    pub fn fit_root_layout_to_height(&mut self, state: &QuickSettingsState, height: f64) {
        // Preserve the refined root's compact footprint even when the host
        // could offer the old 416px panel. Text remains at its full scale;
        // only spatial chrome participates in this fit.
        let height = height.min(320.0);
        let unscaled = self.preferred_height_at(state, 1.0);
        self.layout_scale = self.layout_scale.min((height / unscaled).clamp(0.2, 1.2));
        // Rows that had to grow for their type do not shrink with the chrome
        // scale, so the proportional estimate above can still overshoot.
        // Walk the chrome down to the same floor until the whole root fits.
        while self.preferred_height(state) > height && self.layout_scale > 0.2 {
            self.layout_scale = (self.layout_scale - 0.01).max(0.2);
        }
    }
    fn target_at(scale: f64) -> f64 {
        (TARGET * scale).max(TARGET)
    }
    fn glyph_at(scale: f64) -> f64 {
        (GLYPH * scale).max(12.0)
    }
    fn header_height_at(&self, scale: f64) -> f64 {
        (Self::HEADER * scale)
            .max(Self::target_at(scale))
            .max(self.line_height(ui::tokens::TypeScale::default().base))
    }
    fn sliders_height_at(scale: f64) -> f64 {
        (2.0 * CONTROL_PAD + RELATED_GAP) * scale + 2.0 * Self::target_at(scale)
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
        let metrics = self.edit_list_metrics(panel, self.config.controls.len());
        let _ = self
            .edit_drag
            // Stop part-way through the destination slot rather than exactly
            // centred on it. That exposes the same lifted ghost, insertion
            // rail and sibling reflow a person sees while dragging, instead
            // of making a visual regression scene resemble mere focus.
            .pointer_moved(
                Point::new(
                    start.x,
                    start.y + metrics.pitch() + self.scaled(RELATED_GAP),
                ),
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
        self.preferred_height_at(state, self.layout_scale)
    }
    /// The panel height the page wants at `layout_scale`: one definition for
    /// the fit-to-height estimate (at 1×) and the live geometry, so the two
    /// can never disagree about a section gap.
    fn preferred_height_at(&self, state: &QuickSettingsState, layout_scale: f64) -> f64 {
        let scaled = |value: f64| value * layout_scale;
        if self.edit_mode {
            let rows = n(self.active_edit_controls(state).len());
            let available = if self.available_edit_controls(state).is_empty() {
                0.0
            } else {
                self.caption_band_at(layout_scale) + scaled(TARGET + SECTION_GAP)
            };
            scaled(PANEL_PAD * 2.0)
                + self.header_height_at(layout_scale)
                + 2.0 * self.caption_band_at(layout_scale)
                + rows * self.edit_row_height_at(layout_scale)
                + (rows - 1.0).max(0.0) * scaled(RELATED_GAP)
                + scaled(SECTION_GAP)
                + available
                + scaled(Self::EDIT_FOOTER)
        } else {
            let rows = n(self.visible_controls(state).len().div_ceil(2));
            // Title band → tiles → sliders → cards → power: one section gap
            // before each section that exists on this machine.
            let sections = if rows > 0.0 { 4.0 } else { 3.0 };
            scaled(PANEL_PAD * 2.0 + SECTION_GAP * sections)
                + self.header_height_at(layout_scale)
                + Self::sliders_height_at(layout_scale)
                + self.action_height_at(layout_scale)
                + rows * self.card_height_at(layout_scale)
                + (rows - 1.0).max(0.0) * scaled(SIBLING_GAP)
                + self.power_height_at(layout_scale)
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
    /// Top of the action tiles: one section gap under the title band.
    fn actions_top(&self, panel: Rect) -> f64 {
        self.header_bottom(panel) + self.scaled(SECTION_GAP)
    }
    /// Top of the slider block: one section gap under the tiles.
    fn sliders_top(&self, panel: Rect) -> f64 {
        self.actions_top(panel) + self.action_height() + self.scaled(SECTION_GAP)
    }
    /// The slider block: both bands on the panel padding.
    fn sliders_rect(&self, panel: Rect) -> Rect {
        let top = self.sliders_top(panel);
        Rect::new(
            panel.x0 + self.scaled(PANEL_PAD),
            top,
            panel.x1 - self.scaled(PANEL_PAD),
            top + Self::sliders_height_at(self.layout_scale),
        )
    }
    /// Top of the control cards: one section gap under the slider block.
    fn grid_top(&self, panel: Rect) -> f64 {
        self.sliders_top(panel)
            + Self::sliders_height_at(self.layout_scale)
            + self.scaled(SECTION_GAP)
    }
    fn edit_grid_top(&self, panel: Rect) -> f64 {
        self.header_bottom(panel) + 2.0 * self.caption_band()
    }
    #[cfg(test)]
    fn header_baseline_offset(&self) -> f64 {
        self.scaled(PANEL_PAD) + self.header_height_at(self.layout_scale) / 2.0
            - self.line_height(ui::tokens::TypeScale::default().base) / 2.0
    }
    /// Edit rows as the shared drag controller sees them: Control-height
    /// rows one related step apart.
    fn edit_list_metrics(&self, panel: Rect, count: usize) -> ListMetrics {
        ListMetrics::uniform(
            self.edit_grid_top(panel),
            self.edit_row_height(),
            self.scaled(RELATED_GAP),
            count,
        )
    }
    /// Top of the "AVAILABLE" section: one section gap under the last row.

    fn edit_help_baseline(&self) -> f64 {
        self.header_bottom(self.panel) + self.scaled(CONTROL_PAD)
    }

    fn edit_section_baseline(&self) -> f64 {
        self.edit_help_baseline() + self.caption_band()
    }
    fn available_edit_top(&self, state: &QuickSettingsState) -> f64 {
        let active = n(self.active_edit_controls(state).len());
        self.edit_grid_top(self.panel)
            + active * self.edit_row_height()
            + (active - 1.0).max(0.0) * self.scaled(RELATED_GAP)
            + self.scaled(SECTION_GAP)
    }
    /// The add-chips under the "AVAILABLE" caption: pointer-floor tall,
    /// siblings one sibling gap apart across the panel padding. One helper
    /// feeds paint, hit-testing and the tests.
    fn available_chips(&self, state: &QuickSettingsState) -> Vec<(QuickSettingsControl, Rect)> {
        let available = self.available_edit_controls(state);
        if available.is_empty() {
            return Vec::new();
        }
        let top = self.available_edit_top(state) + self.caption_band();
        let gap = self.scaled(SIBLING_GAP);
        let x0 = self.panel.x0 + self.scaled(PANEL_PAD);
        let width = (self.panel.width()
            - self.scaled(PANEL_PAD * 2.0)
            - gap * n(available.len().saturating_sub(1)))
            / n(available.len());
        available
            .into_iter()
            .enumerate()
            .map(|(i, control)| {
                let x = x0 + n(i) * (width + gap);
                (
                    control,
                    Rect::new(x, top, x + width, top + self.scaled(TARGET)),
                )
            })
            .collect()
    }

    fn available_edit_chip(&self, state: &QuickSettingsState, index: usize) -> Rect {
        self.available_chips(state)
            .get(index)
            .map_or(Rect::ZERO, |(_, rect)| *rect)
    }

    fn card(&self, panel: Rect, i: usize, edit: bool) -> Rect {
        if edit {
            let row_h = self.edit_row_height();
            let y = self.edit_grid_top(panel) + n(i) * (row_h + self.scaled(RELATED_GAP));
            return Rect::new(
                panel.x0 + self.scaled(PANEL_PAD),
                y,
                panel.x1 - self.scaled(PANEL_PAD),
                y + row_h,
            );
        }
        let col = i % 2;
        let row = i / 2;
        let pad = self.scaled(PANEL_PAD);
        let gap = self.scaled(SIBLING_GAP);
        let card = self.card_height();
        let w = (panel.width() - 2.0 * pad - gap) / 2.0;
        let x = panel.x0 + pad + n(col) * (w + gap);
        let y = self.grid_top(panel) + n(row) * (card + gap);
        Rect::new(x, y, x + w, y + card)
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
        let pad = self.scaled(PANEL_PAD);
        let gap = self.scaled(SIBLING_GAP);
        let card = self.card_height();
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
        self.card_drill_in_rect(card)
    }
    /// A card's icon row: the band its glyph and its switch or chevron are
    /// centred on, on the control padding.
    fn card_icon_row(&self, card: Rect) -> Rect {
        let pad = self.scaled(CONTROL_PAD);
        Rect::new(
            card.x0,
            card.y0 + pad,
            card.x1,
            card.y0 + pad + Self::glyph_at(self.layout_scale),
        )
    }
    /// The drill-in target on a card: the chevron's slot widened to the
    /// pointer floor, clipped to the card.
    fn card_drill_in_rect(&self, card: Rect) -> Rect {
        let chevron = trailing_glyph(
            self.card_icon_row(card),
            self.scaled(CONTROL_INSET),
            Self::glyph_at(self.layout_scale),
        );
        let target = Self::target_at(self.layout_scale);
        let half = target / 2.0;
        let center = Point::new(
            chevron.center().x.clamp(card.x0 + half, card.x1 - half),
            chevron.center().y.clamp(card.y0 + half, card.y1 - half),
        );
        Rect::from_center_size(center, (target, target))
    }
    /// Compatibility geometry for integration tests; the renderer uses the
    /// current control vector above.
    pub fn toggle_geometry(panel: Rect, index: usize) -> Rect {
        Self::default().card(panel, index, false)
    }
    pub fn slider_geometry(panel: Rect, i: usize) -> (Rect, f64) {
        Self::default().slider_geometry_for(panel, i)
    }
    /// Slider `i`'s face: after the leading glyph slot, the rest of the
    /// block's width, at least one minimum pointer target tall (HIG §9.6). The
    /// two faces sit one related step apart inside the control padding, so
    /// neither touches the other or the block's edge. One helper feeds
    /// paint, hit-testing and the accessibility tree.
    fn slider_face_rect(&self, panel: Rect, i: usize) -> Rect {
        let block = self.sliders_rect(panel);
        let y = block.y0
            + self.scaled(CONTROL_PAD)
            + n(i) * (Self::target_at(self.layout_scale) + self.scaled(RELATED_GAP));
        Rect::new(
            block.x0 + self.scaled(CONTROL_INSET + GLYPH + CONTROL_INSET),
            y,
            block.x1 - self.scaled(CONTROL_INSET),
            y + Self::target_at(self.layout_scale),
        )
    }
    /// Slider `i`'s painted track — the shared face's own `track_rect`, so
    /// value-from-pointer maths and paint agree — and the top of its band.
    fn slider_geometry_for(&self, panel: Rect, i: usize) -> (Rect, f64) {
        let face = self.slider_face_rect(panel, i);
        (SliderFace::new(face, 0.0).track_rect(), face.y0)
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
        // One section gap under the cards — or straight under the sliders
        // when this machine has no controls to show.
        let grid_top = self.grid_top(panel);
        let y = if rows == 0 {
            grid_top
        } else {
            grid_top
                + n(rows) * self.card_height()
                + n(rows.saturating_sub(1)) * self.scaled(SIBLING_GAP)
                + self.scaled(SECTION_GAP)
        };
        Rect::new(
            panel.x0 + self.scaled(PANEL_PAD),
            y,
            panel.x1 - self.scaled(PANEL_PAD),
            y + self.power_height(),
        )
    }
    fn action(&self, i: usize) -> Rect {
        let pad = self.scaled(PANEL_PAD);
        let gap = self.scaled(SIBLING_GAP);
        let gaps = n(Self::ACTION_COUNT.saturating_sub(1));
        let w = (self.panel.width() - 2.0 * pad - gaps * gap) / n(Self::ACTION_COUNT);
        let x = self.panel.x0 + pad + n(i) * (w + gap);
        let y = self.actions_top(self.panel);
        Rect::new(x, y, x + w, y + self.action_height())
    }
    /// The Edit button: a pointer-floor square on the trailing panel
    /// padding, centred in the title band.
    fn power_focus_index(control_count: usize) -> usize {
        // Keep the trailing power-profile row after the two sliders and all
        // visible controls. This is also the index used by keyboard
        // activation; using a separate offset here makes the painted focus
        // ring target a different control from Enter.
        Self::CONTROL_FOCUS_START.saturating_add(control_count)
    }
    fn utility(&self, panel: Rect, state: &QuickSettingsState, index: usize) -> Rect {
        let _ = (panel, state);
        match index {
            // Edit is painted as a quiet square in the title band, but owns
            // the same wide pointer target as a labelled action tile.
            0 => {
                let edit = self.edit_rect();
                Rect::from_center_size(edit.center(), (self.action(0).width(), 44.0))
            }
            1 => self.action(0),
            2 => self.action(3),
            _ => Rect::ZERO,
        }
    }
    fn edit_rect(&self) -> Rect {
        let pad = self.scaled(PANEL_PAD);
        let target = Self::target_at(self.layout_scale);
        let y = self.panel.y0 + pad + (self.header_height_at(self.layout_scale) - target) / 2.0;
        Rect::new(
            self.panel.x1 - pad - target,
            y,
            self.panel.x1 - pad,
            y + target,
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
            // The pointer target is the painted face — the same rect the
            // accessibility tree publishes — not a second band around the track.
            if self.slider_face_rect(panel, i).contains(point) {
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
                && self.card_drill_in_rect(card).contains(point)
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
    /// The footer's two buttons: one section gap under the last section,
    /// one sibling gap apart.
    fn edit_reset(&self, state: &QuickSettingsState) -> Rect {
        let y = if self.available_edit_controls(state).is_empty() {
            self.available_edit_top(state)
        } else {
            self.available_edit_top(state) + self.caption_band() + self.scaled(TARGET + SECTION_GAP)
        };
        Rect::new(
            self.panel.x0 + self.scaled(PANEL_PAD),
            y,
            self.panel.center().x - self.scaled(SIBLING_GAP) / 2.0,
            y + self.scaled(Self::EDIT_FOOTER),
        )
    }
    fn edit_done(&self, state: &QuickSettingsState) -> Rect {
        let reset = self.edit_reset(state);
        Rect::new(
            self.panel.center().x + self.scaled(SIBLING_GAP) / 2.0,
            reset.y0,
            self.panel.x1 - self.scaled(PANEL_PAD),
            reset.y1,
        )
    }
    fn edit_click(&mut self, point: Point, state: &QuickSettingsState) -> RootAction {
        let active = self.active_edit_controls(state);
        for (i, c) in active.iter().copied().enumerate() {
            let row = self.card(self.panel, i, true);
            if !row.contains(point) {
                continue;
            }
            let action = self.edit_action_rect(row);
            if !c.required() && action.contains(point) {
                self.config.controls.retain(|x| *x != c);
                self.save_config();
                return RootAction::LayoutChanged;
            }
            if self.config.controls.contains(&c) {
                self.edit_drag.begin(c, i, point.y - row.y0, point);
                return RootAction::None;
            }
            return RootAction::None;
        }
        for (control, chip) in self.available_chips(state) {
            if chip.contains(point) {
                self.config.controls.push(control);
                self.save_config();
                return RootAction::LayoutChanged;
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
                &self.edit_list_metrics(panel, self.edit_controls(state).len()),
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
        // Panel role (HIG §9.1/§9.2): the host paints the one Panel surface
        // — `panel_ground` with its derived `panel_border` — and the root
        // paints nothing under its controls. A second fill here (formerly
        // `surfaces.raised`) covered the inner half of that border and made
        // the tiles' own `raised` faces vanish into it.
        if self.edit_mode {
            self.header(scene, transform, text, theme, cursor);
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
        // Panel title (HIG §9.6): `scale.base` semibold, centred on the
        // header band by its measured height so it shares a centre line
        // with the Edit button instead of sitting on an authored baseline
        // that large text pushed into the action tiles.
        let title = if self.edit_mode {
            "Edit controls"
        } else {
            "Quick Settings"
        };
        let title_size = type_size(text, theme.tokens.typography.scale.base);
        let header = self.header_rect();
        let (_, title_h) = text.measure_weighted(title, title_size, None, TextWeight::Semibold);
        text.draw_text_weighted(
            scene,
            title,
            title_size,
            colors.text_primary,
            transform
                .pre_translate((header.x0, header.center().y - f64::from(title_h) / 2.).into()),
            None,
            TextWeight::Semibold,
        );
        if !self.edit_mode {
            let r = self.edit_rect();
            let icon = ui::icons::Icon::Pencil;
            let name = "Edit controls";
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(r, "")
                    .ghost()
                    .hovered(cursor.is_some_and(|p| r.contains(p)))
                    .focused(self.keyboard_focus == Some(0))
                    .accessible_name(name),
            );
            // The standard glyph centred in the pointer-floor button.
            let glyph = Self::glyph_at(self.layout_scale);
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(r.center(), (glyph, glyph)),
                icon,
                colors.text_primary,
                1.5,
            );
        }
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
        let glyph = Self::glyph_at(self.layout_scale);
        let inset = self.scaled(CONTROL_INSET);
        // Destinations remain at the top, in the place established by the
        // original GNOME-inspired shell, rather than becoming unexplained
        // footer glyphs.  The visible label is part of the control: a
        // tooltip or accessibility tree is not sufficient discoverability.
        for (i, (icon, label)) in [
            (ui::icons::Icon::Settings2, "Settings"),
            (ui::icons::Icon::Shield, "Privacy"),
            (ui::icons::Icon::History, "History"),
            (ui::icons::Icon::Power, "Power"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = self.action(i);
            let power = i == 3;
            // Focus index 0 is the Edit button and the tiles follow it
            // (`activate_focused`), so tile `i` lights at `i + 1`.
            paint_control(
                scene,
                transform,
                theme,
                r,
                cursor.is_some_and(|p| r.contains(p)),
                self.keyboard_focus == Some(i.saturating_add(1)),
            );
            let color = if power {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            // These four labels are fixed, high-value shell destinations.
            // Keep every word intact; at an unusually large text scale we
            // reduce this one caption before we truncate its meaning.
            let max_label_width = (r.width() - 2.0 * inset).max(1.);
            let label_size = type_size(text, theme.tokens.typography.scale.xs);
            let (unscaled_label_width, _) =
                text.measure_weighted(label, label_size, None, TextWeight::Medium);
            let label_size = if f64::from(unscaled_label_width) > max_label_width {
                label_size * f32v(max_label_width / f64::from(unscaled_label_width))
            } else {
                label_size
            };
            let (label_width, label_height) =
                text.measure_weighted(label, label_size, None, TextWeight::Medium);
            // Glyph, `icon_label_gap`, caption: one measured block centred
            // in the tile (HIG §7), never a glyph pinned to the top edge
            // with the caption riding the bottom one.
            let gap = self.scaled(Self::GLYPH_GAP);
            let block_h = glyph + gap + f64::from(label_height);
            let block_top = r.y0 + (r.height() - block_h) / 2.0;
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(
                    r.center().x - glyph / 2.0,
                    block_top,
                    r.center().x + glyph / 2.0,
                    block_top + glyph,
                ),
                icon,
                color,
                1.5,
            );
            text.draw_text_weighted(
                scene,
                label,
                label_size,
                color,
                transform.pre_translate(
                    (
                        r.center().x - f64::from(label_width) / 2.,
                        block_top + glyph + gap,
                    )
                        .into(),
                ),
                None,
                TextWeight::Medium,
            );
        }
        // The slider block is one Control surface holding two pointer
        // bands, each with its glyph on the leading inset of the band.
        let sliders = self.sliders_rect(self.panel);
        paint_control(scene, transform, theme, sliders, false, false);
        for i in 0..2 {
            let face = self.slider_face_rect(self.panel, i);
            let available = if i == 0 {
                state.brightness_available
            } else {
                state.volume_available
            };
            let band = Rect::new(sliders.x0, face.y0, sliders.x1, face.y1);
            ui::icons::draw_icon(
                scene,
                transform,
                leading_glyph(band, inset, glyph),
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
        // The power row: battery glyph, profile and detail, chevron — all
        // on the row's centre line.
        let r = self.power_geometry(self.panel, state);
        let power_focus = Self::power_focus_index(self.visible_controls(state).len());
        paint_control(
            scene,
            transform,
            theme,
            r,
            cursor.is_some_and(|p| r.contains(p)),
            self.keyboard_focus == Some(power_focus),
        );
        let battery = leading_glyph(r, inset, glyph);
        ui::icons::draw_icon(
            scene,
            transform,
            battery,
            ui::icons::Icon::Battery,
            colors.semantic.success,
            1.5,
        );
        let chevron = trailing_glyph(r, inset, glyph);
        ui::icons::draw_icon(
            scene,
            transform,
            chevron,
            ui::icons::Icon::ChevronRight,
            colors.text_dim,
            1.5,
        );
        let text_x = battery.x1 + inset;
        let text_width = (chevron.x0 - self.scaled(RELATED_GAP) - text_x).max(1.0);
        let power_label_size = type_size(text, theme.tokens.typography.scale.sm);
        let detail_size = type_size(text, theme.tokens.typography.scale.xs);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let power_label = text.elide_to_width(
            state.power_profile.label(),
            power_label_size,
            text_width as f32,
        );
        let battery_detail = if state.battery.present {
            format!("{}% battery", state.battery.capacity)
        } else {
            "AC power".to_string()
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let battery_detail = text.elide_to_width(&battery_detail, detail_size, text_width as f32);
        let (_, power_label_h) =
            text.measure_weighted(&power_label, power_label_size, None, TextWeight::Semibold);
        let (_, detail_h) = text.measure(&battery_detail, detail_size, None);
        // Label and detail are one measured block on the row's centre line
        // (HIG §7a: measure and draw with one style; the detail follows the
        // label line, never a fixed offset the label can outgrow).
        let block_top = centred_text_top(r, f64::from(power_label_h) + f64::from(detail_h));
        text.draw_text_weighted(
            scene,
            &power_label,
            power_label_size,
            colors.text_primary,
            transform.pre_translate((text_x, block_top).into()),
            None,
            TextWeight::Semibold,
        );
        text.draw_text(
            scene,
            &battery_detail,
            detail_size,
            colors.text_dim,
            transform.pre_translate((text_x, block_top + f64::from(power_label_h)).into()),
            None,
        );
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
        paint_control(
            scene,
            transform,
            theme,
            r,
            cursor.is_some_and(|p| r.contains(p)),
            self.keyboard_focus == Some(Self::CONTROL_FOCUS_START.saturating_add(i)),
        );
        let inset = self.scaled(CONTROL_INSET);
        let glyph = Self::glyph_at(self.layout_scale);
        // Glyph and switch (or chevron) share the icon row's centre line;
        // label and sub-line follow one related step below it (HIG §7).
        let icon_row = self.card_icon_row(r);
        ui::icons::draw_icon(
            scene,
            transform,
            leading_glyph(icon_row, inset, glyph),
            Self::icon(c, s),
            if on {
                colors.accent[5]
            } else {
                colors.text_primary
            },
            1.5,
        );
        let label_size = type_size(text, theme.tokens.typography.scale.sm);
        let sub_size = type_size(text, theme.tokens.typography.scale.xs);
        let text_width = (r.width() - 2.0 * inset).max(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let label = text.elide_to_width(Self::rendered_label(c, s), label_size, text_width as f32);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let sub = text.elide_to_width(Self::subline(c, s), sub_size, text_width as f32);
        let label_y = icon_row.y1 + self.scaled(RELATED_GAP);
        let (_, label_h) = text.measure_weighted(&label, label_size, None, TextWeight::Semibold);
        text.draw_text_weighted(
            scene,
            &label,
            label_size,
            colors.text_primary,
            transform.pre_translate((r.x0 + inset, label_y).into()),
            None,
            TextWeight::Semibold,
        );
        // The sub-line sits under the measured label rather than at a fixed
        // offset the label outgrows at large text (HIG §7a).
        text.draw_text(
            scene,
            &sub,
            sub_size,
            colors.text_dim,
            transform.pre_translate((r.x0 + inset, label_y + f64::from(label_h)).into()),
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
                trailing_glyph(icon_row, inset, glyph),
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
                    trailing_toggle(
                        icon_row,
                        inset,
                        self.scaled(TOGGLE_W),
                        self.scaled(TOGGLE_H),
                    ),
                    on,
                ),
            );
        }
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
        let pad = self.scaled(PANEL_PAD);
        let caption_size = type_size(text, theme.tokens.typography.scale.xs);
        // Section caption on the control padding under the title band.
        text.draw_text(
            scene,
            Self::EDIT_HELP_COPY,
            caption_size,
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
            caption_size,
            colors.text_dim,
            transform.pre_translate((self.panel.x0 + pad, self.edit_section_baseline()).into()),
            None,
        );
        let active = self.active_edit_controls(state);
        let metrics = self.edit_list_metrics(self.panel, active.len());
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
            // Edit rows are indexed from 0 in the focus order
            // (`activate_focused`, `reorder_focused`), so row `i` lights at `i`.
            paint_control(
                scene,
                transform,
                theme,
                r,
                cursor.is_some_and(|p| r.contains(p)),
                self.keyboard_focus == Some(i),
            );
            self.draw_edit_row_content(scene, transform, text, theme, cursor, r, c);
        }
        if let Some(ghost) =
            self.edit_drag
                .ghost_rect(self.panel.x0 + pad, self.panel.x1 - pad, &metrics)
        {
            // The floating item retains its identity, not merely a coloured
            // insertion hole: it is the one lifted control (the active
            // selection tier, HIG §9.3) casting the `e4` a dragged object
            // casts, with a live accent rail on its leading edge.
            // Offset the presentation very slightly from the pointer's
            // exact hit geometry. Otherwise a slow vertical drag can land
            // exactly on a row and look indistinguishable from a resting
            // row. The controller and drop calculation retain the
            // unmodified ghost rectangle above.
            let lift = self.scaled(Self::LIFT);
            let destination_top = self
                .edit_drag
                .insertion()
                .map(|index| metrics.slot_lead(index) + lift);
            let ghost_y0 =
                destination_top.map_or_else(|| ghost.y0 - lift, |top| (ghost.y0 - lift).max(top));
            let ghost = Rect::new(
                ghost.x0 + lift,
                ghost_y0,
                ghost.x1 - lift,
                ghost_y0 + metrics.item_extent,
            );
            let radius = control_radius(theme, ghost);
            ui::surface::shadow(scene, transform, ghost, radius, &theme.tokens.elevation.e4);
            let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(true));
            let shape = RoundedRect::from_rect(ghost, radius);
            scene.fill(Fill::NonZero, transform, tier.fill, None, &shape);
            if let Some(border) = tier.border {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    border,
                    None,
                    &shape,
                );
            }
            if let Some(glow) = tier.inner_glow {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    glow,
                    None,
                    &RoundedRect::from_rect(ghost.inset(1.0), (radius - 1.0).max(0.0)),
                );
            }
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &RoundedRect::from_rect(
                    Rect::new(
                        ghost.x0,
                        ghost.y0 + self.scaled(RELATED_GAP),
                        ghost.x0 + lift,
                        ghost.y1 - self.scaled(RELATED_GAP),
                    ),
                    lift / 2.0,
                ),
            );
            if let Some(control) = self.edit_drag.dragged_id().copied() {
                self.draw_edit_row_content(scene, transform, text, theme, cursor, ghost, control);
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
                let inset = self.scaled(RELATED_GAP);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(
                            self.panel.x0 + pad + inset,
                            y - rail_half,
                            self.panel.x1 - pad - inset,
                            y + rail_half,
                        ),
                        rail_half,
                    ),
                );
            }
        }
        let chips = self.available_chips(state);
        if !chips.is_empty() {
            let top = self.available_edit_top(state);
            text.draw_text(
                scene,
                "AVAILABLE",
                caption_size,
                colors.text_dim,
                transform
                    .pre_translate((self.panel.x0 + pad, top + self.scaled(CONTROL_PAD)).into()),
                None,
            );
            for (i, (control, chip)) in chips.into_iter().enumerate() {
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(chip, Self::label(control))
                        .icon(ui::icons::Icon::Plus)
                        .text_size(caption_size)
                        .hovered(cursor.is_some_and(|point| chip.contains(point)))
                        .focused(self.keyboard_focus == Some(active.len().saturating_add(i)))
                        .accessible_name(Self::label(control)),
                );
            }
        }
        let reset = self.edit_reset(state);
        let done = self.edit_done(state);
        let edit_count = active
            .len()
            .saturating_add(self.available_edit_controls(state).len());
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
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(done, "Done")
                .primary()
                .hovered(cursor.is_some_and(|p| done.contains(p)))
                .focused(self.keyboard_focus == Some(edit_count.saturating_add(1)))
                .accessible_name("Finish editing Quick Settings"),
        );
    }
    /// The grip, label and add/remove button of an edit row, shared by the
    /// resting rows and the drag ghost so the two cannot drift apart. Every
    /// element sits on the row's centre line (HIG §7).
    #[allow(
        clippy::too_many_arguments,
        reason = "the row painter receives the scene, transform, text, theme, pointer, row rect and control together to paint one coherent row"
    )]
    fn draw_edit_row_content(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
        r: Rect,
        c: QuickSettingsControl,
    ) {
        let colors = &theme.tokens.colors;
        let inset = self.scaled(CONTROL_INSET);
        let glyph = Self::glyph_at(self.layout_scale);
        let present = self.config.controls.contains(&c);
        // A six-dot grip is a familiar, intentional reorder affordance;
        // the old horizontal ellipsis read as accidental punctuation. It is
        // centred in the row's leading glyph slot.
        let slot = leading_glyph(r, inset, glyph);
        let dot_pitch = glyph * 0.3;
        for x in [
            slot.center().x - dot_pitch / 2.0,
            slot.center().x + dot_pitch / 2.0,
        ] {
            for y in [
                slot.center().y - dot_pitch,
                slot.center().y,
                slot.center().y + dot_pitch,
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
        let action = trailing_toggle(r, inset, self.scaled(TARGET), self.scaled(TARGET));
        let label_size = type_size(text, theme.tokens.typography.scale.sm);
        let label_x = slot.x1 + inset;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let label = text.elide_to_width(
            Self::label(c),
            label_size,
            (action.x0 - self.scaled(RELATED_GAP) - label_x).max(1.0) as f32,
        );
        let (_, label_h) = text.measure_weighted(&label, label_size, None, TextWeight::Medium);
        text.draw_text_weighted(
            scene,
            &label,
            label_size,
            colors.text_primary,
            transform.pre_translate((label_x, centred_text_top(r, f64::from(label_h))).into()),
            None,
            TextWeight::Medium,
        );
        let action_face = ButtonFace::new(action, "")
            .hovered(cursor.is_some_and(|point| action.contains(point)))
            .accessible_name(if c.required() {
                "Required Quick Settings control"
            } else if present {
                "Remove Quick Settings control"
            } else {
                "Add Quick Settings control"
            })
            .text_size(type_size(text, theme.tokens.typography.scale.xs));
        let action_face = if c.required() {
            action_face.ghost()
        } else {
            action_face.icon(ui::icons::Icon::X)
        };
        Button::paint(scene, transform, text, theme, &action_face);
    }
    pub fn accessibility_snapshot(&self, state: &QuickSettingsState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        // Every node carries the rect its paint and hit-test use, so a
        // screen reader, the control socket and the UX bench address the
        // same geometry. Before the first frame there is no panel yet and
        // a zero rect would be a lie, so bounds are attached only once
        // the popover has been laid out.
        let laid_out = self.panel.area() > 0.0;
        let bounded = |node: AccessNode, rect: Rect| {
            if laid_out {
                node.with_bounds(rect)
            } else {
                node
            }
        };
        let mut root = bounded(
            AccessNode::new(Role::Dialog)
                .with_name("Quick Settings")
                .with_description(if self.edit_mode {
                    "Edit controls; drag or use Control plus Arrow keys to reorder"
                } else {
                    "Frequently used system controls"
                })
                .with_state(AccessState::new().modal(true)),
            self.panel,
        );
        if !self.edit_mode {
            root.children.push(bounded(
                AccessNode::new(Role::Button)
                    .with_name("Edit controls")
                    .with_state(AccessState::new().focusable(true)),
                self.edit_rect(),
            ));
        }
        if !self.edit_mode {
            for (index, name) in ["Settings", "Privacy", "History", "Power"]
                .into_iter()
                .enumerate()
            {
                root.children.push(bounded(
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_state(AccessState::new().focusable(true)),
                    self.action(index),
                ));
            }
            for (index, (name, value)) in
                [("Brightness", state.brightness), ("Volume", state.volume)]
                    .into_iter()
                    .enumerate()
            {
                root.children.push(bounded(
                    AccessNode::new(Role::Slider)
                        .with_name(name)
                        .with_value(format!("{:.0}%", value * 100.0))
                        .with_description("Use Left and Right Arrow to adjust")
                        .with_state(AccessState::new().focusable(true)),
                    self.slider_face_rect(self.panel, index),
                ));
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
        let count = controls.len();
        for (index, c) in controls.into_iter().enumerate() {
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
            let rect = if self.edit_mode {
                let active_count = self.active_edit_controls(state).len();
                if index < active_count {
                    self.card(self.panel, index, true)
                } else {
                    self.available_edit_chip(state, index - active_count)
                }
            } else {
                self.control_card(self.panel, index, count)
            };
            root.children.push(bounded(node, rect));
        }
        if !self.edit_mode {
            // The power-profile row is a real control with keyboard focus
            // (`activate_focused`), so it must be in the tree too.
            root.children.push(bounded(
                AccessNode::new(Role::Button)
                    .with_name("Change power profile")
                    .with_value(state.power_profile.label().to_string())
                    .with_state(AccessState::new().focusable(true)),
                self.power_geometry(self.panel, state),
            ));
        }
        if self.edit_mode {
            root.children.push(bounded(
                AccessNode::new(Role::Button)
                    .with_name("Reset defaults")
                    .with_state(AccessState::new().focusable(true)),
                self.edit_reset(state),
            ));
            root.children.push(bounded(
                AccessNode::new(Role::Button)
                    .with_name("Done")
                    .with_state(AccessState::new().focusable(true)),
                self.edit_done(state),
            ));
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
        let (_, chip) = root
            .available_chips(&state)
            .into_iter()
            .find(|(c, _)| *c == QuickSettingsControl::AirplaneMode)
            .expect("Airplane mode is offered while a radio exists");
        assert_eq!(
            root.edit_click(chip.center(), &state),
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
        assert_eq!(full.x0, panel.x0 + PANEL_PAD);
        assert_eq!(full.x1, panel.x1 - PANEL_PAD);
        assert_eq!(full.width(), 368.);
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
        // Four labeled destinations retain the shared sibling gap at the
        // 400px panel width: comfortably above the 44px minimum and wide
        // enough that the label does not collide with its neighbour.
        assert_eq!(
            actions[0].width(),
            (400.0 - 2.0 * PANEL_PAD - 3.0 * SIBLING_GAP) / 4.0
        );
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
        root.fit_root_layout_to_height(&state, 416.0);
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
        // 16px lower margin intact: the host fits the root's chrome to that
        // budget (`fit_root_layout_to_height`) exactly as it does per frame.
        // This catches accidental growth of whitespace that would hide the
        // power profile at common laptop sizes — the fit must succeed
        // while preserving full-size text and minimum pointer targets.
        let available = 600.0 - 48.0 - 16.0;
        root.fit_root_layout_to_height(&state, available);
        root.panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
        for index in 0..2 {
            assert!(root.slider_face_rect(root.panel, index).height() >= TARGET);
        }
        assert!(root.edit_rect().height() >= TARGET);
        for index in 0..QuickSettingsRoot::ACTION_COUNT {
            assert!(root.action(index).height() >= 44.0);
        }
        assert!(root.preferred_height(&state) <= available + f64::EPSILON);
        let panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
        assert!(root.power_geometry(panel, &state).y1 <= panel.y1 + f64::EPSILON);
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
    fn large_text_rows_grow_to_hold_their_type_and_the_title_stays_clear_of_the_tiles() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.set_layout_text_scale(2.0);
        let panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
        root.panel = panel;
        let label = root.label_line_height();
        let caption = root.caption_line_height();
        // Rects are built as `y + height`, so compare to within an ulp.
        let eps = 1e-6;
        let count = root.visible_controls(&state).len();
        let pad = root.scaled(CONTROL_PAD);
        for i in 0..count {
            let card = root.control_card(panel, i, count);
            let icon_row = root.card_icon_row(card);
            assert!(
                card.height() + eps
                    >= (icon_row.y1 - card.y0) + root.scaled(RELATED_GAP) + label + caption + pad,
                "card {i} must hold icon row, label and caption: {card:?}"
            );
        }
        for i in 0..4 {
            let tile = root.action(i);
            assert!(
                tile.height() + eps
                    >= 2.0 * pad + root.scaled(GLYPH + QuickSettingsRoot::GLYPH_GAP) + caption,
                "tile {i} must hold glyph, gap and caption: {tile:?}"
            );
        }
        let header = root.header_rect();
        assert!(
            header.height() >= root.line_height(ui::tokens::TypeScale::default().base),
            "the title band holds a base-scale title line"
        );
        assert!(
            header.y1 <= root.action(0).y0 + f64::EPSILON,
            "the title band ends before the action tiles begin"
        );
        let power = root.power_geometry(panel, &state);
        assert!(power.height() + eps >= 2.0 * pad + label + caption);
        assert!(
            power.y1 <= panel.y1 + f64::EPSILON,
            "the last control ends inside the panel"
        );
        // Rows still never overlap once they have grown.
        let mut rows: Vec<Rect> = (0..count)
            .map(|i| root.control_card(panel, i, count))
            .collect();
        rows.extend((0..4).map(|i| root.action(i)));
        rows.push(power);
        for (i, a) in rows.iter().enumerate() {
            for b in &rows[..i] {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.width() <= 0.0 || overlap.height() <= 0.0,
                    "{a:?} overlaps {b:?}"
                );
            }
        }
    }

    /// One rhythm at 1× (HIG §7b, §9.6): every adjacent pair in the root
    /// keeps its token step — sections `s4`, grid siblings `s3`, the two
    /// slider bands and a card's icon row `s2` — nothing touches, glyphs and
    /// switches share their row's centre line, and the power row rests at
    /// the Control height because its two lines fit inside it.
    #[test]
    fn root_geometry_keeps_one_rhythm_at_one_x() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        let panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
        root.panel = panel;
        let s2 = f64::from(ui::tokens::Spacing::BASE.s2);
        let s3 = f64::from(ui::tokens::Spacing::BASE.s3);
        let s4 = f64::from(ui::tokens::Spacing::BASE.s4);
        let eps = 1e-6;
        let close = |a: f64, b: f64| (a - b).abs() < eps;
        let tiles: Vec<Rect> = (0..4).map(|i| root.action(i)).collect();
        assert!(
            close(tiles[0].y0 - root.header_rect().y1, s4),
            "title band → tiles is s4"
        );
        assert!(
            root.edit_rect().y1 + s2 <= tiles[0].y0 + eps,
            "the Edit button clears the tiles by at least s2"
        );
        for pair in tiles.windows(2) {
            assert!(
                close(pair[1].x0 - pair[0].x1, s3),
                "tile ↔ tile is s3: {pair:?}"
            );
        }
        let sliders = root.sliders_rect(panel);
        assert!(close(sliders.y0 - tiles[0].y1, s4), "tiles → sliders is s4");
        let (b, v) = (
            root.slider_face_rect(panel, 0),
            root.slider_face_rect(panel, 1),
        );
        assert!(
            close(b.y0 - sliders.y0, s2) && close(sliders.y1 - v.y1, s2),
            "the bands sit on s2 of padding"
        );
        assert!(
            close(v.y0 - b.y1, s2),
            "the two slider bands are s2 apart, never touching"
        );
        assert!(
            close(sliders.x1 - b.x1, s3),
            "a band ends on the block's s3 inset"
        );
        let count = root.visible_controls(&state).len();
        let cards: Vec<Rect> = (0..count)
            .map(|i| root.control_card(panel, i, count))
            .collect();
        assert!(close(cards[0].y0 - sliders.y1, s4), "sliders → cards is s4");
        assert!(close(cards[1].x0 - cards[0].x1, s3), "card ↔ card is s3");
        assert!(
            close(cards[2].y0 - cards[0].y1, s3),
            "card row ↔ card row is s3"
        );
        let power = root.power_geometry(panel, &state);
        assert!(close(power.y0 - cards[2].y1, s4), "cards → power is s4");
        assert!(
            close(panel.y1 - power.y1, s4),
            "power → panel edge is the panel padding"
        );
        assert!(
            close(
                power.height(),
                f64::from(ui::tokens::Metrics::BASE.panel_row_height)
            ),
            "the power row rests at the Control height at 1×"
        );
        // Inside a card: the icon row's glyph and switch share the row's
        // centre line, sit on the s3 inset and never meet.
        let icon_row = root.card_icon_row(cards[0]);
        let glyph = leading_glyph(icon_row, s3, GLYPH);
        let toggle = trailing_toggle(icon_row, s3, TOGGLE_W, TOGGLE_H);
        assert!(
            close(glyph.center().y, icon_row.center().y)
                && close(toggle.center().y, icon_row.center().y)
        );
        assert!(close(glyph.x0 - cards[0].x0, s3) && close(cards[0].x1 - toggle.x1, s3));
        assert!(glyph.x1 + s3 < toggle.x0, "glyph and switch never meet");
        assert!(
            close(icon_row.y0 - cards[0].y0, s2),
            "the icon row sits on s2 of padding"
        );
        // Nothing overlaps.
        let mut all = tiles.clone();
        all.push(sliders);
        all.extend(cards.iter().copied());
        all.push(power);
        for (i, a) in all.iter().enumerate() {
            for b in &all[..i] {
                let o = a.intersect(*b);
                assert!(
                    o.width() <= 0.0 || o.height() <= 0.0,
                    "{a:?} overlaps {b:?}"
                );
            }
        }
    }

    #[test]
    fn accessibility_nodes_carry_the_rects_paint_and_hit_testing_use() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::default();
        state.radios.wifi_available = true;
        root.panel = PANEL;
        let snapshot = root.accessibility_snapshot(&state);
        assert_eq!(snapshot.bounds.map(ui::AccessBounds::to_rect), Some(PANEL));
        for node in &snapshot.children {
            let rect = node
                .bounds
                .map(ui::AccessBounds::to_rect)
                .unwrap_or_else(|| panic!("{:?} has no bounds", node.name));
            assert!(
                rect.area() > 0.0 && PANEL.contains(rect.center()),
                "{:?} lies inside the panel: {rect:?}",
                node.name
            );
        }
        let by_name = |name: &str| {
            snapshot
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some(name))
                .and_then(|node| node.bounds.map(ui::AccessBounds::to_rect))
                .unwrap_or_else(|| panic!("{name} is in the tree with bounds"))
        };
        assert_eq!(by_name("Edit controls"), root.edit_rect());
        assert_eq!(by_name("Settings"), root.action(0));
        assert_eq!(by_name("Power"), root.action(3));
        assert_eq!(by_name("Brightness"), root.slider_face_rect(PANEL, 0));
        assert_eq!(
            by_name("Wi-Fi"),
            root.control_card(PANEL, 0, root.visible_controls(&state).len())
        );
        assert_eq!(
            by_name("Change power profile"),
            root.power_geometry(PANEL, &state)
        );
        // Before the first frame there is no geometry, and none is claimed.
        let unlaid = QuickSettingsRoot::default().accessibility_snapshot(&state);
        assert!(unlaid.children.iter().all(|node| node.bounds.is_none()));
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
