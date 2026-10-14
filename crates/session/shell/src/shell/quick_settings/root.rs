//! Compact, configurable root page for the Quick Settings popover.
//!
//! The panel presents current controls, not a dashboard of every capability.
//! Arrangement is durable user intent while hardware availability is applied
//! only when the panel is rendered.

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::i18n::{t, t_args};
use crate::shell::quick_settings::{PowerProfile, QsLevel};
use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use tilo_desktop::drill_in::{
    CONTROL_INSET, CONTROL_PAD, GLYPH, PANEL_PAD, RELATED_GAP, ROW_HEIGHT, SECTION_GAP,
    SIBLING_GAP, TARGET, TOGGLE_H, TOGGLE_W, centred_text_top, control_radius, leading_glyph,
    paint_control, trailing_glyph, trailing_toggle,
};
use ui::TextRole;
use ui::num::f64_of_usize;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::ButtonRole;
use ui::widget::drag::{DragController, ListMetrics};
use ui::widget::{Button, ButtonFace, Slider, SliderFace, Toggle, ToggleFace};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;

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
    /// The active workspace's app grants and enforced access.
    OpenPrivacy,
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
    pub do_not_disturb: Option<bool>,
    pub rotation_lock: Option<bool>,
    pub power: Option<PowerProfile>,
}

/// A labelled destination tile under the title band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootTile {
    Settings,
    Privacy,
    History,
    Power,
}

impl RootTile {
    /// Paint order, left to right.
    const ALL: [Self; 4] = [Self::Settings, Self::Privacy, Self::History, Self::Power];

    fn label(self) -> String {
        t(match self {
            Self::Settings => "qs-tile-settings",
            Self::Privacy => "qs-tile-privacy",
            Self::History => "qs-tile-history",
            Self::Power => "qs-tile-power",
        })
    }
    const fn semantic_name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Privacy => "privacy",
            Self::History => "history",
            Self::Power => "power",
        }
    }
    fn description(self) -> String {
        t(match self {
            Self::Settings => "qs-tile-settings-description",
            Self::Privacy => "qs-tile-privacy-description",
            Self::History => "qs-tile-history-description",
            Self::Power => "qs-tile-power-description",
        })
    }
    const fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Settings => ui::icons::Icon::Settings2,
            Self::Privacy => ui::icons::Icon::Shield,
            Self::History => ui::icons::Icon::History,
            Self::Power => ui::icons::Icon::Power,
        }
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|tile| *tile == self).unwrap_or(0)
    }
    fn action(self) -> RootAction {
        match self {
            Self::Settings => RootAction::OpenSettings,
            Self::Privacy => RootAction::OpenPrivacy,
            Self::History => RootAction::Enter(QsLevel::Notifications),
            Self::Power => RootAction::OpenPowerMenu,
        }
    }
}

/// One keyboard focus stop on the root page. [`QuickSettingsRoot::root_stops`]
/// is the single ordered table that paint, keyboard activation, semantic
/// targets and the accessibility tree all index, so a focus ring can never
/// light one control while Enter activates another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootStop {
    Slider(usize),
    Control(usize, QuickSettingsControl),
    Details(usize, QuickSettingsControl),
    Devices,
    Clipboard,
    PowerProfile,
    Edit,
    Tile(RootTile),
}

/// A drill-in row's focus stop and the glyph, label and detail line it shows.
struct LinkRow {
    stop: RootStop,
    icon: ui::icons::Icon,
    label: String,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliderCommit {
    /// The one brightness control of a machine whose monitors are not
    /// enumerated individually.
    Brightness(f32),
    /// Monitor `.0` of [`QuickSettingsState::displays`], in percent.
    DisplayBrightness(usize, f32),
    Volume(f32),
}

/// Root interaction state.  Editing uses the shared Tilo reorder controller;
/// it never serialises paint geometry, only stable control identifiers.
pub struct QuickSettingsRoot {
    /// The page's layout rect: the popover itself when everything fits,
    /// otherwise the full-height page placed under the popover at the
    /// current scroll offset. Every geometry helper measures from it, so
    /// paint, hit-testing and accessibility agree while scrolled.
    panel: Rect,
    /// The popover rect the host gave this page, and the region that
    /// scrolls the page inside it when the window is too short for the
    /// controls at their natural size.
    viewport: Rect,
    scroll: ui::widget::ScrollRegion,
    /// The popover rect `scroll` was last sized for; a page laid out in any
    /// other rect does not scroll until a frame sizes it.
    scroll_viewport: Rect,
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
    // One brightness slider per controllable monitor, named from its EDID;
    // synced from the state before geometry is resolved. Empty (or one
    // monitor) keeps the single unlabelled brightness slider.
    display_names: Vec<String>,
    keyboard_focus: Option<usize>,
    // The root stop keyboard focus is on. `root_stops` changes while the
    // popover is open (a slider or control appears once its device state
    // arrives), so an index alone would move focus, or Enter, to another
    // stop. Keyboard navigation sets it; the index follows it.
    root_focus: Option<RootStop>,
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
    const EDIT_ACTION_WIDTH: f64 = 76.0; // Text affordance width, not a spacing step.
    fn edit_help_copy() -> String {
        t("qs-edit-help")
    }

    /// Single-line height of a chrome type role at the user's text scale.
    ///
    /// Geometry is resolved before a frame has a renderer — the host sizes
    /// and hit-tests the popover from these helpers — so rows take their
    /// minimum height from the toolkit's own line metrics for the default
    /// type scale rather than from a literal. The draw path then centres
    /// the text it actually measured inside that row, so a theme with a
    /// different type scale still paints correctly.
    fn line_height(&self, role: TextRole) -> f64 {
        f64::from(
            ui::tokens::Typography::default()
                .style(role)
                .line_height_px(),
        ) * self.text_scale
    }
    /// A control's label line (`Caption` in this dense panel).
    fn label_line_height(&self) -> f64 {
        self.line_height(TextRole::Caption)
    }
    /// A control's sub-line or a section caption (`Caption`).
    fn caption_line_height(&self) -> f64 {
        self.line_height(TextRole::Caption)
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
    /// The power row: its label and sub-line with at least `s1` above and
    /// below, never under the Control height.
    fn power_height_at(&self, layout_scale: f64) -> f64 {
        let inner = f64::from(ui::tokens::Spacing::BASE.s1);
        (ROW_HEIGHT * layout_scale)
            .max(2.0 * inner * layout_scale + self.label_line_height() + self.caption_line_height())
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
            viewport: Rect::ZERO,
            scroll: ui::widget::ScrollRegion::vertical(),
            scroll_viewport: Rect::ZERO,
            layout_scale: 1.0,
            text_scale: 1.0,
            slider_drag: None,
            display_names: Vec::new(),
            keyboard_focus: None,
            root_focus: None,
            config: config.normalised(),
            persist_config,
            edit_mode: false,
            edit_drag: DragController::vertical(),
        }
    }
    pub fn reset_focus(&mut self) {
        // Opening the popover does not itself move keyboard focus. The first
        // Tab selects the first stop in `root_stops` (Brightness) instead of
        // silently preselecting it and making that same Tab skip past it.
        self.keyboard_focus = None;
        self.root_focus = None;
        // Each opening starts at the top of the page.
        self.scroll.set_offset(vello::kurbo::Vec2::ZERO);
    }

    /// Lay the page out in the popover rect `viewport`. When the controls at
    /// their natural size are taller than it, the page keeps that size and
    /// scrolls ([`ui::widget::ScrollRegion`]) instead of squeezing its tiles;
    /// the returned page rect sits at the scroll offset, laid out to the
    /// region's content width. Until a frame has sized the region (it needs
    /// the theme's bar lane) the page is the popover rect itself.
    fn lay_out(&mut self, viewport: Rect, state: &QuickSettingsState) -> Rect {
        self.viewport = viewport;
        self.panel = self.page_rect(state);
        self.panel
    }

    fn page_rect(&self, state: &QuickSettingsState) -> Rect {
        if !self.scrolls() {
            return self.viewport;
        }
        let shown = self.scroll.content_rect();
        let top = shown.y0 - self.scroll.offset().y;
        let height = self.preferred_height(state).max(shown.height());
        Rect::new(shown.x0, top, shown.x1, top + height)
    }

    /// Size the scroll region for this frame's popover and theme.
    fn sync_scroll(&mut self, viewport: Rect, theme: &Theme, state: &QuickSettingsState) {
        self.scroll.set_viewport(viewport, theme);
        self.scroll
            .set_content_size(viewport.width(), self.preferred_height(state));
        self.scroll_viewport = viewport;
    }

    /// The part of the popover the page shows in (less the bar lane while
    /// it scrolls).
    fn visible_rect(&self) -> Rect {
        if self.scrolls() {
            self.scroll.content_rect()
        } else {
            self.panel
        }
    }

    /// Whether the page is taller than the popover and scrolls.
    pub fn scrolls(&self) -> bool {
        !self.viewport.is_zero_area()
            && self.scroll_viewport == self.viewport
            && self.scroll.overflows_vertically()
    }

    /// Wheel, touchpad, touch-drag (as the compositor's emulated wheel) and
    /// scrollbar input over a page that scrolls. Returns whether the page
    /// moved or its bar changed; `handled` is set when the event was the
    /// region's.
    pub fn handle_scroll_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::keyboard::ModifiersState,
        handled: &mut bool,
        theme: &Theme,
    ) -> bool {
        if !self.scrolls() && !self.scroll.is_dragging() {
            return false;
        }
        self.scroll
            .handle_window_event(event, scale_factor, modifiers, handled, theme)
    }

    /// Whether the page's scrollbar is being dragged.
    pub fn scroll_dragging(&self) -> bool {
        self.scroll.is_dragging()
    }

    /// The scrollbar's fade, for the host's frame clock.
    pub fn scroll_needs_frame(&self, theme: &Theme) -> bool {
        self.scrolls() && self.scroll.needs_animation_frame(theme)
    }
    pub fn scroll_wake_deadline(&self, theme: &Theme) -> Option<std::time::Instant> {
        if self.scrolls() {
            self.scroll.next_wake_deadline(theme)
        } else {
            None
        }
    }

    /// How far the page is scrolled, in logical pixels.
    #[cfg(test)]
    pub fn scroll_offset(&self) -> f64 {
        self.scroll.offset().y
    }

    /// Bring the keyboard-focused stop into view, as a scrolling list does.
    fn reveal_focus(&mut self, state: &QuickSettingsState) {
        if !self.scrolls() || self.edit_mode {
            return;
        }
        let Some(stop) = self.focused_stop(state) else {
            return;
        };
        let rect = self.stop_rect(stop, state);
        let page = self.panel;
        self.scroll.scroll_into_view(Rect::new(
            rect.x0 - page.x0,
            rect.y0 - page.y0,
            rect.x1 - page.x0,
            rect.y1 - page.y0,
        ));
        self.panel = self.page_rect(state);
    }

    /// Named geometry for every visible root affordance. Quick Settings is
    /// retained chrome rather than a child-widget tree, so this projection is
    /// the semantic equivalent of `Widget::test_rects()`.
    pub fn semantic_rects(&self, state: &QuickSettingsState) -> Vec<(String, Rect)> {
        // Only what is on screen is a target: a control scrolled out of the
        // popover is reached by scrolling, never by a point outside it.
        if !self.scrolls() {
            return self.page_semantic_rects(state);
        }
        let visible = self.visible_rect();
        self.page_semantic_rects(state)
            .into_iter()
            .filter_map(|(name, rect)| {
                let shown = rect.intersect(visible);
                (shown.width() > 0.0 && shown.height() > 0.0).then_some((name, shown))
            })
            .collect()
    }

    fn page_semantic_rects(&self, state: &QuickSettingsState) -> Vec<(String, Rect)> {
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
        for stop in self.root_stops(state) {
            let rect = self.stop_hit_rect(stop, state);
            let name = self.semantic_stop_name(stop);
            if let RootStop::Control(_, control) = stop
                && Self::has_details(control, state)
            {
                let details = self.control_details_rect(rect);
                rects.push((
                    name.clone(),
                    Rect::new(rect.x0, rect.y0, details.x0, rect.y1),
                ));
                if control == QuickSettingsControl::Vpn {
                    rects.push((format!("{name}_details"), details));
                }
            } else {
                rects.push((name, rect));
            }
        }
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
            QuickSettingsControl::DoNotDisturb => "do_not_disturb",
            QuickSettingsControl::RotationLock => "rotation_lock",
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
    /// Take the monitor list from `state`: one brightness slider each.
    pub fn sync_displays(&mut self, state: &QuickSettingsState) {
        if self.display_names.len() != state.displays.len()
            || self
                .display_names
                .iter()
                .zip(&state.displays)
                .any(|(name, display)| *name != display.name)
        {
            self.display_names = state.displays.iter().map(|d| d.name.clone()).collect();
            self.slider_drag = None;
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
            .max(self.line_height(TextRole::Body))
    }
    fn sliders_height_at(&self, scale: f64) -> f64 {
        let count = self.slider_count();
        let captions: f64 = (0..count).map(|i| self.slider_caption_height(i)).sum();
        (2.0 * CONTROL_PAD + f64_of_usize(count.saturating_sub(1)) * RELATED_GAP) * scale
            + f64_of_usize(count) * Self::target_at(scale)
            + captions
    }
    /// Brightness sliders: one per monitor, at least one.
    fn brightness_rows(&self) -> usize {
        self.display_names.len().max(1)
    }
    /// Every brightness slider, then volume.
    fn slider_count(&self) -> usize {
        self.brightness_rows().saturating_add(1)
    }
    fn volume_slider(&self) -> usize {
        self.brightness_rows()
    }
    /// With several monitors, each brightness slider carries its monitor's
    /// name on a caption line above the face.
    fn slider_caption_height(&self, i: usize) -> f64 {
        if self.display_names.len() > 1 && i < self.brightness_rows() {
            self.caption_line_height()
        } else {
            0.0
        }
    }
    fn slider_name(&self, i: usize) -> String {
        if i == self.volume_slider() {
            t("qs-volume")
        } else {
            match self.display_names.get(i) {
                Some(name) if self.display_names.len() > 1 => {
                    t_args("qs-brightness-display", &[("display", name.into())])
                }
                _ => t("qs-brightness"),
            }
        }
    }
    fn slider_available(&self, state: &QuickSettingsState, i: usize) -> bool {
        if i == self.volume_slider() {
            state.volume_available
        } else {
            !state.displays.is_empty() || state.brightness_available
        }
    }
    fn slider_value(&self, state: &QuickSettingsState, i: usize) -> f32 {
        if i == self.volume_slider() {
            state.volume
        } else {
            state
                .displays
                .get(i)
                .map_or(state.brightness, |display| display.level)
        }
    }
    fn set_slider_value(&self, state: &mut QuickSettingsState, i: usize, value: f32) {
        if i == self.volume_slider() {
            state.volume = value;
            return;
        }
        if let Some(display) = state.displays.get_mut(i) {
            display.level = value;
        }
        if i == 0 {
            state.brightness = value;
        }
    }
    fn slider_commit(&self, state: &QuickSettingsState, i: usize) -> SliderCommit {
        let pct = self.slider_value(state, i) * 100.;
        if i == self.volume_slider() {
            SliderCommit::Volume(pct)
        } else if state.displays.is_empty() {
            SliderCommit::Brightness(pct)
        } else {
            SliderCommit::DisplayBrightness(i, pct)
        }
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
                QuickSettingsControl::NightLight | QuickSettingsControl::DoNotDisturb => true,
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
                // Only a convertible that can sense its orientation rotates.
                QuickSettingsControl::RotationLock => state.rotation_available,
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
            QuickSettingsControl::DoNotDisturb,
            QuickSettingsControl::RotationLock,
        ] {
            if !all.contains(&c) {
                all.push(c);
            }
        }
        all.into_iter()
            .filter(|c| match c {
                QuickSettingsControl::Wifi
                | QuickSettingsControl::NightLight
                | QuickSettingsControl::DoNotDisturb => true,
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
                // Only a convertible that can sense its orientation rotates.
                QuickSettingsControl::RotationLock => state.rotation_available,
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
            let rows = f64_of_usize(self.active_edit_controls(state).len());
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
            let rows = f64_of_usize(self.visible_controls(state).len().div_ceil(2));
            // Title band → tiles → sliders → cards → rows: one section gap
            // before each section that exists on this machine. The Devices
            // and Clipboard row and the power row sit one sibling gap apart.
            let sections = if rows > 0.0 { 4.0 } else { 3.0 };
            scaled(PANEL_PAD * 2.0 + SECTION_GAP * sections + SIBLING_GAP)
                + self.header_height_at(layout_scale)
                + self.sliders_height_at(layout_scale)
                + self.action_height_at(layout_scale)
                + rows * self.card_height_at(layout_scale)
                + (rows - 1.0).max(0.0) * scaled(SIBLING_GAP)
                + 2.0 * self.power_height_at(layout_scale)
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
            top + self.sliders_height_at(self.layout_scale),
        )
    }
    /// Top of the control cards: one section gap under the slider block.
    fn grid_top(&self, panel: Rect) -> f64 {
        self.sliders_top(panel)
            + self.sliders_height_at(self.layout_scale)
            + self.scaled(SECTION_GAP)
    }
    fn edit_grid_top(&self, panel: Rect) -> f64 {
        self.header_bottom(panel) + 2.0 * self.caption_band()
    }
    #[cfg(test)]
    fn header_baseline_offset(&self) -> f64 {
        self.scaled(PANEL_PAD) + self.header_height_at(self.layout_scale) / 2.0
            - self.line_height(TextRole::Body) / 2.0
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
        let active = f64_of_usize(self.active_edit_controls(state).len());
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
            - gap * f64_of_usize(available.len().saturating_sub(1)))
            / f64_of_usize(available.len());
        available
            .into_iter()
            .enumerate()
            .map(|(i, control)| {
                let x = x0 + f64_of_usize(i) * (width + gap);
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
            let y =
                self.edit_grid_top(panel) + f64_of_usize(i) * (row_h + self.scaled(RELATED_GAP));
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
        let x = panel.x0 + pad + f64_of_usize(col) * (w + gap);
        let y = self.grid_top(panel) + f64_of_usize(row) * (card + gap);
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
        let y = self.grid_top(panel) + f64_of_usize(row) * (card + gap);
        if count % 2 == 1 && i.checked_add(1) == Some(count) {
            return Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + card);
        }
        let w = (panel.width() - 2.0 * pad - gap) / 2.0;
        let x = panel.x0 + pad + f64_of_usize(i % 2) * (w + gap);
        Rect::new(x, y, x + w, y + card)
    }
    fn has_details(control: QuickSettingsControl, state: &QuickSettingsState) -> bool {
        matches!(
            control,
            QuickSettingsControl::Bluetooth | QuickSettingsControl::Vpn
        ) || (control == QuickSettingsControl::Wifi && state.radios.wifi_available)
    }
    fn details_level(control: QuickSettingsControl) -> QsLevel {
        match control {
            QuickSettingsControl::Wifi => QsLevel::Wifi,
            QuickSettingsControl::Bluetooth => QsLevel::Bluetooth,
            _ => QsLevel::Vpn,
        }
    }
    fn details_label(control: QuickSettingsControl) -> String {
        t(match control {
            QuickSettingsControl::Wifi => "qs-network-details",
            QuickSettingsControl::Bluetooth => "qs-bluetooth-details",
            _ => "qs-vpn",
        })
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
        // Accessibility can ask for a target before layout, or while a
        // narrow output is changing. Never clamp against an inverted range.
        if card.width() <= 0.0 || card.height() <= 0.0 {
            return Rect::new(card.x0, card.y0, card.x0, card.y0);
        }
        let target = Self::target_at(self.layout_scale);
        let width = target.min(card.width());
        let height = target.min(card.height());
        let center = Point::new(
            chevron
                .center()
                .x
                .clamp(card.x0 + width / 2.0, card.x1 - width / 2.0),
            chevron
                .center()
                .y
                .clamp(card.y0 + height / 2.0, card.y1 - height / 2.0),
        );
        Rect::from_center_size(center, (width, height))
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
        let above: f64 = (0..i).map(|j| self.slider_caption_height(j)).sum();
        let y = block.y0
            + self.scaled(CONTROL_PAD)
            + f64_of_usize(i) * (Self::target_at(self.layout_scale) + self.scaled(RELATED_GAP))
            + above
            + self.slider_caption_height(i);
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
    /// Bottom edge of the control cards — or of the sliders when this
    /// machine has no controls to show.
    fn cards_bottom(&self, panel: Rect, state: &QuickSettingsState) -> f64 {
        let rows = self.visible_controls(state).len().div_ceil(2);
        let grid_top = self.grid_top(panel);
        if rows == 0 {
            grid_top - self.scaled(SECTION_GAP)
        } else {
            grid_top
                + f64_of_usize(rows) * self.card_height()
                + f64_of_usize(rows.saturating_sub(1)) * self.scaled(SIBLING_GAP)
        }
    }
    /// Devices and Clipboard share one row, one section gap under the
    /// cards. `index` 0 is Devices, 1 is Clipboard.
    fn link_rect(&self, panel: Rect, state: &QuickSettingsState, index: usize) -> Rect {
        let pad = self.scaled(PANEL_PAD);
        let gap = self.scaled(SIBLING_GAP);
        let y = self.cards_bottom(panel, state) + self.scaled(SECTION_GAP);
        let w = (panel.width() - 2.0 * pad - gap) / 2.0;
        let x = panel.x0 + pad + f64_of_usize(index.min(1)) * (w + gap);
        Rect::new(x, y, x + w, y + self.power_height())
    }
    /// The power-profile row: one sibling gap under the Devices and
    /// Clipboard row it shares its row style with.
    pub(crate) fn power_geometry(&self, panel: Rect, state: &QuickSettingsState) -> Rect {
        let y = self.link_rect(panel, state, 0).y1 + self.scaled(SIBLING_GAP);
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
        let gaps = f64_of_usize(Self::ACTION_COUNT.saturating_sub(1));
        let w = (self.panel.width() - 2.0 * pad - gaps * gap) / f64_of_usize(Self::ACTION_COUNT);
        let x = self.panel.x0 + pad + f64_of_usize(i) * (w + gap);
        let y = self.actions_top(self.panel);
        Rect::new(x, y, x + w, y + self.action_height())
    }
    /// Every root focus stop, in keyboard order: the sliders (one brightness
    /// slider per monitor, then volume), the live
    /// controls, Devices, Clipboard and the power profile, then Edit and the
    /// four destination tiles. Controls come first so the first Tab after
    /// opening reaches Brightness.
    fn root_stops(&self, state: &QuickSettingsState) -> Vec<RootStop> {
        let mut stops: Vec<RootStop> = (0..self.slider_count())
            .filter(|&index| self.slider_available(state, index))
            .map(RootStop::Slider)
            .collect();
        stops.extend(
            self.visible_controls(state)
                .into_iter()
                .enumerate()
                .flat_map(|(index, control)| {
                    let details = (Self::has_details(control, state)
                        && control != QuickSettingsControl::Vpn)
                        .then_some(RootStop::Details(index, control));
                    std::iter::once(RootStop::Control(index, control)).chain(details)
                }),
        );
        stops.extend([
            RootStop::Devices,
            RootStop::Clipboard,
            RootStop::PowerProfile,
            RootStop::Edit,
        ]);
        stops.extend(RootTile::ALL.map(RootStop::Tile));
        stops
    }
    fn focused_stop(&self, state: &QuickSettingsState) -> Option<RootStop> {
        if self.edit_mode {
            return None;
        }
        let stops = self.root_stops(state);
        self.root_focus_index(&stops)
            .and_then(|index| stops.get(index).copied())
    }
    /// Where keyboard focus is among `stops`: the focused stop's current
    /// position, or none if that stop is gone.
    fn root_focus_index(&self, stops: &[RootStop]) -> Option<usize> {
        match self.root_focus {
            Some(stop) => stops.iter().position(|s| *s == stop),
            None => self.keyboard_focus,
        }
    }
    fn is_focused(&self, state: &QuickSettingsState, stop: RootStop) -> bool {
        self.focused_stop(state) == Some(stop)
    }
    /// The rect a stop paints and publishes to accessibility.
    fn stop_rect(&self, stop: RootStop, state: &QuickSettingsState) -> Rect {
        match stop {
            RootStop::Slider(index) => self.slider_face_rect(self.panel, index),
            RootStop::Control(index, _) => {
                self.control_card(self.panel, index, self.visible_controls(state).len())
            }
            RootStop::Details(index, _) => self.control_details_rect(self.control_card(
                self.panel,
                index,
                self.visible_controls(state).len(),
            )),
            RootStop::Devices => self.link_rect(self.panel, state, 0),
            RootStop::Clipboard => self.link_rect(self.panel, state, 1),
            RootStop::PowerProfile => self.power_geometry(self.panel, state),
            RootStop::Edit => self.edit_rect(),
            RootStop::Tile(tile) => self.action(tile.index()),
        }
    }
    /// The pointer target of a stop. Edit is painted as a quiet square in
    /// the title band but owns the same wide target as a labelled tile.
    fn stop_hit_rect(&self, stop: RootStop, state: &QuickSettingsState) -> Rect {
        match stop {
            RootStop::Edit => {
                Rect::from_center_size(self.edit_rect().center(), (self.action(0).width(), 44.0))
            }
            _ => self.stop_rect(stop, state),
        }
    }
    fn semantic_stop_name(&self, stop: RootStop) -> String {
        match stop {
            RootStop::Slider(i) if i == self.volume_slider() => "quick_settings_volume".into(),
            RootStop::Slider(0) => "quick_settings_brightness".into(),
            RootStop::Slider(i) => format!("quick_settings_brightness_{i}"),
            RootStop::Control(_, control) => {
                format!("quick_settings_{}", Self::semantic_control_name(control))
            }
            RootStop::Details(_, control) => {
                format!(
                    "quick_settings_{}_details",
                    Self::semantic_control_name(control)
                )
            }
            RootStop::Devices => "quick_settings_devices".into(),
            RootStop::Clipboard => "quick_settings_clipboard".into(),
            RootStop::PowerProfile => "quick_settings_power_profile".into(),
            RootStop::Edit => "quick_settings_edit".into(),
            RootStop::Tile(tile) => format!("quick_settings_{}", tile.semantic_name()),
        }
    }
    /// What activating a stop does, shared by Enter/Space and a click on
    /// the stop's body.
    fn activate_stop(&mut self, stop: RootStop, state: &mut QuickSettingsState) -> RootAction {
        match stop {
            RootStop::Slider(_) => RootAction::None,
            RootStop::Control(_, control) => {
                let mut p = PendingRootActions::default();
                Self::toggle(control, state, &mut p).unwrap_or(RootAction::Changed(p))
            }
            RootStop::Details(_, control) => RootAction::Enter(Self::details_level(control)),
            RootStop::Devices => RootAction::Enter(QsLevel::Devices),
            RootStop::Clipboard => RootAction::Enter(QsLevel::Clipboard),
            RootStop::PowerProfile => Self::cycle_power(state),
            RootStop::Edit => {
                self.edit_mode = true;
                self.keyboard_focus = None;
                self.root_focus = None;
                RootAction::LayoutChanged
            }
            RootStop::Tile(tile) => tile.action(),
        }
    }
    fn devices_subline(state: &QuickSettingsState) -> String {
        match state.personal_devices {
            None => t("qs-devices-audio-and-media"),
            Some(count) => t_args("qs-devices-count", &[("count", count.into())]),
        }
    }
    fn clipboard_subline(state: &QuickSettingsState) -> String {
        match state.clipboard_items {
            0 => t("qs-clipboard-empty"),
            count => t_args("qs-clipboard-count", &[("count", count.into())]),
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
    fn label(c: QuickSettingsControl) -> String {
        t(match c {
            QuickSettingsControl::Wifi => "qs-network",
            QuickSettingsControl::Bluetooth => "qs-bluetooth",
            QuickSettingsControl::Vpn => "qs-vpn",
            QuickSettingsControl::NightLight => "qs-night-light",
            QuickSettingsControl::AirplaneMode => "qs-airplane-mode",
            QuickSettingsControl::MobileData => "qs-mobile-data",
            QuickSettingsControl::DoNotDisturb => "qs-do-not-disturb",
            QuickSettingsControl::RotationLock => "qs-rotation-lock",
        })
    }
    fn rendered_label(c: QuickSettingsControl, s: &QuickSettingsState) -> String {
        match c {
            QuickSettingsControl::Wifi if !s.radios.wifi_available => t("qs-wired-network"),
            QuickSettingsControl::Wifi => t("qs-wifi"),
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
            QuickSettingsControl::DoNotDisturb => ui::icons::Icon::Bell,
            QuickSettingsControl::RotationLock => ui::icons::Icon::RotateCw,
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
            QuickSettingsControl::DoNotDisturb => s.do_not_disturb,
            QuickSettingsControl::RotationLock => s.rotation_lock,
        }
    }
    fn subline(c: QuickSettingsControl, s: &QuickSettingsState) -> String {
        if s.radio_change_pending
            && matches!(
                c,
                QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
            )
        {
            return t("display-confirm-applying");
        }

        let on_off = |on: bool| t(if on { "qs-on" } else { "qs-off" });
        match c {
            QuickSettingsControl::Wifi if !s.radios.wifi_available => s
                .wired
                .as_ref()
                .and_then(|wired| wired.device.as_deref())
                .map_or_else(
                    || t("qs-not-connected"),
                    |device| {
                        if s.wired.as_ref().is_some_and(|wired| wired.connected) {
                            device.to_owned()
                        } else {
                            t("qs-cable-unplugged")
                        }
                    },
                ),
            QuickSettingsControl::Wifi if s.wifi => s
                .wifi_networks
                .iter()
                .find(|v| v.connected)
                .map_or_else(|| t("qs-not-connected"), |v| v.ssid.clone()),
            QuickSettingsControl::Bluetooth if s.bluetooth => s
                .bt_devices
                .iter()
                .find(|v| v.connected)
                .map_or_else(|| t("qs-not-connected"), |v| v.name.clone()),
            QuickSettingsControl::Wifi | QuickSettingsControl::Bluetooth => t("qs-off"),
            QuickSettingsControl::Vpn => s
                .vpns
                .iter()
                .find(|v| v.active)
                .or_else(|| s.vpns.first())
                .map_or_else(|| t("qs-vpn-no-profiles"), |v| v.name.clone()),
            QuickSettingsControl::NightLight => on_off(s.night_light),
            QuickSettingsControl::AirplaneMode => on_off(s.radios.airplane_mode),
            QuickSettingsControl::MobileData => on_off(s.radios.wwan_enabled),
            QuickSettingsControl::DoNotDisturb => t(if s.do_not_disturb {
                "qs-do-not-disturb-on"
            } else {
                "qs-off"
            }),
            QuickSettingsControl::RotationLock => t(if s.rotation_lock {
                "qs-rotation-locked"
            } else {
                "qs-rotation-unlocked"
            }),
        }
    }
    fn toggle(
        c: QuickSettingsControl,
        s: &mut QuickSettingsState,
        pending: &mut PendingRootActions,
    ) -> Option<RootAction> {
        if s.radio_change_pending
            && matches!(
                c,
                QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
            )
        {
            return None;
        }
        match c {
            QuickSettingsControl::Wifi => {
                if s.radios.wifi_available {
                    pending.wifi = Some(!s.wifi);
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
                pending.airplane = Some(!s.radios.airplane_mode);
            }
            QuickSettingsControl::MobileData => {
                s.radios.wwan_enabled = !s.radios.wwan_enabled;
                pending.mobile = Some(s.radios.wwan_enabled);
            }
            QuickSettingsControl::DoNotDisturb => {
                s.do_not_disturb = !s.do_not_disturb;
                pending.do_not_disturb = Some(s.do_not_disturb);
            }
            QuickSettingsControl::RotationLock => {
                s.rotation_lock = !s.rotation_lock;
                pending.rotation_lock = Some(s.rotation_lock);
            }
        }
        None
    }

    pub fn focus_next(&mut self, state: &QuickSettingsState, backwards: bool) -> bool {
        let stops = (!self.edit_mode).then(|| self.root_stops(state));
        let (count, current) = match &stops {
            // Editable rows, Reset defaults, Done. The header no longer
            // duplicates Done with an unexplained checkmark.
            None => (
                self.edit_controls(state).len().saturating_add(2),
                self.keyboard_focus,
            ),
            Some(stops) => (stops.len(), self.root_focus_index(stops)),
        };
        if count == 0 {
            return false;
        }
        let next = match (current, backwards) {
            (Some(i), true) => i.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1)),
            (Some(i), false) => i
                .checked_add(1)
                .and_then(|next| next.checked_rem(count))
                .unwrap_or_default(),
            (None, true) => count.saturating_sub(1),
            (None, false) => 0,
        };
        self.keyboard_focus = Some(next);
        self.root_focus = stops.and_then(|stops| stops.get(next).copied());
        self.reveal_focus(state);
        true
    }
    /// Whether the next [`Self::focus_next`] in this direction would wrap
    /// around the page, so a host can hand focus to a neighbouring surface
    /// instead.
    pub fn focus_would_wrap(&self, state: &QuickSettingsState, backwards: bool) -> bool {
        if self.edit_mode {
            return false;
        }
        let stops = self.root_stops(state);
        let last = stops.len().saturating_sub(1);
        match (self.root_focus_index(&stops), backwards) {
            // No selection is not a wrap: Super+Q then Shift+Tab must land
            // on Power. The card only intercepts wrap off a real first stop.
            (Some(0), true) => true,
            (Some(i), false) => i >= last,
            _ => false,
        }
    }
    pub fn activate_focused(&mut self, panel: Rect, state: &mut QuickSettingsState) -> RootAction {
        self.lay_out(panel, state);
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
                self.root_focus = None;
                return RootAction::LayoutChanged;
            }
            return RootAction::None;
        }
        match self.focused_stop(state) {
            Some(stop) => self.activate_stop(stop, state),
            None => RootAction::None,
        }
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
        let Some(RootStop::Slider(index)) = self.focused_stop(state) else {
            return None;
        };
        if !self.slider_available(state, index) {
            return None;
        }
        let value = (self.slider_value(state, index) + direction * 0.05).clamp(0., 1.);
        self.set_slider_value(state, index, value);
        Some(self.slider_commit(state, index))
    }
    /// The next profile the daemon offers. A machine without a
    /// performance mode (most ARM laptops) would otherwise be asked for it
    /// on every second press, which fails and leaves the row stuck.
    fn cycle_power(s: &mut QuickSettingsState) -> RootAction {
        const ORDER: [PowerProfile; 3] = [
            PowerProfile::Saver,
            PowerProfile::Balanced,
            PowerProfile::Performance,
        ];
        let offered = |profile: &PowerProfile| {
            s.power_profiles.is_empty() || s.power_profiles.contains(profile)
        };
        let current = ORDER
            .iter()
            .position(|profile| *profile == s.power_profile)
            .unwrap_or(1);
        let Some(next) = ORDER
            .iter()
            .cycle()
            .skip(current.saturating_add(1))
            .take(ORDER.len())
            .copied()
            .find(offered)
            .filter(|next| *next != s.power_profile)
        else {
            return RootAction::None;
        };
        s.power_profile = next;
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
        let panel = self.lay_out(panel, state);
        if self.edit_mode {
            return self.edit_click(point, state);
        }
        for i in 0..self.slider_count() {
            let (bar, _) = self.slider_geometry_for(panel, i);
            // The pointer target is the painted face — the same rect the
            // accessibility tree publishes — not a second band around the track.
            if self.slider_face_rect(panel, i).contains(point) {
                let value = f32v(((point.x - bar.x0) / bar.width()).clamp(0., 1.));
                self.set_slider_value(state, i, value);
                self.slider_drag = Some(i);
                return RootAction::Changed(PendingRootActions::default());
            }
        }
        let Some(stop) = self
            .root_stops(state)
            .into_iter()
            .find(|stop| self.stop_hit_rect(*stop, state).contains(point))
        else {
            return RootAction::None;
        };
        if let RootStop::Control(_, control) = stop
            && Self::has_details(control, state)
            && self
                .control_details_rect(self.stop_rect(stop, state))
                .contains(point)
        {
            return RootAction::Enter(Self::details_level(control));
        }
        self.activate_stop(stop, state)
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
        let panel = self.lay_out(panel, state);
        if let Some(i) = self.slider_drag {
            let (bar, _) = self.slider_geometry_for(panel, i);
            let v = f32v(((point.x - bar.x0) / bar.width()).clamp(0., 1.));
            self.set_slider_value(state, i, v);
            return true;
        }
        if self.edit_drag.is_pending() {
            // The reorderable list is the active rows; available chips below
            // it are not drop slots.
            return self.edit_drag.pointer_moved(
                point,
                &self.edit_list_metrics(panel, self.active_edit_controls(state).len()),
            );
        }
        false
    }
    pub fn finish_drag(&mut self, state: &QuickSettingsState) -> Option<SliderCommit> {
        if let Some(i) = self.slider_drag.take() {
            return Some(self.slider_commit(state, i));
        }
        // The controller reports visible row indices. Hidden persisted
        // controls (a VPN slot with no profiles) sit between them in the
        // saved order, so map both ends to their saved positions, exactly as
        // keyboard reordering does.
        if let Some((from, to)) = self.edit_drag.commit() {
            let active = self.active_edit_controls(state);
            let saved = |control: QuickSettingsControl| {
                self.config.controls.iter().position(|c| *c == control)
            };
            if let (Some(from), Some(to)) = (
                active.get(from).copied().and_then(saved),
                active.get(to).copied().and_then(saved),
            ) && from != to
            {
                let c = self.config.controls.remove(from);
                self.config.controls.insert(to, c);
                self.save_config();
            }
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
        self.sync_scroll(panel, theme, state);
        self.lay_out(panel, state);
        // A pointer over the bar lane or outside the shown page hovers
        // nothing on the page.
        let scrolls = self.scrolls();
        let cursor = cursor.filter(|point| !scrolls || self.visible_rect().contains(*point));
        // Panel role (HIG §9.1/§9.2): the host paints the one Panel surface
        // — `panel_ground` with its derived `panel_border` — and the root
        // paints nothing under its controls. A second fill here (formerly
        // `surfaces.raised`) covered the inner half of that border and made
        // the tiles' own `raised` faces vanish into it.
        if scrolls {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.visible_rect(),
            );
        }
        self.header(scene, transform, text, theme, cursor, state);
        if self.edit_mode {
            self.draw_edit(scene, transform, text, theme, cursor, state);
        } else {
            self.draw_root(scene, transform, text, theme, cursor, state);
        }
        if scrolls {
            scene.pop_layer();
            self.scroll.paint_bars(scene, transform, theme);
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
        // Panel title (HIG §9.6): `scale.base` semibold, centred on the
        // header band by its measured height so it shares a centre line
        // with the Edit button instead of sitting on an authored baseline
        // that large text pushed into the action tiles.
        let title = t(if self.edit_mode {
            "qs-edit-controls"
        } else {
            "qs-title"
        });
        let title = title.as_str();
        let title_style = theme.text(TextRole::Body);
        let header = self.header_rect();
        let (_, title_h) = text.measure_style(
            title,
            None,
            &title_style.clone().with_weight(TextWeight::Semibold),
        );
        text.draw_style(
            scene,
            title,
            colors.text_primary,
            transform
                .pre_translate((header.x0, header.center().y - f64::from(title_h) / 2.).into()),
            None,
            &title_style.with_weight(TextWeight::Semibold),
        );
        if !self.edit_mode {
            let r = self.edit_rect();
            let icon = ui::icons::Icon::Pencil;
            let name = t("qs-edit-controls");
            let name = name.as_str();
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(r, "")
                    .role(ButtonRole::Plain)
                    .hovered(cursor.is_some_and(|p| r.contains(p)))
                    .focused(self.is_focused(state, RootStop::Edit))
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
        for tile in RootTile::ALL {
            let (icon, label) = (tile.icon(), tile.label());
            let label = label.as_str();
            let r = self.stop_rect(RootStop::Tile(tile), state);
            let power = tile == RootTile::Power;
            paint_control(
                scene,
                transform,
                theme,
                r,
                cursor.is_some_and(|p| r.contains(p)),
                self.is_focused(state, RootStop::Tile(tile)),
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
            let mut label_style = theme.text(TextRole::Label);
            let (unscaled_label_width, _) = text.measure_style(label, None, &label_style);
            if f64::from(unscaled_label_width) > max_label_width {
                label_style.size *= f32v(max_label_width / f64::from(unscaled_label_width));
            }
            let (label_width, label_height) = text.measure_style(label, None, &label_style);
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
            text.draw_style(
                scene,
                label,
                color,
                transform.pre_translate(
                    (
                        r.center().x - f64::from(label_width) / 2.,
                        block_top + glyph + gap,
                    )
                        .into(),
                ),
                None,
                &label_style,
            );
        }
        // The slider block is one Control surface holding two pointer
        // bands, each with its glyph on the leading inset of the band.
        let sliders = self.sliders_rect(self.panel);
        paint_control(scene, transform, theme, sliders, false, false);
        let caption_style = theme.text(TextRole::Caption);
        for i in 0..self.slider_count() {
            let face = self.slider_face_rect(self.panel, i);
            let available = self.slider_available(state, i);
            let band = Rect::new(sliders.x0, face.y0, sliders.x1, face.y1);
            if self.slider_caption_height(i) > 0.0
                && let Some(name) = self.display_names.get(i)
            {
                text.draw_style(
                    scene,
                    name,
                    colors.text_dim,
                    transform
                        .pre_translate((face.x0, face.y0 - self.slider_caption_height(i)).into()),
                    Some(f32v(face.width())),
                    &caption_style,
                );
            }
            ui::icons::draw_icon(
                scene,
                transform,
                leading_glyph(band, inset, glyph),
                if i == self.volume_slider() {
                    ui::icons::Icon::Volume2
                } else {
                    ui::icons::Icon::Sun
                },
                colors.text_dim,
                1.5,
            );
            Slider::paint(
                scene,
                transform,
                theme,
                &SliderFace::new(face, f64::from(self.slider_value(state, i)))
                    .hovered(available && cursor.is_some_and(|p| face.contains(p)))
                    .focused(self.is_focused(state, RootStop::Slider(i)))
                    .dragging(self.slider_drag == Some(i))
                    .disabled(!available),
            );
        }
        let controls = self.visible_controls(state);
        for (i, c) in controls.iter().copied().enumerate() {
            self.draw_control(
                &mut PaintCtx::new(scene, transform, text, theme),
                cursor,
                state,
                i,
                controls.len(),
                c,
            );
        }
        for row in [
            LinkRow {
                stop: RootStop::Devices,
                icon: ui::icons::Icon::Monitor,
                label: t("qs-devices"),
                detail: Self::devices_subline(state),
            },
            LinkRow {
                stop: RootStop::Clipboard,
                icon: ui::icons::Icon::ClipboardPaste,
                label: t("qs-clipboard"),
                detail: Self::clipboard_subline(state),
            },
        ] {
            self.draw_link_row(
                &mut PaintCtx::new(scene, transform, text, theme),
                cursor,
                state,
                &row,
            );
        }
        // The power row: battery glyph, profile and detail, chevron — all
        // on the row's centre line.
        let r = self.power_geometry(self.panel, state);
        paint_control(
            scene,
            transform,
            theme,
            r,
            cursor.is_some_and(|p| r.contains(p)),
            self.is_focused(state, RootStop::PowerProfile),
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
        let power_label_style = theme.text(TextRole::Caption);
        let detail_style = theme.text(TextRole::Caption);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let power_label = text.elide_to_width_style(
            &state.power_profile.label(),
            text_width as f32,
            &power_label_style,
        );
        let battery_detail = power_detail(state);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let battery_detail =
            text.elide_to_width_style(&battery_detail, text_width as f32, &detail_style);
        let (_, power_label_h) = text.measure_style(
            &power_label,
            None,
            &power_label_style.clone().with_weight(TextWeight::Semibold),
        );
        let (_, detail_h) = text.measure_style(&battery_detail, None, &detail_style);
        // Label and detail are one measured block on the row's centre line
        // (HIG §7a: measure and draw with one style; the detail follows the
        // label line, never a fixed offset the label can outgrow).
        let block_top = centred_text_top(r, f64::from(power_label_h) + f64::from(detail_h));
        text.draw_style(
            scene,
            &power_label,
            colors.text_primary,
            transform.pre_translate((text_x, block_top).into()),
            None,
            &power_label_style.clone().with_weight(TextWeight::Semibold),
        );
        text.draw_style(
            scene,
            &battery_detail,
            colors.text_dim,
            transform.pre_translate((text_x, block_top + f64::from(power_label_h)).into()),
            None,
            &detail_style,
        );
    }
    /// A drill-in row (Devices, Clipboard): leading glyph, label over a
    /// detail line, trailing chevron — the power row's anatomy at half width.
    fn draw_link_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        cursor: Option<Point>,
        state: &QuickSettingsState,
        row: &LinkRow,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let LinkRow {
            stop,
            icon,
            ref label,
            ref detail,
        } = *row;
        let colors = &theme.tokens.colors;
        let glyph = Self::glyph_at(self.layout_scale);
        let inset = self.scaled(CONTROL_INSET);
        let r = self.stop_rect(stop, state);
        paint_control(
            scene,
            transform,
            theme,
            r,
            cursor.is_some_and(|p| r.contains(p)),
            self.is_focused(state, stop),
        );
        let leading = leading_glyph(r, inset, glyph);
        ui::icons::draw_icon(scene, transform, leading, icon, colors.text_primary, 1.5);
        let chevron = trailing_glyph(r, inset, glyph);
        ui::icons::draw_icon(
            scene,
            transform,
            chevron,
            ui::icons::Icon::ChevronRight,
            colors.text_dim,
            1.5,
        );
        let text_x = leading.x1 + inset;
        let text_width = f32v((chevron.x0 - self.scaled(RELATED_GAP) - text_x).max(1.0));
        let label_style = theme.text(TextRole::Caption);
        let detail_style = theme.text(TextRole::Caption);
        let label = text.elide_to_width_style(label, text_width, &label_style);
        let detail = text.elide_to_width_style(detail, text_width, &detail_style);
        let (_, label_h) = text.measure_style(
            &label,
            None,
            &label_style.clone().with_weight(TextWeight::Semibold),
        );
        let (_, detail_h) = text.measure_style(&detail, None, &detail_style);
        let block_top = centred_text_top(r, f64::from(label_h) + f64::from(detail_h));
        text.draw_style(
            scene,
            &label,
            colors.text_primary,
            transform.pre_translate((text_x, block_top).into()),
            None,
            &label_style.clone().with_weight(TextWeight::Semibold),
        );
        text.draw_style(
            scene,
            &detail,
            colors.text_dim,
            transform.pre_translate((text_x, block_top + f64::from(label_h)).into()),
            None,
            &detail_style,
        );
    }
    fn draw_control(
        &self,
        ctx: &mut PaintCtx<'_>,
        cursor: Option<Point>,
        s: &QuickSettingsState,
        i: usize,
        count: usize,
        c: QuickSettingsControl,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        let r = self.control_card(self.panel, i, count);
        let on = Self::on(c, s);
        paint_control(
            scene,
            transform,
            theme,
            r,
            cursor.is_some_and(|p| r.contains(p)),
            self.is_focused(s, RootStop::Control(i, c)),
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
                colors.intent.inverted
            } else {
                colors.text_primary
            },
            1.5,
        );
        let label_style = theme.text(TextRole::Caption);
        let sub_style = theme.text(TextRole::Caption);
        let text_width = (r.width() - 2.0 * inset).max(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let label =
            text.elide_to_width_style(&Self::rendered_label(c, s), text_width as f32, &label_style);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let sub = text.elide_to_width_style(&Self::subline(c, s), text_width as f32, &sub_style);
        let label_y = icon_row.y1 + self.scaled(RELATED_GAP);
        let (_, label_h) = text.measure_style(
            &label,
            None,
            &label_style.clone().with_weight(TextWeight::Semibold),
        );
        text.draw_style(
            scene,
            &label,
            colors.text_primary,
            transform.pre_translate((r.x0 + inset, label_y).into()),
            None,
            &label_style.clone().with_weight(TextWeight::Semibold),
        );
        // The sub-line sits under the measured label rather than at a fixed
        // offset the label outgrows at large text (HIG §7a).
        text.draw_style(
            scene,
            &sub,
            colors.text_dim,
            transform.pre_translate((r.x0 + inset, label_y + f64::from(label_h)).into()),
            None,
            &sub_style,
        );
        if (matches!(c, QuickSettingsControl::Wifi) && s.radios.wifi_available)
            || matches!(
                c,
                QuickSettingsControl::Bluetooth | QuickSettingsControl::Vpn
            )
        {
            let details = self.control_details_rect(r);
            let name = Self::details_label(c);
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::icon_only(details, ui::icons::Icon::ChevronRight, &name)
                    .hovered(cursor.is_some_and(|point| details.contains(point)))
                    .focused(self.is_focused(s, RootStop::Details(i, c))),
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
                )
                .disabled(s.radio_change_pending && c == QuickSettingsControl::AirplaneMode),
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
        let caption_style = theme.text(TextRole::Caption);
        // Section caption on the control padding under the title band.
        text.draw_style(
            scene,
            &Self::edit_help_copy(),
            colors.text_dim,
            transform.pre_translate(
                (
                    self.panel.x0 + self.scaled(Self::PAD),
                    self.edit_help_baseline(),
                )
                    .into(),
            ),
            None,
            &caption_style,
        );
        text.draw_style(
            scene,
            &t("qs-edit-active-heading"),
            colors.text_dim,
            transform.pre_translate((self.panel.x0 + pad, self.edit_section_baseline()).into()),
            None,
            &caption_style,
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
            ui::surface::shadow(
                scene,
                transform,
                ghost,
                radius,
                &theme.tokens.elevation.floating,
            );
            ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(true))
                .paint(scene, transform, ghost, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.inverted,
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
                    colors.intent.inverted,
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
            text.draw_style(
                scene,
                &t("qs-edit-available-heading"),
                colors.text_dim,
                transform
                    .pre_translate((self.panel.x0 + pad, top + self.scaled(CONTROL_PAD)).into()),
                None,
                &caption_style,
            );
            for (i, (control, chip)) in chips.into_iter().enumerate() {
                let label = Self::label(control);
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(chip, &label)
                        .icon(ui::icons::Icon::Plus)
                        .hovered(cursor.is_some_and(|point| chip.contains(point)))
                        .focused(self.keyboard_focus == Some(active.len().saturating_add(i)))
                        .accessible_name(&label),
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
            &ButtonFace::new(reset, &t("qs-edit-reset"))
                .hovered(cursor.is_some_and(|p| reset.contains(p)))
                .focused(self.keyboard_focus == Some(edit_count))
                .accessible_name(&t("qs-edit-reset-a11y")),
        );
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(done, &t("qs-edit-done"))
                .role(ButtonRole::Primary)
                .hovered(cursor.is_some_and(|p| done.contains(p)))
                .focused(self.keyboard_focus == Some(edit_count.saturating_add(1)))
                .accessible_name(&t("qs-edit-done-a11y")),
        );
    }
    /// The grip, label and add/remove button of an edit row, shared by the
    /// resting rows and the drag ghost so the two cannot drift apart. Every
    /// element sits on the row's centre line (HIG §7).
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
        let label_style = theme.text(TextRole::Caption);
        let label_x = slot.x1 + inset;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "quick-settings panel geometry is bounded by compositor output dimensions; f32 text width is sufficient for shaping"
        )]
        let label = text.elide_to_width_style(
            &Self::label(c),
            (action.x0 - self.scaled(RELATED_GAP) - label_x).max(1.0) as f32,
            &label_style,
        );
        let (_, label_h) = text.measure_style(
            &label,
            None,
            &label_style.clone().with_weight(TextWeight::Medium),
        );
        text.draw_style(
            scene,
            &label,
            colors.text_primary,
            transform.pre_translate((label_x, centred_text_top(r, f64::from(label_h))).into()),
            None,
            &label_style.clone().with_weight(TextWeight::Medium),
        );
        let action_name = t(if c.required() {
            "qs-edit-required-a11y"
        } else if present {
            "qs-edit-remove-a11y"
        } else {
            "qs-edit-add-a11y"
        });
        let action_face = ButtonFace::new(action, "")
            .hovered(cursor.is_some_and(|point| action.contains(point)))
            .accessible_name(&action_name);
        let action_face = if c.required() {
            action_face.role(ButtonRole::Plain)
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
                .with_name(t("qs-title"))
                .with_description(t(if self.edit_mode {
                    "qs-edit-a11y-description"
                } else {
                    "qs-a11y-description"
                }))
                .with_state(AccessState::new().modal(true)),
            self.panel,
        );
        if !self.edit_mode {
            // Children follow the keyboard focus table, so an assistive
            // technology's child index is the focus stop it names. Each is
            // keyed by the stop it is, not its place: the stops shift when a
            // reading lands after the panel opens (a wired link turns the
            // network tile on, volume becomes available), and a reader that
            // resolved a control a moment earlier must still press that
            // control, not whichever one moved into its slot.
            for stop in self.root_stops(state) {
                let node = self
                    .stop_access_node(stop, state)
                    .with_hashed_key(self.semantic_stop_name(stop))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.is_focused(state, stop)),
                    );
                let node = match stop {
                    RootStop::Control(_, control) if Self::is_switch(control) => {
                        let mut node = node;
                        node.state.checked = Some(Self::on(control, state));
                        node.state.disabled = state.radio_change_pending
                            && control == QuickSettingsControl::AirplaneMode;
                        node
                    }
                    _ => node,
                };
                root.children
                    .push(bounded(node, self.stop_rect(stop, state)));
            }
            return root;
        }
        let controls: Vec<_> = self
            .active_edit_controls(state)
            .into_iter()
            .chain(self.available_edit_controls(state))
            .collect();
        let active_count = self.active_edit_controls(state).len();
        for (index, c) in controls.into_iter().enumerate() {
            let node = AccessNode::new(Role::Button)
                .with_name(Self::rendered_label(c, state))
                .with_description(t(if c.required() {
                    "qs-edit-required-control-a11y"
                } else {
                    "qs-edit-optional-control-a11y"
                }))
                .with_state(AccessState::new().focusable(true));
            let rect = if index < active_count {
                self.card(self.panel, index, true)
            } else {
                // index >= active_count on this branch, so this never saturates.
                self.available_edit_chip(state, index.saturating_sub(active_count))
            };
            root.children.push(bounded(node, rect));
        }
        {
            root.children.push(bounded(
                AccessNode::new(Role::Button)
                    .with_name(t("qs-edit-reset"))
                    .with_state(AccessState::new().focusable(true)),
                self.edit_reset(state),
            ));
            root.children.push(bounded(
                AccessNode::new(Role::Button)
                    .with_name(t("qs-edit-done"))
                    .with_state(AccessState::new().focusable(true)),
                self.edit_done(state),
            ));
        }
        root
    }
    const fn is_switch(control: QuickSettingsControl) -> bool {
        !matches!(control, QuickSettingsControl::Vpn)
    }
    fn stop_access_node(&self, stop: RootStop, state: &QuickSettingsState) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};
        match stop {
            RootStop::Slider(index) => {
                let value = self.slider_value(state, index);
                AccessNode::new(Role::Slider)
                    .with_name(self.slider_name(index))
                    .with_value(format!("{:.0}%", value * 100.0))
                    .with_description(t("qs-slider-a11y-description"))
            }
            RootStop::Control(_, control) => AccessNode::new(if Self::is_switch(control) {
                Role::Switch
            } else {
                Role::Button
            })
            .with_name(Self::rendered_label(control, state))
            .with_description(Self::subline(control, state)),
            RootStop::Details(_, control) => {
                AccessNode::new(Role::Button).with_name(Self::details_label(control))
            }
            RootStop::Devices => AccessNode::new(Role::Button)
                .with_name(t("qs-devices"))
                .with_value(Self::devices_subline(state))
                .with_description(t("qs-devices-a11y-description")),
            RootStop::Clipboard => AccessNode::new(Role::Button)
                .with_name(t("qs-clipboard"))
                .with_value(Self::clipboard_subline(state))
                .with_description(t("qs-clipboard-a11y-description")),
            RootStop::PowerProfile => AccessNode::new(Role::Button)
                .with_name(t("qs-power-profile-a11y"))
                .with_value(format!(
                    "{}, {}",
                    state.power_profile.label(),
                    power_detail(state)
                )),
            RootStop::Edit => AccessNode::new(Role::Button).with_name(t("qs-edit-controls")),
            RootStop::Tile(tile) => AccessNode::new(Role::Button)
                .with_name(tile.label())
                .with_description(tile.description()),
        }
    }
    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.panel),
            [i] if !self.edit_mode && *i < self.slider_count() => {
                Some(self.slider_geometry_for(self.panel, *i).0)
            }
            _ => None,
        }
    }
    pub fn focus_accessibility_path(&mut self, path: &[usize], state: &QuickSettingsState) -> bool {
        let [i] = path else {
            return false;
        };
        self.keyboard_focus = Some(*i);
        self.root_focus = if self.edit_mode {
            None
        } else {
            self.root_stops(state).get(*i).copied()
        };
        true
    }
}

/// The power row's second line: the battery, and whichever app is keeping
/// the screen from sleeping.
fn power_detail(state: &QuickSettingsState) -> String {
    let power = if state.battery.present {
        t_args(
            "qs-power-battery",
            &[("percent", state.battery.percent.into())],
        )
    } else {
        t("qs-power-ac")
    };
    match state.awake_holders.as_slice() {
        [] => power,
        [app, rest @ ..] if rest.iter().all(|other| other == app) => t_args(
            "qs-power-kept-awake-by-app",
            &[("power", power.into()), ("app", app.into())],
        ),
        many => t_args(
            "qs-power-kept-awake-by-apps",
            &[("power", power.into()), ("count", many.len().into())],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PANEL: Rect = Rect::new(100., 48., 500., 560.);

    #[test]
    fn the_power_row_names_the_app_keeping_the_screen_awake() {
        let mut state = QuickSettingsState::fixture();
        assert_eq!(power_detail(&state), "AC power");
        state.awake_holders = vec!["Firefox".into()];
        assert_eq!(
            power_detail(&state),
            "AC power \u{b7} Firefox is preventing sleep"
        );
        state.awake_holders = vec!["Firefox".into(), "Firefox".into()];
        assert_eq!(
            power_detail(&state),
            "AC power \u{b7} Firefox is preventing sleep"
        );
        state.awake_holders = vec!["Firefox".into(), "VLC".into()];
        assert_eq!(
            power_detail(&state),
            "AC power \u{b7} 2 apps are preventing sleep"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn root_controls_reflect_present_hardware_and_migrated_config() {
        fn semantic_targets_cover_every_visible_root_control_and_edit_action() {
            let mut root = QuickSettingsRoot::default();
            let state = QuickSettingsState {
                brightness_available: true,
                bluetooth_available: true,
                radios: os_actions::RadioState {
                    wifi_available: true,
                    ..os_actions::RadioState::default()
                },
                ..QuickSettingsState::default()
            };
            root.panel = PANEL;

            let targets = root.semantic_rects(&state);
            for required in [
                "quick_settings_edit",
                "quick_settings_settings",
                "quick_settings_privacy",
                "quick_settings_history",
                "quick_settings_power",
                "quick_settings_devices",
                "quick_settings_clipboard",
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

        fn edit_catalogue_does_not_offer_unavailable_mobile_data() {
            let root = QuickSettingsRoot::default();
            let state = QuickSettingsState::default();
            assert!(
                !root
                    .edit_controls(&state)
                    .contains(&QuickSettingsControl::MobileData)
            );
        }

        semantic_targets_cover_every_visible_root_control_and_edit_action();
        default_omits_airplane_and_dead_radios();
        saved_airplane_choice_remains_removable_without_current_radio_hardware();
        network_slot_is_hardware_truthful_and_unifies_wired();
        accessibility_exposes_only_present_hardware_with_dynamic_network_name();
        config_migrates_duplicates_and_required_wifi();
        edit_catalogue_does_not_offer_unavailable_mobile_data();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn root_focus_order_matches_visible_actions_and_edit_mode() {
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

        /// Super+Q then Shift+Tab must still open Power when brightness, Wi-Fi
        /// and Bluetooth are absent. Those controls sit *before* the utility
        /// row; hiding them must not move the last stop off Power, or the
        /// clipboard e2e lock path (and a real user) activates History.
        fn reverse_tab_from_no_selection_opens_power_without_hardware() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            root.panel = PANEL;
            assert!(
                !state.brightness_available && state.displays.is_empty(),
                "default is a machine with no backlight"
            );
            assert!(!state.radios.wifi_available && !state.bluetooth_available);
            root.reset_focus();
            assert!(root.focus_next(&state, true));
            assert_eq!(
                root.focused_stop(&state),
                Some(RootStop::Tile(RootTile::Power))
            );
            assert_eq!(
                root.activate_focused(PANEL, &mut state),
                RootAction::OpenPowerMenu
            );
        }

        /// Device state arrives while the popover is open: a slider or
        /// control that appears or goes away changes the stop list under a
        /// focused stop. Focus and Enter stay on the stop the user reached.
        fn focus_stays_on_its_stop_when_stops_appear_or_vanish() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState {
                volume_available: true,
                ..QuickSettingsState::default()
            };
            root.panel = PANEL;
            root.reset_focus();
            assert!(root.focus_next(&state, true));
            assert_eq!(
                root.focused_stop(&state),
                Some(RootStop::Tile(RootTile::Power))
            );

            // The volume probe fails: one stop fewer before Power.
            state.volume_available = false;
            assert_eq!(
                root.focused_stop(&state),
                Some(RootStop::Tile(RootTile::Power))
            );
            // A backlight appears: one stop more.
            state.brightness_available = true;
            assert_eq!(
                root.focused_stop(&state),
                Some(RootStop::Tile(RootTile::Power))
            );
            assert!(!root.focus_would_wrap(&state, true));
            assert_eq!(
                root.activate_focused(PANEL, &mut state),
                RootAction::OpenPowerMenu
            );

            // Stepping continues from where focus is, not from a stale index.
            assert!(root.focus_next(&state, true));
            let stops = root.root_stops(&state);
            let power = stops
                .iter()
                .position(|s| *s == RootStop::Tile(RootTile::Power))
                .unwrap();
            assert_eq!(root.focused_stop(&state), stops.get(power - 1).copied());

            // A focused slider that goes away leaves nothing focused rather
            // than handing Enter to its neighbour.
            root.reset_focus();
            assert!(root.focus_next(&state, false));
            assert_eq!(root.focused_stop(&state), Some(RootStop::Slider(0)));
            state.brightness_available = false;
            assert_eq!(root.focused_stop(&state), None);
            assert_eq!(root.activate_focused(PANEL, &mut state), RootAction::None);
        }

        fn normal_focus_order_matches_visible_actions_and_accessibility_paths() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            root.panel = PANEL;
            let stops = root.root_stops(&state);
            let position = |stop| stops.iter().position(|s| *s == stop).unwrap();
            for (stop, expected) in [
                (RootStop::Devices, RootAction::Enter(QsLevel::Devices)),
                (RootStop::Clipboard, RootAction::Enter(QsLevel::Clipboard)),
                (RootStop::Tile(RootTile::Settings), RootAction::OpenSettings),
                (RootStop::Tile(RootTile::Privacy), RootAction::OpenPrivacy),
                (
                    RootStop::Tile(RootTile::History),
                    RootAction::Enter(QsLevel::Notifications),
                ),
                (RootStop::Tile(RootTile::Power), RootAction::OpenPowerMenu),
                (RootStop::Edit, RootAction::LayoutChanged),
            ] {
                let path = position(stop);
                assert!(root.focus_accessibility_path(&[path], &state));
                assert_eq!(root.focused_stop(&state), Some(stop));
                assert_eq!(root.activate_focused(PANEL, &mut state), expected);
                root.edit_mode = false;
            }
        }

        /// Every stop paints, publishes and activates as the same control: the
        /// accessibility child at a focus index is the one node marked focused,
        /// and it carries the rect that stop paints.
        fn each_focus_stop_marks_exactly_one_accessibility_node() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            state.radios.wifi_available = true;
            state.bluetooth_available = true;
            root.panel = PANEL;
            let stops = root.root_stops(&state);
            for (index, stop) in stops.iter().copied().enumerate() {
                assert!(root.focus_next(&state, false));
                assert_eq!(root.keyboard_focus, Some(index));
                let snapshot = root.accessibility_snapshot(&state);
                assert_eq!(snapshot.children.len(), stops.len());
                let focused: Vec<_> = snapshot
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| node.state.focused)
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(focused, vec![index], "{stop:?}");
                assert_eq!(
                    snapshot.children[index]
                        .bounds
                        .map(ui::AccessBounds::to_rect),
                    Some(root.stop_rect(stop, &state)),
                    "{stop:?}"
                );
            }
            assert!(root.focus_next(&state, false));
            assert_eq!(root.keyboard_focus, Some(0), "Tab wraps to Brightness");
        }

        fn every_destination_tile_click_routes_to_its_action() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            root.panel = PANEL;
            for tile in RootTile::ALL {
                let centre = root.stop_rect(RootStop::Tile(tile), &state).center();
                assert_eq!(root.handle_click(centre, PANEL, &mut state), tile.action());
            }
            for (stop, level) in [
                (RootStop::Devices, QsLevel::Devices),
                (RootStop::Clipboard, QsLevel::Clipboard),
            ] {
                let centre = root.stop_rect(stop, &state).center();
                assert_eq!(
                    root.handle_click(centre, PANEL, &mut state),
                    RootAction::Enter(level)
                );
            }
        }

        fn devices_and_clipboard_rows_summarise_their_panes() {
            let mut state = QuickSettingsState::default();
            assert_eq!(
                QuickSettingsRoot::devices_subline(&state),
                "Audio and media"
            );
            state.personal_devices = Some(3);
            assert_eq!(QuickSettingsRoot::devices_subline(&state), "3 devices");
            state.personal_devices = Some(1);
            assert_eq!(QuickSettingsRoot::devices_subline(&state), "1 device");
            assert_eq!(QuickSettingsRoot::clipboard_subline(&state), "Empty");
            state.clipboard_items = 4;
            assert_eq!(QuickSettingsRoot::clipboard_subline(&state), "4 items");
        }

        fn power_profile_focus_matches_its_enter_action() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            state.radios.wifi_available = true;
            state.bluetooth_available = true;
            let focus = root
                .root_stops(&state)
                .iter()
                .position(|stop| *stop == RootStop::PowerProfile)
                .unwrap();

            // The renderer lights the power-profile row at this same index.
            // Enter must therefore cycle that row, not the preceding control.
            root.keyboard_focus = Some(focus);
            assert!(root.is_focused(&state, RootStop::PowerProfile));
            let previous = state.power_profile;
            let action = root.activate_focused(PANEL, &mut state);
            assert_eq!(
                action,
                RootAction::Changed(PendingRootActions {
                    power: Some(state.power_profile),
                    ..Default::default()
                })
            );
            assert_ne!(state.power_profile, previous);
        }

        fn power_profile_cycles_only_through_offered_profiles() {
            // An ARM laptop's daemon offers no performance mode.
            let mut state = QuickSettingsState {
                power_profiles: vec![PowerProfile::Saver, PowerProfile::Balanced],
                ..QuickSettingsState::default()
            };
            let mut seen = Vec::new();
            for _ in 0..4 {
                let _ = QuickSettingsRoot::cycle_power(&mut state);
                seen.push(state.power_profile);
            }
            assert_eq!(
                seen,
                [
                    PowerProfile::Saver,
                    PowerProfile::Balanced,
                    PowerProfile::Saver,
                    PowerProfile::Balanced
                ]
            );
            state.power_profiles = vec![PowerProfile::Balanced];
            assert_eq!(
                QuickSettingsRoot::cycle_power(&mut state),
                RootAction::None,
                "with one profile there is nothing to switch to"
            );
        }

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

        opening_has_no_hidden_focus_and_first_tab_selects_brightness();
        reverse_tab_from_no_selection_opens_power_without_hardware();
        normal_focus_order_matches_visible_actions_and_accessibility_paths();
        focus_stays_on_its_stop_when_stops_appear_or_vanish();
        each_focus_stop_marks_exactly_one_accessibility_node();
        every_destination_tile_click_routes_to_its_action();
        devices_and_clipboard_rows_summarise_their_panes();
        power_profile_focus_matches_its_enter_action();
        power_profile_cycles_only_through_offered_profiles();
        finishing_edit_clears_removed_done_focus_before_root_activation();
        edit_focus_order_covers_visible_rows_available_controls_and_footer();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn edit_mode_adds_removes_and_reorders_by_keyboard_pointer_and_explicit_remove() {
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
            // Visible order moves even across the hidden VPN slot.
            assert_eq!(
                root.active_edit_controls(&state),
                [
                    QuickSettingsControl::Wifi,
                    QuickSettingsControl::NightLight,
                    QuickSettingsControl::Bluetooth,
                ]
            );
        }

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

        fn edit_remove_action_has_a_clear_wide_target() {
            let root = QuickSettingsRoot {
                edit_mode: true,
                panel: PANEL,
                ..Default::default()
            };
            let row = root.card(PANEL, 0, true);
            let action = root.edit_action_rect(row);
            assert_eq!(action.width(), QuickSettingsRoot::EDIT_ACTION_WIDTH);
            assert!(action.x0 > row.x0);
            assert!(action.x1 < row.x1);
            assert!(action.y0 > row.y0 && action.y1 < row.y1);
        }

        fn edit_header_keeps_help_section_and_first_control_visually_separate() {
            let mut root = QuickSettingsRoot {
                edit_mode: true,
                ..Default::default()
            };
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

        fn edit_help_copy_uses_font_safe_keyboard_names() {
            let copy = QuickSettingsRoot::edit_help_copy();
            assert_eq!(copy, "Drag to reorder · Ctrl + Up/Down");
            assert!(!copy.contains('↑'));
            assert!(!copy.contains('↓'));
        }

        edit_add_remove_and_keyboard_reorder_change_model();
        pointer_drag_uses_shared_controller_and_commits();
        edit_row_body_reorders_and_only_explicit_action_removes();
        edit_remove_action_has_a_clear_wide_target();
        edit_header_keeps_help_section_and_first_control_visually_separate();
        edit_help_copy_uses_font_safe_keyboard_names();
    }

    /// Lay `root` out in a popover `viewport` shorter than its page, then
    /// Tab through every stop: each lands fully inside the shown part of the
    /// popover, scrolled there the way a list follows its focus, at the
    /// size it has when nothing scrolls.
    fn assert_reachable_by_scrolling(
        root: &mut QuickSettingsRoot,
        state: &QuickSettingsState,
        viewport: Rect,
    ) {
        let theme = Theme::dark();
        root.sync_displays(state);
        root.sync_scroll(viewport, &theme, state);
        root.lay_out(viewport, state);
        assert!(root.scrolls(), "a page taller than {viewport:?} scrolls");
        let shown = root.visible_rect();
        let stops = root.root_stops(state);
        root.reset_focus();
        let mut deepest: f64 = 0.0;
        for stop in &stops {
            root.focus_next(state, false);
            assert_eq!(root.focused_stop(state), Some(*stop));
            let rect = root.stop_rect(*stop, state);
            assert!(
                rect.y0 >= shown.y0 - 1e-6 && rect.y1 <= shown.y1 + 1e-6,
                "{stop:?} at {rect:?} is not shown in {shown:?} (offset {})",
                root.scroll_offset()
            );
            deepest = deepest.max(root.scroll_offset());
        }
        assert!(deepest > 0.0, "reaching the last stops scrolled the page");
        let targets = root.semantic_rects(state);
        assert!(!targets.is_empty());
        for (name, rect) in targets {
            assert!(
                shown.contains(rect.center()),
                "{name} targets {rect:?} outside {shown:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn root_geometry_keeps_targets_separate_and_balanced() {
        fn compact_geometry_never_overlaps() {
            let root = QuickSettingsRoot::default();
            let state = QuickSettingsState::default();
            let panel = Rect::new(0., 48., 400., 48. + root.preferred_height(&state));
            for i in 0..root.control_count(&state) {
                let r = root.control_card(panel, i, root.control_count(&state));
                for j in 0..i {
                    let overlap =
                        r.intersect(root.control_card(panel, j, root.control_count(&state)));
                    assert!(overlap.width() <= 0.0 || overlap.height() <= 0.0);
                }
            }
        }

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
        fn bottom_utilities_keep_independent_touch_targets() {
            let mut root = QuickSettingsRoot::default();
            let state = QuickSettingsState::default();
            root.panel = Rect::new(0., 48., 400., 520.);
            let actions: Vec<_> = [
                RootStop::Edit,
                RootStop::Tile(RootTile::Settings),
                RootStop::Tile(RootTile::Privacy),
                RootStop::Tile(RootTile::History),
                RootStop::Tile(RootTile::Power),
            ]
            .into_iter()
            .map(|stop| root.stop_hit_rect(stop, &state))
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

        fn a_short_popover_keeps_natural_tiles_and_scrolls_to_the_power_profile() {
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
            let natural = root.preferred_height(&state);
            let tile = root.action_height();
            let card = root.card_height();
            let viewport = Rect::new(0.0, 48.0, 400.0, 48.0 + 300.0);
            assert!(natural > viewport.height(), "the fixture must not fit");
            assert_reachable_by_scrolling(&mut root, &state, viewport);
            assert_eq!(root.layout_scale, 1.0, "nothing is squeezed to fit");
            assert_eq!(root.action_height(), tile);
            assert_eq!(root.card_height(), card);
        }

        fn a_root_with_room_keeps_the_token_rhythm() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            state.radios.wifi_available = true;
            state.bluetooth_available = true;
            // A 1080p desktop under the top bar: nothing has to give.
            assert_eq!(root.layout_scale, 1.0);
            let panel = Rect::new(0.0, 48.0, 400.0, 48.0 + root.preferred_height(&state));
            root.panel = panel;
            let count = root.visible_controls(&state).len();
            let first = root.control_card(panel, 0, count);
            let second = root.control_card(panel, 1, count);
            assert_eq!(first.x0 - panel.x0, PANEL_PAD);
            assert_eq!(panel.x1 - second.x1, PANEL_PAD);
            assert_eq!(second.x0 - first.x1, SIBLING_GAP);
            assert!(panel.y1 - root.power_geometry(panel, &state).y1 >= PANEL_PAD);
        }

        fn power_utility_keeps_a_clear_root_target() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            let panel = Rect::new(0., 48., 400., 520.);
            root.panel = panel;
            assert_eq!(
                root.handle_click(
                    root.stop_hit_rect(RootStop::Tile(RootTile::Power), &state)
                        .center(),
                    panel,
                    &mut state
                ),
                RootAction::OpenPowerMenu
            );
        }

        fn accessibility_nodes_carry_the_rects_paint_and_hit_testing_use() {
            let mut root = QuickSettingsRoot::default();
            let state = QuickSettingsState {
                brightness_available: true,
                radios: os_actions::RadioState {
                    wifi_available: true,
                    ..os_actions::RadioState::default()
                },
                ..QuickSettingsState::default()
            };
            root.panel = PANEL;
            let snapshot = root.accessibility_snapshot(&state);
            assert_eq!(snapshot.bounds.map(ui::AccessBounds::to_rect), Some(PANEL));
            for node in &snapshot.children {
                let rect = node.bounds.map_or_else(
                    || panic!("{:?} has no bounds", node.name),
                    ui::AccessBounds::to_rect,
                );
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

        compact_geometry_never_overlaps();
        odd_control_count_uses_a_balanced_full_width_last_card();
        bottom_utilities_keep_independent_touch_targets();
        a_short_popover_keeps_natural_tiles_and_scrolls_to_the_power_profile();
        a_root_with_room_keeps_the_token_rhythm();
        power_utility_keeps_a_clear_root_target();
        accessibility_nodes_carry_the_rects_paint_and_hit_testing_use();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn root_panel_fits_large_text_and_keeps_one_rhythm() {
        fn large_text_keeps_full_size_targets_and_scrolls_on_an_800_by_600_desktop() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            state.radios.wifi_available = true;
            state.bluetooth_available = true;
            root.set_layout_text_scale(1.3);
            // The panel begins below the 48px top bar and leaves the shell's
            // 16px lower margin. Text and targets keep their full size; a
            // page taller than that scrolls instead of shrinking.
            let available = 600.0 - 48.0 - 16.0;
            let viewport = Rect::new(0., 48., 400., 48. + available);
            assert_reachable_by_scrolling(&mut root, &state, viewport);
            // A rect at a fractional `y` loses a few ulps of its height.
            for index in 0..2 {
                assert!(root.slider_face_rect(root.panel, index).height() >= TARGET - 1e-9);
            }
            assert!(root.edit_rect().height() >= TARGET - 1e-9);
            for index in 0..QuickSettingsRoot::ACTION_COUNT {
                assert!(root.action(index).height() >= 44.0);
            }
        }

        fn large_text_reaches_every_root_control_on_a_960_by_540_desktop() {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::default();
            state.radios.wifi_available = true;
            state.bluetooth_available = true;
            root.set_layout_text_scale(1.3);
            let available = 540.0 - 48.0 - 16.0;
            let viewport = Rect::new(0.0, 48.0, 400.0, 48.0 + available);
            assert_reachable_by_scrolling(&mut root, &state, viewport);
        }

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
                        >= (icon_row.y1 - card.y0)
                            + root.scaled(RELATED_GAP)
                            + label
                            + caption
                            + pad,
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
                header.height() >= root.line_height(TextRole::Body),
                "the title band holds a base-scale title line"
            );
            assert!(
                header.y1 <= root.action(0).y0 + f64::EPSILON,
                "the title band ends before the action tiles begin"
            );
            let power = root.power_geometry(panel, &state);
            let inner = root.scaled(f64::from(ui::tokens::Spacing::BASE.s1));
            assert!(power.height() + eps >= 2.0 * inner + label + caption);
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
            let links = [
                root.link_rect(panel, &state, 0),
                root.link_rect(panel, &state, 1),
            ];
            assert!(close(links[0].y0 - cards[2].y1, s4), "cards → rows is s4");
            assert!(close(links[1].x0 - links[0].x1, s3), "row ↔ row is s3");
            let power = root.power_geometry(panel, &state);
            assert!(close(power.y0 - links[0].y1, s3), "rows → power is s3");
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
            let mut all = tiles;
            all.push(sliders);
            all.extend(cards.iter().copied());
            all.extend(links);
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

        large_text_keeps_full_size_targets_and_scrolls_on_an_800_by_600_desktop();
        large_text_reaches_every_root_control_on_a_960_by_540_desktop();
        large_text_rows_grow_to_hold_their_type_and_the_title_stays_clear_of_the_tiles();
        root_geometry_keeps_one_rhythm_at_one_x();
    }

    #[test]
    fn rotation_lock_appears_only_where_the_device_can_rotate() {
        let root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState {
            rotation_available: false,
            ..QuickSettingsState::default()
        };
        assert!(
            !root
                .visible_controls(&state)
                .contains(&QuickSettingsControl::RotationLock)
        );
        assert!(
            !root
                .edit_controls(&state)
                .contains(&QuickSettingsControl::RotationLock)
        );
        state.rotation_available = true;
        assert!(
            root.visible_controls(&state)
                .contains(&QuickSettingsControl::RotationLock)
        );
    }

    #[test]
    fn the_rotation_lock_tile_toggles_the_persisted_lock() {
        let mut state = QuickSettingsState {
            rotation_available: true,
            rotation_lock: false,
            ..QuickSettingsState::default()
        };
        let mut pending = PendingRootActions::default();
        let action =
            QuickSettingsRoot::toggle(QuickSettingsControl::RotationLock, &mut state, &mut pending);
        assert_eq!(action, None);
        assert!(state.rotation_lock);
        assert_eq!(pending.rotation_lock, Some(true));
        assert!(QuickSettingsRoot::on(
            QuickSettingsControl::RotationLock,
            &state
        ));
        assert_eq!(
            QuickSettingsRoot::subline(QuickSettingsControl::RotationLock, &state),
            "Locked"
        );
    }

    /// A reading that lands after the panel opened adds a stop in front of
    /// the power row. The power row's node keeps its key, so a reader that
    /// resolved it before the reading still presses the power row, and no
    /// two stops ever share a key.
    #[test]
    fn root_stops_keep_their_accessible_identity_when_a_stop_appears() {
        let root = QuickSettingsRoot {
            panel: PANEL,
            ..QuickSettingsRoot::default()
        };
        let mut state = QuickSettingsState::default();
        let key_of = |snapshot: &ui::a11y::AccessNode, name: &str| {
            let node = snapshot
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} is in the tree"));
            node.key.unwrap_or_else(|| panic!("{name} carries a key"))
        };
        let before = root.accessibility_snapshot(&state);
        let power = key_of(&before, "Change power profile");
        let power_index = before
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Change power profile"));

        state.radios.wifi_available = true;
        state.volume_available = true;
        let after = root.accessibility_snapshot(&state);
        assert_ne!(
            after
                .children
                .iter()
                .position(|node| node.name.as_deref() == Some("Change power profile")),
            power_index,
            "the stops in front of the power row grew"
        );
        assert_eq!(key_of(&after, "Change power profile"), power);
        let mut keys: Vec<u64> = after.children.iter().filter_map(|node| node.key).collect();
        assert_eq!(keys.len(), after.children.len(), "every stop is keyed");
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), after.children.len(), "no two stops share a key");
    }

    #[test]
    fn each_monitor_gets_a_named_brightness_slider() {
        use crate::shell::chrome_state::DisplayBrightness;
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::fixture();
        let single_height = root.preferred_height(&state);
        state.displays = vec![
            DisplayBrightness {
                connector: "eDP-1".into(),
                name: "Built-in display".into(),
                level: 0.5,
            },
            DisplayBrightness {
                connector: "DP-2".into(),
                name: "Dell U2720Q".into(),
                level: 0.2,
            },
        ];
        root.sync_displays(&state);
        root.panel = PANEL;
        assert_eq!(root.slider_count(), 3, "two monitors, then volume");
        assert!(
            root.preferred_height(&state) > single_height,
            "the extra slider and its name make room for themselves"
        );
        let names: Vec<String> = (0..3).map(|i| root.slider_name(i)).collect();
        assert_eq!(
            names,
            [
                "Brightness, Built-in display",
                "Brightness, Dell U2720Q",
                "Volume"
            ]
        );
        let faces: Vec<Rect> = (0..3).map(|i| root.slider_face_rect(PANEL, i)).collect();
        assert!(
            faces.windows(2).all(|pair| pair[0].y1 < pair[1].y0),
            "faces never overlap: {faces:?}"
        );
        assert!(
            faces[1].y0 - faces[0].y1 >= root.caption_line_height(),
            "the second monitor's name fits above its face"
        );

        // A press on the second monitor's slider commits that monitor only.
        let (track, _) = root.slider_geometry_for(PANEL, 1);
        let end = Point::new(track.x0 + track.width() * 0.75, faces[1].center().y);
        let _ = root.handle_click(end, PANEL, &mut state);
        let commit = root.finish_drag(&state);
        assert!(
            matches!(commit, Some(SliderCommit::DisplayBrightness(1, pct)) if (pct - 75.0).abs() < 1.0),
            "{commit:?}"
        );
        assert!((state.displays[0].level - 0.5).abs() < f32::EPSILON);

        // Keyboard focus walks the monitors before volume and the controls.
        assert_eq!(
            root.root_stops(&state)[..3],
            [
                RootStop::Slider(0),
                RootStop::Slider(1),
                RootStop::Slider(2)
            ]
        );
        root.keyboard_focus = Some(1);
        assert!(matches!(
            root.adjust_focused_slider(&mut state, 1.0),
            Some(SliderCommit::DisplayBrightness(1, _))
        ));
        root.keyboard_focus = Some(2);
        assert!(matches!(
            root.adjust_focused_slider(&mut state, 1.0),
            Some(SliderCommit::Volume(_))
        ));
        assert_eq!(
            root.semantic_stop_name(RootStop::Slider(1)),
            "quick_settings_brightness_1"
        );
        assert_eq!(
            root.semantic_stop_name(RootStop::Slider(2)),
            "quick_settings_volume"
        );
    }

    #[test]
    fn a_single_monitor_keeps_the_single_unlabelled_slider() {
        use crate::shell::chrome_state::DisplayBrightness;
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::fixture();
        state.displays = vec![DisplayBrightness {
            connector: "DP-1".into(),
            name: "Dell U2720Q".into(),
            level: 0.3,
        }];
        root.sync_displays(&state);
        assert_eq!(root.slider_count(), 2);
        assert_eq!(root.slider_name(0), "Brightness");
        assert!(root.slider_caption_height(0).abs() < f64::EPSILON);
        assert!(
            matches!(root.slider_commit(&state, 0), SliderCommit::DisplayBrightness(0, pct) if (pct - 30.0).abs() < 0.01),
            "a lone external monitor is still driven over DDC/CI"
        );
    }
    #[test]
    fn radio_details_are_reachable_without_changing_a_radio() {
        for (name, level) in [
            ("Wi-Fi networks", QsLevel::Wifi),
            ("Bluetooth devices", QsLevel::Bluetooth),
        ] {
            let mut root = QuickSettingsRoot::default();
            let mut state = QuickSettingsState::fixture();
            state.radios.wifi_available = true;
            state.bluetooth_available = true;
            root.panel = PANEL;
            let before = (state.wifi, state.bluetooth);
            let count = root.accessibility_snapshot(&state).children.len();
            let mut found = false;
            for _ in 0..count {
                root.focus_next(&state, false);
                let tree = root.accessibility_snapshot(&state);
                if tree
                    .children
                    .iter()
                    .any(|node| node.name.as_deref() == Some(name) && node.state.focused)
                {
                    found = true;
                    assert_eq!(
                        root.activate_focused(PANEL, &mut state),
                        RootAction::Enter(level.clone())
                    );
                    assert_eq!((state.wifi, state.bluetooth), before);
                    assert_eq!(
                        root.activate_focused(PANEL, &mut state),
                        RootAction::Enter(level.clone())
                    );
                    break;
                }
            }
            assert!(
                found,
                "{name} must have its own keyboard and accessible focus stop"
            );
        }
    }

    #[test]
    fn radio_details_capture_and_accessibility_use_the_same_target() {
        let mut missing = Vec::new();
        for (variant, theme, width) in [
            ("dark", Theme::dark(), 400.0),
            ("light200", Theme::light().with_text_scale(2.0), 400.0),
            (
                "contrast200",
                Theme::high_contrast().with_text_scale(2.0),
                400.0,
            ),
            ("narrow200", Theme::dark().with_text_scale(2.0), 320.0),
            (
                "combined150-130",
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
                400.0,
            ),
        ] {
            for (name, id) in [
                ("Wi-Fi networks", "network"),
                ("Bluetooth devices", "bluetooth"),
            ] {
                let mut root = QuickSettingsRoot::default();
                let mut state = QuickSettingsState::fixture();
                state.radios.wifi_available = true;
                state.bluetooth_available = true;
                root.set_layout_text_scale(f32v(theme.responsive_scale()));
                root.sync_displays(&state);
                let panel = Rect::new(
                    24.0,
                    24.0,
                    width + 24.0,
                    24.0 + root.preferred_height(&state),
                );
                root.panel = panel;
                let snapshot = root.accessibility_snapshot(&state);
                let index = snapshot
                    .children
                    .iter()
                    .position(|node| node.name.as_deref() == Some(name));
                if index.is_none() {
                    missing.push(format!("{variant}/{id}"));
                }
                // The pre-change capture shows the old body focus; the after
                // capture shows the newly reachable details action.
                let control = if id == "network" {
                    QuickSettingsControl::Wifi
                } else {
                    QuickSettingsControl::Bluetooth
                };
                let fallback = root
                    .root_stops(&state)
                    .iter()
                    .position(
                        |stop| matches!(stop, RootStop::Control(_, current) if *current == control),
                    )
                    .expect("fixture has the radio card");
                assert!(root.focus_accessibility_path(&[index.unwrap_or(fallback)], &state));
                if let Some(directory) = std::env::var_os("TILO_QS_DETAILS_EVIDENCE") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let mut scene = Scene::new();
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        theme.tokens.colors.surfaces.raised,
                        None,
                        &RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.panel)),
                    );
                    root.draw(
                        &mut scene,
                        Affine::IDENTITY,
                        &mut TextRenderer::new(),
                        &theme,
                        panel,
                        None,
                        &state,
                    );
                    ui::offscreen::OffscreenRenderer::new(460, 980).render_png(
                        &scene,
                        theme.tokens.colors.surfaces.base,
                        directory.join(format!("{variant}-{id}.png")),
                    );
                }
            }
        }
        assert!(
            missing.is_empty(),
            "details action absent from the production accessibility tree: {missing:?}"
        );
    }

    #[test]
    fn details_focus_is_inert_when_its_radio_disappears() {
        let mut root = QuickSettingsRoot::default();
        let mut state = QuickSettingsState::fixture();
        state.radios.wifi_available = true;
        state.bluetooth_available = true;
        root.panel = PANEL;
        let index = root
            .accessibility_snapshot(&state)
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Wi-Fi networks"))
            .expect("Wi-Fi details focus stop");
        assert!(root.focus_accessibility_path(&[index], &state));
        state.radios.wifi_available = false;
        let before = (state.wifi, state.bluetooth, state.night_light);
        assert_eq!(root.activate_focused(PANEL, &mut state), RootAction::None);
        assert_eq!((state.wifi, state.bluetooth, state.night_light), before);
    }
}
