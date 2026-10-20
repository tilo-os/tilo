//! The Quick Settings root page: sliders, a grid of two-target tiles, rows
//! and a footer, with each tile's section opening in place under its row.
//!
//! The panel presents current controls, not a dashboard of every capability.
//! Arrangement is durable user intent while hardware availability is applied
//! only when the panel is rendered. Everything is laid out top to bottom in
//! one pass ([`PageLayout`]); paint, hit-testing, keyboard focus and the
//! accessibility tree all read that one layout.

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::i18n::{t, t_args};
use crate::shell::quick_settings::sections::{
    self, SectionLayout, SectionMetrics, SectionPaint, SectionStop,
};
use crate::shell::quick_settings::{PowerProfile, QsLevel, QsSection};
use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use std::collections::HashMap;
use tilo_desktop::drill_in::{
    CONTROL_INSET, CONTROL_PAD, GLYPH, PANEL_PAD, RELATED_GAP, ROW_HEIGHT, SECTION_GAP,
    SIBLING_GAP, TARGET, centred_text_top, control_radius, leading_glyph, paint_control,
    trailing_toggle,
};
use ui::TextRole;
use ui::motion::{Disclose, Extent};
use ui::num::f64_of_usize;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::ButtonRole;
use ui::widget::drag::{DragController, ListMetrics};
use ui::widget::{Button, ButtonFace, Interaction, ListRowFace, RowLeading, Slider, SliderFace};
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

#[derive(Clone, Debug, PartialEq)]
pub enum RootAction {
    None,
    /// Close the panel and open Settings, at a page when it names one.
    OpenSettings(Option<&'static str>),
    /// Lock the screen.
    Lock,
    OpenPowerMenu,
    Enter(QsLevel),
    Changed(PendingRootActions),
    LayoutChanged,
    /// A section opened: the host reads what it lists.
    SectionOpened(QsSection),
    Wifi(WifiIntent),
    Bluetooth(BluetoothIntent),
    /// Turn a VPN profile on or off.
    Vpn {
        name: String,
        enabled: bool,
    },
    /// Play sound through this output (its audio graph node).
    SoundOutput(u32),
    /// A press on a row the host describes (a Devices row, a copy): at
    /// `point` for a pointer, or `None` from the keyboard.
    Hosted {
        section: QsSection,
        index: usize,
        point: Option<Point>,
    },
    /// Clear the clipboard history's unpinned copies.
    ClearClipboard,
}

/// What a press in the Wi-Fi section asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiIntent {
    /// Join a network that needs no password typed: open, or known.
    Join(String),
    /// Ask for a protected network's password, in place.
    AskPassword(String),
    SubmitPassword,
    CancelPassword,
}

/// What a press in the Bluetooth section asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BluetoothIntent {
    Connect(String),
    Disconnect(String),
    /// A connected accessory's use on the person's other devices.
    Details(String),
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

/// A row under the tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootRow {
    /// The battery (or, without one, the power mode): opens its section.
    Battery,
    Devices,
    Clipboard,
    Notifications,
}

/// A button in the panel's footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterButton {
    Settings,
    Lock,
    Power,
}

impl FooterButton {
    const ALL: [Self; 3] = [Self::Settings, Self::Lock, Self::Power];

    fn label(self) -> String {
        t(match self {
            Self::Settings => "qs-tile-settings",
            Self::Lock => "power-lock",
            Self::Power => "qs-tile-power",
        })
    }
    fn description(self) -> String {
        t(match self {
            Self::Settings => "qs-tile-settings-description",
            Self::Lock => "qs-lock-description",
            Self::Power => "qs-tile-power-description",
        })
    }
    const fn semantic_name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Lock => "lock",
            Self::Power => "power",
        }
    }
    const fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Settings => ui::icons::Icon::Settings2,
            Self::Lock => ui::icons::Icon::Lock,
            Self::Power => ui::icons::Icon::Power,
        }
    }
    fn action(self) -> RootAction {
        match self {
            Self::Settings => RootAction::OpenSettings(None),
            Self::Lock => RootAction::Lock,
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
    /// A tile's main area: turns its control on or off.
    Control(usize, QuickSettingsControl),
    /// A tile's chevron: opens or closes its section.
    Expand(usize, QuickSettingsControl),
    /// The volume slider's chevron: opens or closes where sound plays.
    SoundExpand,
    /// A stop inside the open section.
    Section(SectionStop),
    Row(RootRow),
    Edit,
    Footer(FooterButton),
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

/// Where a section is drawn on the page this frame.
#[derive(Clone, Debug, PartialEq)]
struct SectionSlot {
    section: QsSection,
    /// Its full extent, as laid out; only `shown` of it is revealed.
    rect: Rect,
    shown: f64,
    layout: SectionLayout,
}

impl SectionSlot {
    /// The revealed part of the section.
    fn clip(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.shown,
        )
    }

    /// How far it is revealed, `0..=1`.
    fn progress(&self) -> f64 {
        if self.rect.height() <= 0.0 {
            1.0
        } else {
            (self.shown / self.rect.height()).clamp(0.0, 1.0)
        }
    }
}

/// The root page laid out top to bottom.
#[derive(Clone, Debug, Default, PartialEq)]
struct PageLayout {
    sliders: Rect,
    tiles: Vec<(QuickSettingsControl, Rect)>,
    sections: Vec<SectionSlot>,
    rows: Vec<(RootRow, Rect)>,
    footer: Vec<(FooterButton, Rect)>,
    /// Edit controls, at the footer's leading end.
    edit: Rect,
    /// Opened to paste (Super+V): the page is the Clipboard section alone.
    paste: bool,
    /// Where the page ends, its bottom padding included.
    bottom: f64,
}

/// What one frame of the root page is drawn from: the state, the pointer
/// and the stop keyboard focus is on, looked up once.
struct RootFrame<'a> {
    state: &'a QuickSettingsState,
    cursor: Option<Point>,
    focused: Option<RootStop>,
}

/// What each section is anchored under: the row of its tile, or a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    /// Under the sliders (where sound plays).
    Sliders,
    TileRow(usize),
    Row(RootRow),
}

/// Who owns the arrangement this page shows. Hosts that inject a model
/// (tests, screenshots and embedders) must not silently rewrite the desktop
/// user's persisted arrangement: only the normal session constructor owns
/// that file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigOwner {
    Session,
    Injected,
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
    // The volume slider has a chevron for where sound plays: there are
    // outputs to choose from. Synced with the monitors.
    sound: bool,
    keyboard_focus: Option<usize>,
    // The root stop keyboard focus is on. `root_stops` changes while the
    // popover is open (a slider or control appears once its device state
    // arrives), so an index alone would move focus, or Enter, to another
    // stop. Keyboard navigation sets it; the index follows it.
    root_focus: Option<RootStop>,
    config: QuickSettingsConfig,
    config_owner: ConfigOwner,
    edit_mode: bool,
    edit_drag: DragController<QuickSettingsControl>,
    /// How much of each section shows, on the disclosure motion: the open
    /// one growing to its height, a closing one shrinking to nothing.
    reveal: HashMap<QsSection, Extent>,
    /// The section last followed, so a change starts its motion.
    followed: Option<QsSection>,
    /// The motion settings and input source of the last change.
    motion: Option<(EffectiveMotion, MotionSource)>,
    /// The password typed into the inline Wi-Fi prompt can be submitted
    /// (the field itself is the host's secret input).
    password_ready: bool,
}

impl Default for QuickSettingsRoot {
    /// Deterministic for tests. Production construction calls
    /// [`Self::load_persisted`].
    fn default() -> Self {
        Self::with_config(QuickSettingsConfig::default())
    }
}

impl QuickSettingsRoot {
    // One rhythm for the page (HIG §7b, §9.6): the same relationship always
    // gets the same step, and every value is a token expression.
    /// The title band (HIG §9.6: the Bar height); the Edit button sits in it.
    const HEADER: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
    /// The edit-mode footer buttons: the shared `Button` height.
    const EDIT_FOOTER: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
    /// How far the dragged ghost lifts off its slot, and its rail's width
    /// (`s1`).
    const LIFT: f64 = ui::tokens::Spacing::BASE.s1 as f64;
    /// A two-line row or tile (design rows: 52).
    const TWO_LINE: f64 = ui::tokens::Metrics::BASE.row_double as f64;
    /// A single-line row (design rows: 36).
    const ONE_LINE: f64 = ui::tokens::Metrics::BASE.row_single as f64;
    /// A tile's chevron area: the pointer floor wide.
    const EXPAND_WIDTH: f64 = TARGET;

    const PAD: f64 = ui::tokens::Spacing::BASE.s4 as f64;
    const EDIT_ACTION_WIDTH: f64 = 76.0; // Text affordance width, not a spacing step.
    fn edit_help_copy() -> String {
        t("qs-edit-help")
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
            sound: false,
            keyboard_focus: None,
            root_focus: None,
            config: config.normalised(),
            config_owner: if persist_config {
                ConfigOwner::Session
            } else {
                ConfigOwner::Injected
            },
            edit_mode: false,
            edit_drag: DragController::vertical(),
            reveal: HashMap::new(),
            followed: None,
            motion: None,
            password_ready: false,
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

    /// Whether the password typed into the inline Wi-Fi prompt can be
    /// submitted; the host owns the secret field and says so each frame.
    pub fn set_wifi_password_ready(&mut self, ready: bool) {
        self.password_ready = ready;
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
    /// A row or tile with a second line: the design's two-line height, or
    /// taller when the text scale needs it.
    fn two_line_height(&self) -> f64 {
        (Self::TWO_LINE * self.layout_scale).max(
            2.0 * CONTROL_PAD * self.layout_scale
                + self.line_height(TextRole::Label)
                + self.caption_line_height(),
        )
    }
    /// A single-line row.
    fn one_line_height(&self) -> f64 {
        (Self::ONE_LINE * self.layout_scale)
            .max(2.0 * CONTROL_PAD * self.layout_scale + self.line_height(TextRole::Body))
    }
    fn tile_height(&self) -> f64 {
        self.two_line_height()
    }
    /// Edit-mode row: its label on the control padding, never under the
    /// Control height.
    fn edit_row_height_at(&self, layout_scale: f64) -> f64 {
        (ROW_HEIGHT * layout_scale).max(2.0 * CONTROL_PAD * layout_scale + self.label_line_height())
    }
    fn edit_row_height(&self) -> f64 {
        self.edit_row_height_at(self.layout_scale)
    }
    /// A section caption band in edit mode ("Active controls", "Available"):
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

    /// The sizes the sections are laid out with.
    fn section_metrics(&self) -> SectionMetrics {
        SectionMetrics {
            row: self.one_line_height(),
            two_line: self.two_line_height(),
            control: Self::target_at(self.layout_scale),
            caption: self.caption_line_height(),
            title: self.line_height(TextRole::Title),
        }
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

    // ---------- sections and their motion ----------

    /// The section open on the page: the state's, when its control is on
    /// the page.
    fn open_section(&self, state: &QuickSettingsState) -> Option<QsSection> {
        let section = state.section?;
        self.anchor(section, state).map(|_| section)
    }

    /// Where `section` hangs on the page, or `None` when the control it
    /// belongs to is not shown on this machine.
    fn anchor(&self, section: QsSection, state: &QuickSettingsState) -> Option<Anchor> {
        let control = match section {
            QsSection::Sound => return self.sound.then_some(Anchor::Sliders),
            QsSection::Battery | QsSection::Devices | QsSection::Clipboard => {
                let row = match section {
                    QsSection::Battery => RootRow::Battery,
                    QsSection::Devices => RootRow::Devices,
                    _ => RootRow::Clipboard,
                };
                return Self::rows(state).contains(&row).then_some(Anchor::Row(row));
            }
            QsSection::Wifi => QuickSettingsControl::Wifi,
            QsSection::Bluetooth => QuickSettingsControl::Bluetooth,
            QsSection::Vpn => QuickSettingsControl::Vpn,
        };
        let index = self
            .visible_controls(state)
            .iter()
            .position(|visible| *visible == control)?;
        Self::section_of(control, state).map(|_| Anchor::TileRow(index / 2))
    }

    /// The section a tile's chevron opens, if it has one.
    fn section_of(control: QuickSettingsControl, state: &QuickSettingsState) -> Option<QsSection> {
        match control {
            QuickSettingsControl::Wifi if state.radios.wifi_available => Some(QsSection::Wifi),
            QuickSettingsControl::Bluetooth => Some(QsSection::Bluetooth),
            QuickSettingsControl::Vpn => Some(QsSection::Vpn),
            _ => None,
        }
    }

    /// Follow the state's open section on the disclosure motion: the panel
    /// grows by the section as it opens and shrinks as it closes, on the
    /// same curve the host's panel height follows. Call once per frame
    /// before laying out; `now` is the frame's time.
    pub fn follow_sections(
        &mut self,
        state: &QuickSettingsState,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        let open = self.open_section(state);
        if open != self.followed {
            self.followed = open;
            self.motion = Some((motion, source));
        }
        let width = self.section_width();
        let metrics = self.section_metrics();
        for section in QsSection::ALL {
            let target = if open == Some(section) {
                sections::layout(section, state, width, metrics).height + self.scaled(RELATED_GAP)
            } else {
                0.0
            };
            let entry = self.reveal.entry(section).or_insert_with(|| {
                let mut extent = Extent::default();
                extent.snap(0.0);
                extent
            });
            if (entry.target() - target).abs() > f64::EPSILON {
                entry.follow(target, motion, source, now);
            }
        }
    }

    /// Show every section exactly as the state has it, with no motion: a
    /// panel that opens shows its page as it is.
    pub fn settle_sections(&mut self, state: &QuickSettingsState) {
        let open = self.open_section(state);
        self.followed = open;
        let width = self.section_width();
        let metrics = self.section_metrics();
        self.reveal.clear();
        if let Some(section) = open {
            let mut extent = Extent::default();
            extent.snap(
                sections::layout(section, state, width, metrics).height + self.scaled(RELATED_GAP),
            );
            self.reveal.insert(section, extent);
        }
    }

    /// Whether a section is still opening or closing.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.reveal.values().any(|extent| extent.is_moving(now))
    }

    /// How much of `section` shows at `now`: its full height when it is
    /// open and nothing has followed it yet.
    fn shown(&self, section: QsSection, full: f64, open: bool, now: Instant) -> f64 {
        match self.reveal.get(&section) {
            Some(extent) => extent.get(now).clamp(0.0, full.max(extent.target())),
            None if open => full,
            None => 0.0,
        }
    }

    /// The width a section is laid out at.
    fn section_width(&self) -> f64 {
        let width = if self.panel.width() > 0.0 {
            self.panel.width()
        } else {
            f64::from(ui::tokens::Metrics::BASE.panel_width_list)
        };
        (width - 2.0 * self.scaled(PANEL_PAD)).max(1.0)
    }

    // ---------- layout ----------

    /// The rows under the tiles that this machine shows.
    fn rows(state: &QuickSettingsState) -> Vec<RootRow> {
        let mut rows = Vec::new();
        if state.battery.present
            || !state.power_profiles.is_empty()
            || !state.awake_holders.is_empty()
        {
            rows.push(RootRow::Battery);
        }
        rows.extend([RootRow::Devices, RootRow::Clipboard, RootRow::Notifications]);
        rows
    }

    fn row_height(&self, row: RootRow) -> f64 {
        match row {
            RootRow::Battery => self.two_line_height(),
            RootRow::Devices | RootRow::Clipboard | RootRow::Notifications => {
                self.one_line_height()
            }
        }
    }

    /// The page at `now`, laid out top to bottom in `panel`.
    fn page_layout_at(&self, panel: Rect, state: &QuickSettingsState, now: Instant) -> PageLayout {
        let pad = self.scaled(PANEL_PAD);
        let section_gap = self.scaled(SECTION_GAP);
        let sibling_gap = self.scaled(SIBLING_GAP);
        let related_gap = self.scaled(RELATED_GAP);
        let (x0, x1) = (panel.x0 + pad, panel.x1 - pad);
        let open = self.open_section(state);
        let metrics = self.section_metrics();
        let width = (x1 - x0).max(1.0);
        let mut layout = PageLayout::default();
        // A section under its anchor, revealed as far as its motion is.
        let place_sections = |anchor: Anchor, y: &mut f64, layout: &mut PageLayout| {
            for section in QsSection::ALL {
                if self.anchor(section, state) != Some(anchor) {
                    continue;
                }
                let is_open = open == Some(section);
                // A closed section with nothing left of its motion has no
                // room on the page: skip laying it out.
                if !is_open
                    && self
                        .reveal
                        .get(&section)
                        .is_none_or(|extent| extent.get(now) <= 0.0)
                {
                    continue;
                }
                let laid = sections::layout(section, state, width, metrics);
                // The gap above opens with the section: one extent, so the
                // page grows exactly as the panel's height does.
                let total = self.shown(section, laid.height + related_gap, is_open, now);
                if total <= 0.0 {
                    continue;
                }
                let top = *y + related_gap;
                layout.sections.push(SectionSlot {
                    section,
                    rect: Rect::new(x0, top, x1, top + laid.height),
                    shown: (total - related_gap).clamp(0.0, laid.height),
                    layout: laid,
                });
                *y += total;
            }
        };

        if state.pasting() {
            // Opened to paste: the copies alone, at the caret, at once.
            let laid = sections::layout(QsSection::Clipboard, state, width, metrics);
            let top = panel.y0 + pad;
            layout.bottom = top + laid.height + pad;
            layout.sections.push(SectionSlot {
                section: QsSection::Clipboard,
                rect: Rect::new(x0, top, x1, top + laid.height),
                shown: laid.height,
                layout: laid,
            });
            layout.paste = true;
            return layout;
        }

        layout.sliders = self.sliders_block_in(panel);
        let mut y = layout.sliders.y1;
        place_sections(Anchor::Sliders, &mut y, &mut layout);

        let controls = self.visible_controls(state);
        if !controls.is_empty() {
            y += section_gap;
            let tile_h = self.tile_height();
            let tile_w = (width - sibling_gap) / 2.0;
            for (row, pair) in controls.chunks(2).enumerate() {
                if row > 0 {
                    y += sibling_gap;
                }
                for (col, control) in pair.iter().enumerate() {
                    let x = x0 + f64_of_usize(col) * (tile_w + sibling_gap);
                    layout
                        .tiles
                        .push((*control, Rect::new(x, y, x + tile_w, y + tile_h)));
                }
                y += tile_h;
                place_sections(Anchor::TileRow(row), &mut y, &mut layout);
            }
        }

        y += section_gap;
        for (index, row) in Self::rows(state).into_iter().enumerate() {
            if index > 0 {
                y += self.scaled(ui::tokens::Spacing::BASE.s1.into());
            }
            let h = self.row_height(row);
            layout.rows.push((row, Rect::new(x0, y, x1, y + h)));
            y += h;
            place_sections(Anchor::Row(row), &mut y, &mut layout);
        }

        // The footer: icon buttons, Edit controls at the leading end and
        // Settings, Lock and Power at the trailing one. Each is named, and
        // the name is its tooltip.
        y += section_gap;
        let side = Self::target_at(self.layout_scale);
        layout.edit = Rect::new(x0, y, x0 + side, y + side);
        let count = FooterButton::ALL.len();
        for (index, button) in FooterButton::ALL.into_iter().enumerate() {
            let from_end = f64_of_usize(count.saturating_sub(index));
            let x = x1 - from_end * side - (from_end - 1.0) * related_gap;
            layout
                .footer
                .push((button, Rect::new(x, y, x + side, y + side)));
        }
        y += side + pad;
        layout.bottom = y;
        layout
    }

    /// The page as it stands now.
    fn page_layout(&self, state: &QuickSettingsState) -> PageLayout {
        self.page_layout_at(self.panel, state, ui::time::now())
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
        self.sound = state.volume_available && !state.sound_outputs.is_empty();
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
        f64_of_usize(count.saturating_sub(1)) * RELATED_GAP * scale
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
        let row = self.edit_row(panel, 1);
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
                // Airplane mode is only meaningful with a radio.
                QuickSettingsControl::AirplaneMode => {
                    state.radios.wifi_available || state.radios.wwan_available
                }
                QuickSettingsControl::Vpn => !state.vpns.is_empty(),
                QuickSettingsControl::MobileData => state.radios.wwan_available,
                // Only a convertible that can sense its orientation rotates.
                QuickSettingsControl::RotationLock => state.rotation_available,
                // Only where the power daemon offers a saver mode.
                QuickSettingsControl::PowerMode => {
                    state.power_profiles.contains(&PowerProfile::Saver)
                }
            })
            .collect()
    }
    /// Controls usable on this machine right now. Persisted optional controls
    /// are deliberately not shown as addable when the backing capability is
    /// absent; if hardware appears later the saved preference remains intact.
    fn edit_controls(&self, state: &QuickSettingsState) -> Vec<QuickSettingsControl> {
        let mut all = self.config.controls.clone();
        for c in QuickSettingsControl::ALL {
            if !all.contains(&c) {
                all.push(c);
            }
        }
        let visible_with = |c: QuickSettingsControl| {
            let mut config = self.config.clone();
            if !config.controls.contains(&c) {
                config.controls.push(c);
            }
            Self::with_config(config)
                .visible_controls(state)
                .contains(&c)
        };
        all.into_iter().filter(|c| visible_with(*c)).collect()
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
    /// The panel height the page wants: the root page with its open section
    /// at full height — the extent the host's panel height follows on the
    /// disclosure motion — or the edit page.
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
            let panel = Rect::new(
                0.0,
                0.0,
                f64::from(ui::tokens::Metrics::BASE.panel_width_list),
                0.0,
            );
            let at_rest = self.page_layout_at_rest(panel, state);
            at_rest.bottom - panel.y0
        }
    }

    /// The page with every section where its motion ends: the open one at
    /// its full height, the others closed.
    fn page_layout_at_rest(&self, panel: Rect, state: &QuickSettingsState) -> PageLayout {
        let mut settled = Self::with_config(self.config.clone());
        settled.layout_scale = self.layout_scale;
        settled.text_scale = self.text_scale;
        settled.display_names.clone_from(&self.display_names);
        settled.sound = self.sound;
        settled.panel = if self.panel.width() > 0.0 {
            Rect::new(self.panel.x0, panel.y0, self.panel.x1, panel.y0)
        } else {
            panel
        };
        settled.settle_sections(state);
        settled.page_layout_at(settled.panel, state, ui::time::now())
    }
    fn save_config(&mut self) {
        self.config = self.config.clone().normalised();
        if self.config_owner == ConfigOwner::Injected {
            // Interaction tests and embedded previews exercise the same
            // model, but never write a developer's real desktop settings.
            // Their caller owns persistence, if any.
            return;
        }
        if let Err(error) = self.config.save() {
            tracing::warn!(%error, "could not persist Quick Settings layout");
        }
    }
    /// The slider block, first on the page: the panel has no title, so
    /// it starts at the panel padding and stays put in every state, since
    /// everything that changes is below it.
    fn sliders_block_in(&self, panel: Rect) -> Rect {
        let pad = self.scaled(PANEL_PAD);
        let y = panel.y0 + pad;
        Rect::new(
            panel.x0 + pad,
            y,
            panel.x1 - pad,
            y + self.sliders_height_at(self.layout_scale),
        )
    }
    fn sliders_block(&self) -> Rect {
        self.sliders_block_in(self.panel)
    }
    fn edit_grid_top(&self, panel: Rect) -> f64 {
        self.header_bottom(panel) + 2.0 * self.caption_band()
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
    /// Top of the "Available" section: one section gap under the last row.
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
    /// The add-chips under the "Available" caption: pointer-floor tall,
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

    /// Edit-mode row `i`: Control-height rows one related step apart on
    /// the panel padding.
    fn edit_row(&self, panel: Rect, i: usize) -> Rect {
        let row_h = self.edit_row_height();
        let y = self.edit_grid_top(panel) + f64_of_usize(i) * (row_h + self.scaled(RELATED_GAP));
        Rect::new(
            panel.x0 + self.scaled(PANEL_PAD),
            y,
            panel.x1 - self.scaled(PANEL_PAD),
            y + row_h,
        )
    }
    fn edit_action_rect(&self, row: Rect) -> Rect {
        Rect::new(
            row.x1 - self.scaled(Self::EDIT_ACTION_WIDTH + 6.0),
            row.y0 + self.scaled(7.0),
            row.x1 - self.scaled(6.0),
            row.y1 - self.scaled(7.0),
        )
    }

    /// A tile's chevron area: its trailing pointer-floor-wide strip.
    fn expand_rect(&self, tile: Rect) -> Rect {
        Rect::new(
            (tile.x1 - self.scaled(Self::EXPAND_WIDTH)).max(tile.x0),
            tile.y0,
            tile.x1,
            tile.y1,
        )
    }
    /// Slider `i`'s face: after the leading glyph slot, the rest of the
    /// block's width, at least one minimum pointer target tall (HIG §9.6). The
    /// faces sit one related step apart, the first on the block's top edge
    /// (the panel padding above it). One helper feeds
    /// paint, hit-testing and the accessibility tree.
    fn slider_face_rect(&self, block: Rect, i: usize) -> Rect {
        let above: f64 = (0..i).map(|j| self.slider_caption_height(j)).sum();
        let y = block.y0
            + f64_of_usize(i) * (Self::target_at(self.layout_scale) + self.scaled(RELATED_GAP))
            + above
            + self.slider_caption_height(i);
        // The volume slider leaves room for its chevron.
        let trailing = if self.sound && i == self.volume_slider() {
            self.scaled(Self::EXPAND_WIDTH)
        } else {
            self.scaled(CONTROL_INSET)
        };
        Rect::new(
            block.x0 + self.scaled(CONTROL_INSET + GLYPH + CONTROL_INSET),
            y,
            block.x1 - trailing,
            y + Self::target_at(self.layout_scale),
        )
    }

    /// The volume slider's chevron, on its band's trailing edge.
    fn volume_expand_rect(&self, block: Rect) -> Rect {
        let face = self.slider_face_rect(block, self.volume_slider());
        Rect::new(face.x1, face.y0, block.x1, face.y1)
    }
    /// Slider `i`'s painted track — the shared face's own `track_rect`, so
    /// value-from-pointer maths and paint agree — and the top of its band.
    fn slider_geometry_for(&self, block: Rect, i: usize) -> (Rect, f64) {
        let face = self.slider_face_rect(block, i);
        (SliderFace::new(face, 0.0).track_rect(), face.y0)
    }
    /// Slider `i`'s geometry in a popover `panel`, for integration tests.
    pub fn slider_geometry(panel: Rect, i: usize) -> (Rect, f64) {
        let root = Self {
            panel,
            ..Self::default()
        };
        root.slider_geometry_for(root.sliders_block(), i)
    }

    /// Every root focus stop, in keyboard (and visual) order: the sliders,
    /// each tile's switch and chevron with its open section's stops after
    /// its row, the rows (the battery's section after it), Edit, then the
    /// footer. Controls come first so the first Tab after opening reaches
    /// Brightness.
    fn root_stops(&self, state: &QuickSettingsState) -> Vec<RootStop> {
        let layout = self.page_layout(state);
        self.stops_in(&layout, state)
    }

    fn stops_in(&self, layout: &PageLayout, state: &QuickSettingsState) -> Vec<RootStop> {
        if layout.paste {
            return layout
                .sections
                .iter()
                .flat_map(|slot| sections::stops(&slot.layout, state, self.section_metrics()))
                .map(|(stop, _)| RootStop::Section(stop))
                .collect();
        }
        let mut stops: Vec<RootStop> = (0..self.slider_count())
            .filter(|&index| self.slider_available(state, index))
            .map(RootStop::Slider)
            .collect();
        let open = self.open_section(state);
        if self.sound {
            stops.push(RootStop::SoundExpand);
        }
        let section_stops = |anchor: Anchor, stops: &mut Vec<RootStop>| {
            if let Some(slot) = layout.sections.iter().find(|slot| {
                Some(slot.section) == open && self.anchor(slot.section, state) == Some(anchor)
            }) {
                stops.extend(
                    sections::stops(&slot.layout, state, self.section_metrics())
                        .into_iter()
                        .map(|(stop, _)| RootStop::Section(stop)),
                );
            }
        };
        section_stops(Anchor::Sliders, &mut stops);
        for (index, (control, _)) in layout.tiles.iter().enumerate() {
            stops.push(RootStop::Control(index, *control));
            if Self::section_of(*control, state).is_some() {
                stops.push(RootStop::Expand(index, *control));
            }
            let row_done = index % 2 == 1 || index.saturating_add(1) == layout.tiles.len();
            if row_done {
                section_stops(Anchor::TileRow(index / 2), &mut stops);
            }
        }
        for (row, _) in &layout.rows {
            stops.push(RootStop::Row(*row));
            section_stops(Anchor::Row(*row), &mut stops);
        }
        stops.push(RootStop::Edit);
        stops.extend(FooterButton::ALL.map(RootStop::Footer));
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
    /// The rect a stop paints and publishes to accessibility.
    fn stop_rect(&self, stop: RootStop, state: &QuickSettingsState) -> Rect {
        let layout = self.page_layout(state);
        self.stop_rect_in(&layout, stop, state)
    }
    fn stop_rect_in(
        &self,
        layout: &PageLayout,
        stop: RootStop,
        state: &QuickSettingsState,
    ) -> Rect {
        match stop {
            RootStop::Slider(index) => self.slider_face_rect(layout.sliders, index),
            RootStop::Control(index, _) => {
                layout.tiles.get(index).map_or(Rect::ZERO, |(control, r)| {
                    // The switch is the tile less its chevron, when it
                    // has one.
                    if Self::section_of(*control, state).is_some() {
                        Rect::new(r.x0, r.y0, self.expand_rect(*r).x0, r.y1)
                    } else {
                        *r
                    }
                })
            }
            RootStop::Expand(index, _) => layout
                .tiles
                .get(index)
                .map_or(Rect::ZERO, |(_, r)| self.expand_rect(*r)),
            RootStop::SoundExpand => self.volume_expand_rect(layout.sliders),
            RootStop::Section(section_stop) => self
                .open_slot(layout, state)
                .and_then(|slot| {
                    sections::stops(&slot.layout, state, self.section_metrics())
                        .into_iter()
                        .find(|(s, _)| *s == section_stop)
                        .map(|(_, r)| on_page(slot.rect, r))
                })
                .unwrap_or(Rect::ZERO),
            RootStop::Row(row) => layout
                .rows
                .iter()
                .find(|(r, _)| *r == row)
                .map_or(Rect::ZERO, |(_, rect)| *rect),
            RootStop::Edit => layout.edit,
            RootStop::Footer(button) => layout
                .footer
                .iter()
                .find(|(b, _)| *b == button)
                .map_or(Rect::ZERO, |(_, rect)| *rect),
        }
    }
    /// The open section's slot on `layout`, when it shows.
    fn open_slot<'a>(
        &self,
        layout: &'a PageLayout,
        state: &QuickSettingsState,
    ) -> Option<&'a SectionSlot> {
        let open = self.open_section(state)?;
        layout.sections.iter().find(|slot| slot.section == open)
    }
    /// The pointer target of a stop.
    fn stop_hit_rect(&self, stop: RootStop, state: &QuickSettingsState) -> Rect {
        self.stop_rect(stop, state)
    }
    fn semantic_stop_name(&self, stop: RootStop, state: &QuickSettingsState) -> String {
        match stop {
            RootStop::Slider(i) if i == self.volume_slider() => "quick_settings_volume".into(),
            RootStop::Slider(0) => "quick_settings_brightness".into(),
            RootStop::Slider(i) => format!("quick_settings_brightness_{i}"),
            RootStop::Control(_, control) => {
                format!("quick_settings_{}", Self::semantic_control_name(control))
            }
            RootStop::Expand(_, control) => {
                format!(
                    "quick_settings_{}_details",
                    Self::semantic_control_name(control)
                )
            }
            RootStop::SoundExpand => "quick_settings_volume_details".into(),
            RootStop::Section(stop) => format!("quick_settings_{}", section_stop_name(stop, state)),
            RootStop::Row(row) => format!(
                "quick_settings_{}",
                match row {
                    RootRow::Battery => "battery",
                    RootRow::Devices => "devices",
                    RootRow::Clipboard => "clipboard",
                    RootRow::Notifications => "notifications",
                }
            ),
            RootStop::Edit => "quick_settings_edit".into(),
            RootStop::Footer(button) => format!("quick_settings_{}", button.semantic_name()),
        }
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
                        self.edit_row(self.panel, index),
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
        self.root_stops(state)
            .into_iter()
            .map(|stop| {
                (
                    self.semantic_stop_name(stop, state),
                    self.stop_hit_rect(stop, state),
                )
            })
            .collect()
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
            QuickSettingsControl::PowerMode => "power_mode",
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
            RootStop::Expand(_, control) => match Self::section_of(control, state) {
                Some(section) => Self::toggle_section(section, state),
                None => RootAction::None,
            },
            RootStop::SoundExpand => Self::toggle_section(QsSection::Sound, state),
            RootStop::Section(stop) => Self::activate_section_stop(stop, state),
            RootStop::Row(RootRow::Battery) => Self::toggle_section(QsSection::Battery, state),
            RootStop::Row(RootRow::Devices) => Self::toggle_section(QsSection::Devices, state),
            RootStop::Row(RootRow::Clipboard) => Self::toggle_section(QsSection::Clipboard, state),
            RootStop::Row(RootRow::Notifications) => RootAction::Enter(QsLevel::Notifications),
            RootStop::Edit => {
                self.edit_mode = true;
                self.keyboard_focus = None;
                self.root_focus = None;
                RootAction::LayoutChanged
            }
            RootStop::Footer(button) => button.action(),
        }
    }

    /// Open `section`, or close it when it is the one open.
    fn toggle_section(section: QsSection, state: &mut QuickSettingsState) -> RootAction {
        state.toggle_section(section);
        if state.section == Some(section) {
            RootAction::SectionOpened(section)
        } else {
            RootAction::LayoutChanged
        }
    }

    /// Close the open section, as Esc does. Whether one was open.
    pub fn close_section(&mut self, state: &mut QuickSettingsState) -> bool {
        if state.section.take().is_none() {
            return false;
        }
        // Focus inside the section has nowhere to stay.
        if matches!(self.root_focus, Some(RootStop::Section(_))) {
            self.root_focus = None;
            self.keyboard_focus = None;
        }
        true
    }

    fn activate_section_stop(stop: SectionStop, state: &mut QuickSettingsState) -> RootAction {
        match stop {
            SectionStop::TurnOn => {
                let mut pending = PendingRootActions::default();
                match state.section {
                    Some(QsSection::Bluetooth) => {
                        state.bluetooth = true;
                        pending.bluetooth = Some(true);
                    }
                    _ if state.radio_change_pending => return RootAction::None,
                    _ => pending.wifi = Some(true),
                }
                RootAction::Changed(pending)
            }
            SectionStop::Network(index) => {
                let Some(network) = state.wifi_networks.get(index) else {
                    return RootAction::None;
                };
                if network.connected || state.wifi_joining.is_some() {
                    RootAction::None
                } else if network.secure && !network.known {
                    RootAction::Wifi(WifiIntent::AskPassword(network.ssid.clone()))
                } else {
                    RootAction::Wifi(WifiIntent::Join(network.ssid.clone()))
                }
            }
            SectionStop::PasswordCancel => RootAction::Wifi(WifiIntent::CancelPassword),
            SectionStop::PasswordConnect => RootAction::Wifi(WifiIntent::SubmitPassword),
            SectionStop::Settings => RootAction::OpenSettings(Some(match state.section {
                Some(QsSection::Bluetooth) => "settings://bluetooth",
                Some(QsSection::Battery) => "settings://power",
                Some(QsSection::Sound) => "settings://sound",
                Some(QsSection::Devices) => "settings://devices_sync",
                _ => "settings://network",
            })),
            SectionStop::Hosted(index) => match state.section {
                Some(section) => RootAction::Hosted {
                    section,
                    index,
                    point: None,
                },
                None => RootAction::None,
            },
            SectionStop::ClearClipboard => RootAction::ClearClipboard,
            SectionStop::Device(index) => {
                state
                    .bt_devices
                    .get(index)
                    .map_or(RootAction::None, |device| {
                        RootAction::Bluetooth(if device.connected {
                            BluetoothIntent::Disconnect(device.address.clone())
                        } else {
                            BluetoothIntent::Connect(device.address.clone())
                        })
                    })
            }
            SectionStop::DeviceDetails(index) => state
                .bt_devices
                .get(index)
                .map_or(RootAction::None, |device| {
                    RootAction::Bluetooth(BluetoothIntent::Details(device.address.clone()))
                }),
            SectionStop::Profile(index) => {
                state
                    .vpns
                    .get(index)
                    .map_or(RootAction::None, |profile| RootAction::Vpn {
                        name: profile.name.clone(),
                        enabled: !profile.active,
                    })
            }
            SectionStop::Output(index) => state
                .sound_outputs
                .get(index)
                .filter(|output| !output.current)
                .map_or(RootAction::None, |output| {
                    RootAction::SoundOutput(output.node)
                }),
            SectionStop::Mode(mode) => {
                if state.power_profile == mode {
                    return RootAction::None;
                }
                state.power_profile = mode;
                RootAction::Changed(PendingRootActions {
                    power: Some(mode),
                    ..Default::default()
                })
            }
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
    fn notifications_subline(state: &QuickSettingsState) -> String {
        match state.notification_count {
            0 => t("qs-notifications-none"),
            count => t_args("qs-notifications-count", &[("count", count.into())]),
        }
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
            QuickSettingsControl::PowerMode => "qs-power-mode",
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
            QuickSettingsControl::Wifi if !s.wifi => ui::icons::Icon::WifiOff,
            QuickSettingsControl::Wifi => ui::icons::Icon::Wifi,
            QuickSettingsControl::Bluetooth => ui::icons::Icon::Bluetooth,
            QuickSettingsControl::Vpn => ui::icons::Icon::Shield,
            QuickSettingsControl::NightLight => ui::icons::Icon::Sun,
            QuickSettingsControl::AirplaneMode => ui::icons::Icon::Plane,
            QuickSettingsControl::MobileData => ui::icons::Icon::Globe,
            QuickSettingsControl::DoNotDisturb => ui::icons::Icon::Moon,
            QuickSettingsControl::RotationLock => ui::icons::Icon::RotateCw,
            QuickSettingsControl::PowerMode => ui::icons::Icon::Gauge,
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
            QuickSettingsControl::PowerMode => s.power_profile == PowerProfile::Saver,
        }
    }
    /// The network Wi-Fi is on: the observed link when it is Wi-Fi or
    /// offline, else (wired, or not observed yet) the last scan's.
    fn wifi_subline(s: &QuickSettingsState) -> String {
        match &s.link {
            Some(link) if link.kind == os_actions::LinkKind::Wifi => link
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| t("qs-connected")),
            Some(link) if link.kind == os_actions::LinkKind::Offline => t("qs-not-connected"),
            _ => s
                .wifi_networks
                .iter()
                .find(|v| v.connected)
                .map_or_else(|| t("qs-not-connected"), |v| v.ssid.clone()),
        }
    }
    pub(crate) fn subline(c: QuickSettingsControl, s: &QuickSettingsState) -> String {
        if s.radio_change_pending
            && matches!(
                c,
                QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
            )
        {
            // The change under way, until the machine answers: never the
            // request shown as if it had happened.
            return match (c, s.radio_change) {
                (QuickSettingsControl::Wifi, Some(os_actions::RadioChange::Wifi(on)))
                | (
                    QuickSettingsControl::AirplaneMode,
                    Some(os_actions::RadioChange::Airplane(on)),
                ) => t(if on {
                    "qs-turning-on"
                } else {
                    "qs-turning-off"
                }),
                _ => t("display-confirm-applying"),
            };
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
            QuickSettingsControl::Wifi if s.wifi => Self::wifi_subline(s),
            QuickSettingsControl::Wifi
                if s.radios.wifi_hardware_blocked || s.wifi_problem.is_some() =>
            {
                sections::wifi_off_message(s)
            }
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
                .map_or_else(|| t("qs-off"), |v| v.name.clone()),
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
            QuickSettingsControl::PowerMode => s.power_profile.label(),
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
            // On: the active profiles go off. Off: the first one comes on;
            // the section lists every profile with its own switch.
            QuickSettingsControl::Vpn => {
                let profile = s
                    .vpns
                    .iter()
                    .find(|profile| profile.active)
                    .or_else(|| s.vpns.first())?;
                return Some(RootAction::Vpn {
                    name: profile.name.clone(),
                    enabled: !profile.active,
                });
            }
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
            QuickSettingsControl::PowerMode => return Some(Self::toggle_power_saver(s)),
        }
        None
    }

    /// Lay the page out in `panel` with every section where its motion
    /// ends, as a frame would once it settled.
    #[cfg(test)]
    pub(crate) fn lay_out_settled_for_tests(&mut self, panel: Rect, state: &QuickSettingsState) {
        self.settle_sections(state);
        self.lay_out(panel, state);
    }

    /// Whether a stop holds keyboard focus.
    #[must_use]
    pub fn has_keyboard_focus(&self) -> bool {
        self.root_focus.is_some() || self.keyboard_focus.is_some()
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

    /// The other power mode the Power mode tile switches to: the saver, or
    /// back to balanced from it.
    fn toggle_power_saver(s: &mut QuickSettingsState) -> RootAction {
        let next = if s.power_profile == PowerProfile::Saver {
            PowerProfile::Balanced
        } else {
            PowerProfile::Saver
        };
        if !s.power_profiles.contains(&next) {
            return RootAction::None;
        }
        s.power_profile = next;
        RootAction::Changed(PendingRootActions {
            power: Some(next),
            ..Default::default()
        })
    }

    pub fn handle_click(
        &mut self,
        point: Point,
        panel: Rect,
        state: &mut QuickSettingsState,
    ) -> RootAction {
        self.lay_out(panel, state);
        if self.edit_mode {
            return self.edit_click(point, state);
        }
        if self.scrolls() && !self.visible_rect().contains(point) {
            return RootAction::None;
        }
        let layout = self.page_layout(state);
        let sliders = if layout.paste { 0 } else { self.slider_count() };
        for i in 0..sliders {
            let (bar, _) = self.slider_geometry_for(layout.sliders, i);
            // The pointer target is the painted face — the same rect the
            // accessibility tree publishes — not a second band around the track.
            if self.slider_face_rect(layout.sliders, i).contains(point) {
                let value = f32v(((point.x - bar.x0) / bar.width()).clamp(0., 1.));
                self.set_slider_value(state, i, value);
                self.slider_drag = Some(i);
                return RootAction::Changed(PendingRootActions::default());
            }
        }
        // A section's stops are hit where they are drawn, inside its clip;
        // a connected accessory's details button wins over its row.
        if let Some(slot) = self.open_slot(&layout, state)
            && slot.clip().contains(point)
        {
            let local = Point::new(point.x - slot.rect.x0, point.y - slot.rect.y0);
            return sections::stop_at(&slot.layout, state, self.section_metrics(), local).map_or(
                RootAction::None,
                |stop| match stop {
                    // The host knows which of the row's buttons is where.
                    SectionStop::Hosted(index) => RootAction::Hosted {
                        section: slot.section,
                        index,
                        point: Some(point),
                    },
                    _ => Self::activate_section_stop(stop, state),
                },
            );
        }
        let stops = self.stops_in(&layout, state);
        // A tile's chevron is inside its tile: test it first.
        let hit = stops
            .iter()
            .filter(|stop| !matches!(stop, RootStop::Section(_)))
            .find(|stop| {
                matches!(stop, RootStop::Expand(..))
                    && self.stop_rect_in(&layout, **stop, state).contains(point)
            })
            .or_else(|| {
                stops.iter().find(|stop| {
                    !matches!(stop, RootStop::Section(_))
                        && match stop {
                            RootStop::Edit => self.stop_hit_rect(**stop, state),
                            _ => self.stop_rect_in(&layout, **stop, state),
                        }
                        .contains(point)
                })
            })
            .copied();
        match hit {
            Some(stop) => self.activate_stop(stop, state),
            None => RootAction::None,
        }
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
            let row = self.edit_row(self.panel, i);
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

    /// The inline Wi-Fi password field, where the host draws its secret
    /// input: inside the open Wi-Fi section, while it is revealed.
    /// Where row `index` of the host-described rows of `section` is on the
    /// page, while that section is the open one. The host hit-tests the
    /// row's buttons against it with [`super::hosted`]'s geometry.
    pub fn hosted_row_rect(
        &self,
        state: &QuickSettingsState,
        section: QsSection,
        index: usize,
    ) -> Option<Rect> {
        if self.edit_mode {
            return None;
        }
        let layout = self.page_layout(state);
        let slot = self.open_slot(&layout, state)?;
        if slot.section != section {
            return None;
        }
        slot.layout
            .items
            .iter()
            .find(|item| item.kind == sections::ItemKind::Hosted(index))
            .map(|item| on_page(slot.rect, item.rect))
    }

    pub fn wifi_password_field_rect(&self, state: &QuickSettingsState) -> Option<Rect> {
        if self.edit_mode || state.wifi_password.is_none() {
            return None;
        }
        let layout = self.page_layout(state);
        let slot = self.open_slot(&layout, state)?;
        if slot.section != QsSection::Wifi || slot.progress() < 1.0 {
            return None;
        }
        let item = slot
            .layout
            .items
            .iter()
            .find(|item| item.kind == sections::ItemKind::Password)?;
        Some(on_page(
            slot.rect,
            sections::password_field(item.rect, self.section_metrics()),
        ))
    }

    /// The inline prompt's Cancel and Connect, while it shows.
    pub fn wifi_password_button_rects(&self, state: &QuickSettingsState) -> Option<(Rect, Rect)> {
        self.wifi_password_field_rect(state)?;
        Some((
            self.stop_rect(RootStop::Section(SectionStop::PasswordCancel), state),
            self.stop_rect(RootStop::Section(SectionStop::PasswordConnect), state),
        ))
    }

    /// What the page shows of itself: the popover, less the bar lane while
    /// it scrolls. The host clips the inline password field to it.
    pub fn shown_rect(&self) -> Rect {
        self.visible_rect()
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
        // Panel role (HIG §9.1/§9.2): the host paints the one Panel surface;
        // the root paints nothing under its controls.
        if scrolls {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.visible_rect(),
            );
        }
        if self.edit_mode {
            // Only the edit page has a title; the page itself starts with
            // its sliders.
            self.header(scene, transform, text, theme);
            self.draw_edit(scene, transform, text, theme, cursor, state);
        } else {
            self.draw_root(
                &mut PaintCtx::new(scene, transform, text, theme),
                cursor,
                state,
            );
        }
        if scrolls {
            scene.pop_layer();
            self.scroll.paint_bars(scene, transform, theme);
        }
    }
    fn header(&self, scene: &mut Scene, transform: Affine, text: &mut TextRenderer, theme: &Theme) {
        let colors = &theme.tokens.colors;
        // The edit page's title (HIG §9.6): `scale.base` semibold, centred
        // on the header band by its measured height.
        let title = t("qs-edit-controls");
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
    }
    fn draw_root(&self, ctx: &mut PaintCtx<'_>, cursor: Option<Point>, state: &QuickSettingsState) {
        // One layout and one focus lookup for the whole frame.
        let layout = self.page_layout(state);
        let stops = self.stops_in(&layout, state);
        let frame = RootFrame {
            state,
            cursor,
            focused: self
                .root_focus_index(&stops)
                .and_then(|index| stops.get(index).copied()),
        };
        if layout.paste {
            for slot in &layout.sections {
                self.draw_section(ctx, &frame, slot);
            }
            return;
        }
        self.draw_sliders(ctx, &layout, &frame);
        for (index, (control, rect)) in layout.tiles.iter().enumerate() {
            self.draw_tile(ctx, &frame, (index, *control, *rect));
        }
        for slot in &layout.sections {
            self.draw_section(ctx, &frame, slot);
        }
        for (row, rect) in &layout.rows {
            self.draw_row(ctx, &frame, *row, *rect);
        }
        Self::draw_icon_button(
            ctx,
            &frame,
            RootStop::Edit,
            ui::icons::Icon::Pencil,
            &t("qs-edit-controls"),
            layout.edit,
        );
        for (button, rect) in &layout.footer {
            Self::draw_footer_button(ctx, &frame, *button, *rect);
        }
    }

    /// The slider block: a band per slider straight on the panel, each with
    /// its glyph on the leading inset, in the tiles' glyph column. No Control
    /// surface of its own: the panel is already the surface, and a card in it
    /// read as a card in a card. The list rows below sit on the panel too.
    fn draw_sliders(&self, ctx: &mut PaintCtx<'_>, layout: &PageLayout, frame: &RootFrame<'_>) {
        let (state, cursor) = (frame.state, frame.cursor);
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        let glyph = Self::glyph_at(self.layout_scale);
        let inset = self.scaled(CONTROL_INSET);
        let sliders = layout.sliders;
        // The tracks sit on the panel ground, so they are inked for that
        // ground (as the video player's controls on their floating panel
        // are), and their unfilled well is the tone an off switch's track
        // takes on it: one lift off the panel, visible on every material
        // now that no card sits under it.
        let ground = ui::chrome::shell::panel_ground(theme);
        let mut on_panel = theme.on_ground(ground);
        on_panel.tokens.colors.surfaces.raised = on_panel.tokens.colors.lift_on(ground, 1.0);
        let caption_style = theme.text(TextRole::Caption);
        for i in 0..self.slider_count() {
            let face = self.slider_face_rect(sliders, i);
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
                &on_panel,
                &SliderFace::new(face, f64::from(self.slider_value(state, i)))
                    .hovered(available && cursor.is_some_and(|p| face.contains(p)))
                    .focused(frame.focused == Some(RootStop::Slider(i)))
                    .dragging(self.slider_drag == Some(i))
                    .disabled(!available),
            );
        }
        if self.sound {
            let expand = self.volume_expand_rect(sliders);
            let turn = self.chevron_turn(QsSection::Sound, state);
            let chevron = Rect::from_center_size(expand.center(), (glyph, glyph));
            if cursor.is_some_and(|p| expand.contains(p)) {
                // The chevron's own target lifts under the pointer, as a
                // tile's half does.
                ui::widget::primitives::QuickTileFace {
                    rect: expand,
                    main: expand,
                    expand: None,
                    on: false,
                    hovered: Some(expand),
                    radius: control_radius(theme, expand),
                }
                .paint_ground(
                    scene,
                    transform,
                    theme,
                    tilo_desktop::drill_in::control_fill(theme, true),
                );
            }
            ui::icons::draw_icon(
                scene,
                transform.pre_rotate_about(std::f64::consts::FRAC_PI_2 * turn, chevron.center()),
                chevron,
                ui::icons::Icon::ChevronRight,
                colors.text_dim,
                1.5,
            );
            if frame.focused == Some(RootStop::SoundExpand) {
                ui::widget::paint_focus_ring(
                    scene,
                    transform,
                    expand,
                    control_radius(theme, expand),
                    theme,
                );
            }
        }
    }

    /// A tile: the on colour when its control is on, its glyph, name and
    /// state line, and — when it has a section — a chevron area of its own
    /// that turns as the section opens.
    #[allow(
        clippy::too_many_lines,
        reason = "a tile's two targets, their hover, the chevron's turn and both focus rings paint in one ordered pass over one geometry"
    )]
    fn draw_tile(
        &self,
        ctx: &mut PaintCtx<'_>,
        frame: &RootFrame<'_>,
        (index, c, r): (usize, QuickSettingsControl, Rect),
    ) {
        let (s, cursor) = (frame.state, frame.cursor);
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let on = Self::on(c, s);
        let section = Self::section_of(c, s);
        let expand = section.map(|_| self.expand_rect(r));
        let main = expand.map_or(r, |e| Rect::new(r.x0, r.y0, e.x0, r.y1));
        let over = |rect: Rect| cursor.is_some_and(|p| rect.contains(p));
        let main_hovered = over(main) && !expand.is_some_and(over);
        let radius = control_radius(theme, r);
        let hovered = if main_hovered {
            Some(main)
        } else {
            expand.filter(|e| over(*e))
        };
        let face = ui::widget::primitives::QuickTileFace {
            rect: r,
            main,
            expand,
            on,
            hovered,
            radius,
        };
        if !on {
            paint_control(scene, transform, theme, r, false, false);
        }
        // A control that is on is filled with the on colour: the whole tile
        // says it at a glance. Hover lifts the half under the pointer.
        face.paint_ground(
            scene,
            transform,
            theme,
            tilo_desktop::drill_in::control_fill(theme, true),
        );
        let (ink, ink_dim) = face.ink(theme);
        let inset = self.scaled(CONTROL_INSET);
        let glyph = Self::glyph_at(self.layout_scale);
        let icon = leading_glyph(main, inset, glyph);
        ui::icons::draw_icon(scene, transform, icon, Self::icon(c, s), ink, 1.5);
        let text_x = icon.x1 + self.scaled(RELATED_GAP);
        let text_width =
            f32v((main.x1 - self.scaled(ui::tokens::Spacing::BASE.s1.into()) - text_x).max(1.0));
        let label_style = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let sub_style = theme.text(TextRole::Caption);
        let label =
            text.elide_to_width_style(&Self::rendered_label(c, s), text_width, &label_style);
        let sub = text.elide_to_width_style(&Self::subline(c, s), text_width, &sub_style);
        let (_, label_h) = text.measure_style(&label, None, &label_style);
        let (_, sub_h) = text.measure_style(&sub, None, &sub_style);
        let top = centred_text_top(main, f64::from(label_h) + f64::from(sub_h));
        text.draw_style(
            scene,
            &label,
            ink,
            transform.pre_translate((text_x, top).into()),
            None,
            &label_style,
        );
        text.draw_style(
            scene,
            &sub,
            ink_dim,
            transform.pre_translate((text_x, top + f64::from(label_h)).into()),
            None,
            &sub_style,
        );
        if let (Some(expand), Some(section)) = (expand, section) {
            // A hairline sets the chevron's area apart from the switch.
            let inset_y =
                self.scaled(CONTROL_PAD) + self.scaled(ui::tokens::Spacing::BASE.s1.into());
            face.paint_divider(scene, transform, theme, inset_y);
            let turn = self.chevron_turn(section, s);
            let chevron = Rect::from_center_size(expand.center(), (glyph, glyph));
            ui::icons::draw_icon(
                scene,
                transform.pre_rotate_about(std::f64::consts::FRAC_PI_2 * turn, chevron.center()),
                chevron,
                ui::icons::Icon::ChevronRight,
                ink,
                1.5,
            );
        }
        let radius = control_radius(theme, r);
        if frame.focused == Some(RootStop::Control(index, c)) {
            ui::widget::paint_focus_ring(scene, transform, main, radius, theme);
        }
        if let Some(expand) = expand
            && frame.focused == Some(RootStop::Expand(index, c))
        {
            ui::widget::paint_focus_ring(scene, transform, expand, radius, theme);
        }
    }

    /// How far a section's chevron has turned: a quarter turn, from
    /// pointing at the section's place to pointing into it, with its reveal.
    fn chevron_turn(&self, section: QsSection, state: &QuickSettingsState) -> f64 {
        let open = self.open_section(state) == Some(section);
        let Some(extent) = self.reveal.get(&section) else {
            return if open { 1.0 } else { 0.0 };
        };
        let full = sections::layout(section, state, self.section_width(), self.section_metrics())
            .height
            + self.scaled(RELATED_GAP);
        let progress = extent.get(ui::time::now()) / full.max(1.0);
        Disclose::reveal(progress, 1.0).turn
    }

    /// A section in its slot: revealed as far as its motion is, its content
    /// laid out at full height under a clip, fading in over the second half.
    fn draw_section(&self, ctx: &mut PaintCtx<'_>, frame: &RootFrame<'_>, slot: &SectionSlot) {
        let (state, cursor) = (frame.state, frame.cursor);
        let open = self.open_section(state) == Some(slot.section);
        let reveal = Disclose::reveal(slot.progress(), slot.rect.height());
        let clip = slot.clip();
        let radius = control_radius(ctx.theme, slot.rect);
        let shape = RoundedRect::from_rect(clip, radius.min(clip.height() / 2.0));
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &shape,
        );
        // The section's rows lie on the panel's own surface, as a
        // disclosure row's body does: its place under the control that
        // opened it and its inset group them, never a box inside the panel.
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            reveal.opacity,
            ctx.transform,
            &clip,
        );
        let focused = match frame.focused {
            Some(RootStop::Section(stop)) if open => Some(stop),
            _ => None,
        };
        let origin = slot.rect.origin();
        sections::paint(
            ctx,
            &slot.layout,
            &SectionPaint {
                section: slot.section,
                state,
                metrics: self.section_metrics(),
                origin,
                cursor: cursor
                    .filter(|point| open && clip.contains(*point))
                    .map(|point| Point::new(point.x - origin.x, point.y - origin.y)),
                focused,
                password_ready: self.password_ready,
            },
        );
        ctx.scene.pop_layer();
        ctx.scene.pop_layer();
    }

    /// The section a row opens in place, if it opens one.
    fn row_section(row: RootRow) -> Option<QsSection> {
        match row {
            RootRow::Battery => Some(QsSection::Battery),
            RootRow::Devices => Some(QsSection::Devices),
            RootRow::Clipboard => Some(QsSection::Clipboard),
            RootRow::Notifications => None,
        }
    }

    /// A section row's chevron, turning down as its section opens.
    fn draw_row_chevron(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        section: QsSection,
        state: &QuickSettingsState,
    ) {
        let bleed = ui::chrome::row_bleed(ctx.theme);
        let glyph = Self::glyph_at(self.layout_scale);
        let slot = Rect::new(
            rect.x1 - bleed - glyph,
            rect.center().y - glyph / 2.0,
            rect.x1 - bleed,
            rect.center().y + glyph / 2.0,
        );
        let turn = self.chevron_turn(section, state);
        ui::icons::draw_icon(
            ctx.scene,
            ctx.transform
                .pre_rotate_about(std::f64::consts::FRAC_PI_2 * turn, slot.center()),
            slot,
            ui::icons::Icon::ChevronRight,
            ctx.theme.tokens.colors.text_dim,
            1.5,
        );
    }

    fn draw_row(&self, ctx: &mut PaintCtx<'_>, frame: &RootFrame<'_>, row: RootRow, rect: Rect) {
        let (state, cursor) = (frame.state, frame.cursor);
        let focused = frame.focused == Some(RootStop::Row(row));
        let hovered = cursor.is_some_and(|p| rect.contains(p));
        let interaction = Interaction {
            hovered,
            focused,
            ..Interaction::default()
        };
        match row {
            RootRow::Battery => {
                let present = state.battery.present;
                let title = t(if present { "qs-battery" } else { "qs-power" });
                let value = present.then(|| format!("{}%", state.battery.percent));
                let secondary = if present {
                    sections::battery_time_text(state)
                        .unwrap_or_else(|| sections::battery_state_text(state))
                } else {
                    state.power_profile.label()
                };
                let icon = match state.battery.state {
                    tilo_power_mgmt::BatteryState::Charging if present => {
                        ui::icons::Icon::BatteryCharging
                    }
                    _ if present => ui::icons::Icon::Battery,
                    _ => ui::icons::Icon::Gauge,
                };
                let chevron = self.scaled(GLYPH + RELATED_GAP);
                let mut face = ListRowFace::new(&title)
                    .leading(RowLeading::Icon(icon))
                    .secondary(&secondary)
                    .trailing_width(chevron)
                    .interaction(interaction);
                if let Some(value) = value.as_deref() {
                    face = face.value(value);
                }
                face.paint(ctx, rect);
                self.draw_row_chevron(ctx, rect, QsSection::Battery, state);
            }
            RootRow::Devices | RootRow::Clipboard => {
                let (icon, title, value, section) = if row == RootRow::Devices {
                    (
                        ui::icons::Icon::Monitor,
                        t("qs-devices"),
                        Self::devices_subline(state),
                        QsSection::Devices,
                    )
                } else {
                    (
                        ui::icons::Icon::ClipboardPaste,
                        t("qs-clipboard"),
                        Self::clipboard_subline(state),
                        QsSection::Clipboard,
                    )
                };
                ListRowFace::new(&title)
                    .leading(RowLeading::Icon(icon))
                    .value(&value)
                    .trailing_width(self.scaled(GLYPH + RELATED_GAP))
                    .interaction(interaction)
                    .paint(ctx, rect);
                self.draw_row_chevron(ctx, rect, section, state);
            }
            RootRow::Notifications => {
                ListRowFace::new(&t("qs-notifications"))
                    .leading(RowLeading::Icon(ui::icons::Icon::Bell))
                    .value(&Self::notifications_subline(state))
                    .disclosure()
                    .interaction(interaction)
                    .paint(ctx, rect);
            }
        }
        if focused {
            ui::widget::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                rect,
                f64::from(ctx.theme.tokens.radii.control),
                ctx.theme,
            );
        }
    }

    /// A footer button: quiet — its glyph and name, lifting on hover.
    fn draw_footer_button(
        ctx: &mut PaintCtx<'_>,
        frame: &RootFrame<'_>,
        button: FooterButton,
        rect: Rect,
    ) {
        Self::draw_icon_button(
            ctx,
            frame,
            RootStop::Footer(button),
            button.icon(),
            &button.label(),
            rect,
        );
    }
    /// A footer icon button: quiet, named, lifting under the pointer.
    fn draw_icon_button(
        ctx: &mut PaintCtx<'_>,
        frame: &RootFrame<'_>,
        stop: RootStop,
        icon: ui::icons::Icon,
        name: &str,
        rect: Rect,
    ) {
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ui::widget::primitives::IconButton::face(rect, icon, name)
                .hovered(frame.cursor.is_some_and(|p| rect.contains(p)))
                .focused(frame.focused == Some(stop)),
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
            let mut r = self.edit_row(self.panel, i);
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
                colors.intent.accent_ink,
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
                    colors.intent.accent_ink,
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
    #[allow(
        clippy::too_many_lines,
        reason = "the root and edit pages publish their stops from the same tables their paint and hit-testing read"
    )]
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
            // reading lands after the panel opens, and a reader that
            // resolved a control a moment earlier must still press that
            // control, not whichever one moved into its slot.
            let layout = self.page_layout(state);
            let stops = self.stops_in(&layout, state);
            let focused = self
                .root_focus_index(&stops)
                .and_then(|index| stops.get(index).copied());
            for stop in stops {
                let mut node = self
                    .stop_access_node(stop, state)
                    .with_hashed_key(self.semantic_stop_name(stop, state))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(focused == Some(stop)),
                    );
                match stop {
                    RootStop::Control(_, control) => {
                        node.state.checked = Some(Self::on(control, state));
                        node.state.disabled = state.radio_change_pending
                            && matches!(
                                control,
                                QuickSettingsControl::AirplaneMode | QuickSettingsControl::Wifi
                            );
                    }
                    RootStop::Expand(_, control) => {
                        node.state.expanded = Some(
                            Self::section_of(control, state).is_some()
                                && Self::section_of(control, state) == self.open_section(state),
                        );
                    }
                    RootStop::Row(
                        row @ (RootRow::Battery | RootRow::Devices | RootRow::Clipboard),
                    ) => {
                        node.state.expanded =
                            Some(self.open_section(state) == Self::row_section(row));
                    }
                    RootStop::SoundExpand => {
                        node.state.expanded =
                            Some(self.open_section(state) == Some(QsSection::Sound));
                    }
                    RootStop::Section(SectionStop::Output(index)) => {
                        node.state.selected = state
                            .sound_outputs
                            .get(index)
                            .is_some_and(|output| output.current);
                    }
                    RootStop::Section(SectionStop::Profile(index)) => {
                        node.state.checked = state.vpns.get(index).map(|profile| profile.active);
                    }
                    RootStop::Section(SectionStop::Mode(mode)) => {
                        node.state.selected = state.power_profile == mode;
                    }
                    _ => {}
                }
                root.children
                    .push(bounded(node, self.stop_rect_in(&layout, stop, state)));
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
                self.edit_row(self.panel, index)
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
            RootStop::Control(_, control) => AccessNode::new(Role::Switch)
                .with_name(Self::rendered_label(control, state))
                .with_description(Self::subline(control, state)),
            RootStop::Expand(_, control) => {
                AccessNode::new(Role::Button).with_name(Self::details_label(control))
            }
            RootStop::SoundExpand => AccessNode::new(Role::Button).with_name(t("qs-sound-output")),
            RootStop::Section(stop) => section_access_node(stop, state),
            RootStop::Row(RootRow::Battery) => AccessNode::new(Role::Button)
                .with_name(t(if state.battery.present {
                    "qs-battery"
                } else {
                    "qs-power"
                }))
                .with_value(if state.battery.present {
                    format!("{}%", state.battery.percent)
                } else {
                    state.power_profile.label()
                }),
            RootStop::Row(RootRow::Devices) => AccessNode::new(Role::Button)
                .with_name(t("qs-devices"))
                .with_value(Self::devices_subline(state))
                .with_description(t("qs-devices-a11y-description")),
            RootStop::Row(RootRow::Clipboard) => AccessNode::new(Role::Button)
                .with_name(t("qs-clipboard"))
                .with_value(Self::clipboard_subline(state))
                .with_description(t("qs-clipboard-a11y-description")),
            RootStop::Row(RootRow::Notifications) => AccessNode::new(Role::Button)
                .with_name(t("qs-notifications"))
                .with_value(Self::notifications_subline(state)),
            RootStop::Edit => AccessNode::new(Role::Button).with_name(t("qs-edit-controls")),
            RootStop::Footer(button) => AccessNode::new(Role::Button)
                .with_name(button.label())
                .with_description(button.description()),
        }
    }
    fn details_label(control: QuickSettingsControl) -> String {
        t(match control {
            QuickSettingsControl::Wifi => "qs-network-details",
            QuickSettingsControl::Bluetooth => "qs-bluetooth-details",
            _ => "qs-vpn-details",
        })
    }
    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.panel),
            [i] if !self.edit_mode && *i < self.slider_count() => {
                Some(self.slider_geometry_for(self.sliders_block(), *i).0)
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

/// The semantic name of a stop in the open section.
fn section_stop_name(stop: SectionStop, state: &QuickSettingsState) -> String {
    match stop {
        SectionStop::TurnOn => "section_turn_on".into(),
        SectionStop::Network(index) => format!(
            "wifi_network_{}",
            state
                .wifi_networks
                .get(index)
                .map_or_else(String::new, |n| n.ssid.clone())
        ),
        SectionStop::PasswordCancel => "wifi_password_cancel".into(),
        SectionStop::PasswordConnect => "wifi_password_connect".into(),
        SectionStop::Settings => "section_settings".into(),
        SectionStop::Device(index) => format!(
            "bluetooth_device_{}",
            state
                .bt_devices
                .get(index)
                .map_or_else(String::new, |d| d.address.clone())
        ),
        SectionStop::DeviceDetails(index) => format!(
            "bluetooth_device_details_{}",
            state
                .bt_devices
                .get(index)
                .map_or_else(String::new, |d| d.address.clone())
        ),
        SectionStop::Profile(index) => format!(
            "vpn_profile_{}",
            state
                .vpns
                .get(index)
                .map_or_else(String::new, |p| p.uuid.clone())
        ),
        SectionStop::Output(index) => format!(
            "sound_output_{}",
            state
                .sound_outputs
                .get(index)
                .map_or(0, |output| output.node)
        ),
        SectionStop::Hosted(index) => format!(
            "{}_row_{index}",
            match state.section {
                Some(QsSection::Clipboard) => "clipboard",
                _ => "devices",
            }
        ),
        SectionStop::ClearClipboard => "clipboard_clear".into(),
        SectionStop::Mode(mode) => format!(
            "power_mode_{}",
            match mode {
                PowerProfile::Saver => "saver",
                PowerProfile::Balanced => "balanced",
                PowerProfile::Performance => "performance",
            }
        ),
    }
}

fn section_access_node(stop: SectionStop, state: &QuickSettingsState) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, Role};
    match stop {
        SectionStop::TurnOn => AccessNode::new(Role::Button).with_name(t(
            if state.section == Some(QsSection::Bluetooth) {
                "qs-bluetooth-turn-on"
            } else {
                "qs-wifi-turn-on"
            },
        )),
        SectionStop::Network(index) => {
            let network = state.wifi_networks.get(index);
            AccessNode::new(Role::Button)
                .with_name(network.map_or_else(String::new, |n| n.ssid.clone()))
                .with_description(t(match network {
                    Some(n) if n.known => "qs-wifi-known",
                    Some(n) if n.secure => "qs-wifi-secured",
                    _ => "qs-wifi-open",
                }))
        }
        SectionStop::PasswordCancel => AccessNode::new(Role::Button).with_name(t("portal-cancel")),
        SectionStop::PasswordConnect => {
            AccessNode::new(Role::Button).with_name(t("portal-connect"))
        }
        SectionStop::Settings => AccessNode::new(Role::Link).with_name(t(match state.section {
            Some(QsSection::Bluetooth) => "qs-bluetooth-pair",
            Some(QsSection::Battery) if state.battery.present => "qs-battery-settings",
            Some(QsSection::Battery) => "qs-power-settings",
            Some(QsSection::Vpn) => "qs-vpn-settings",
            Some(QsSection::Sound) => "qs-sound-settings",
            Some(QsSection::Devices) => "qs-devices-settings",
            _ => "qs-wifi-settings",
        })),
        SectionStop::Hosted(index) => {
            let rows = state
                .section
                .map_or(&[][..], |section| sections::hosted_rows(section, state));
            let row = rows.get(index);
            AccessNode::new(Role::Button)
                .with_name(row.map_or_else(String::new, |row| row.title.clone()))
                .with_description(row.map_or_else(String::new, |row| row.detail.clone()))
                .with_state(
                    ui::a11y::AccessState::new().disabled(row.is_some_and(|row| !row.enabled)),
                )
        }
        SectionStop::ClearClipboard => AccessNode::new(Role::Button)
            .with_name(t("qs-clipboard-clear-history"))
            .with_description(t("qs-clipboard-clear-a11y")),
        SectionStop::Device(index) => {
            let device = state.bt_devices.get(index);
            AccessNode::new(Role::Button)
                .with_name(device.map_or_else(String::new, |d| d.name.clone()))
                .with_description(t(if device.is_some_and(|d| d.connected) {
                    "qs-bluetooth-device-connected-a11y"
                } else {
                    "qs-bluetooth-device-paired-a11y"
                }))
        }
        SectionStop::DeviceDetails(_) => {
            AccessNode::new(Role::Button).with_name(t("qs-bluetooth-use-elsewhere"))
        }
        SectionStop::Profile(index) => AccessNode::new(Role::Switch).with_name(
            state
                .vpns
                .get(index)
                .map_or_else(String::new, |p| p.name.clone()),
        ),
        SectionStop::Mode(mode) => AccessNode::new(Role::RadioButton).with_name(mode.label()),
        SectionStop::Output(index) => AccessNode::new(Role::RadioButton).with_name(
            state
                .sound_outputs
                .get(index)
                .map_or_else(String::new, |output| output.name.clone()),
        ),
    }
}

/// `rect`, laid out inside a section whose own rect on the page is
/// `section`, in the page's coordinates.
fn on_page(section: Rect, rect: Rect) -> Rect {
    Affine::translate(section.origin().to_vec2()).transform_rect_bbox(rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::quick_settings::{BtDevice, WifiNetwork};
    use ui::time::Duration;
    use ui::tokens::Motion;
    const PANEL: Rect = Rect::new(100., 48., 500., 960.);

    fn motion() -> EffectiveMotion {
        EffectiveMotion::new(Motion::default(), false)
    }

    /// A laptop: Wi-Fi, Bluetooth, a battery and the power daemon.
    fn laptop() -> QuickSettingsState {
        let mut state = QuickSettingsState {
            brightness_available: true,
            bluetooth_available: true,
            bluetooth: true,
            radios: os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..os_actions::RadioState::default()
            },
            wifi: true,
            power_profiles: vec![
                PowerProfile::Saver,
                PowerProfile::Balanced,
                PowerProfile::Performance,
            ],
            ..QuickSettingsState::default()
        };
        state.battery = tilo_power_mgmt::BatterySnapshot {
            present: true,
            percent: 64,
            state: tilo_power_mgmt::BatteryState::Discharging,
        };
        state.wifi_networks = vec![
            WifiNetwork {
                ssid: "Home".into(),
                signal: 4,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: true,
                known: true,
            },
            WifiNetwork {
                ssid: "Cafe".into(),
                signal: 2,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: false,
                known: false,
            },
            WifiNetwork {
                ssid: "Library".into(),
                signal: 3,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: false,
                known: false,
            },
        ];
        state.bt_devices = vec![BtDevice {
            name: "Headphones".into(),
            address: "02:00:00:00:00:01".into(),
            battery: 80,
            connected: true,
        }];
        state
    }

    fn laid_out(state: &QuickSettingsState) -> QuickSettingsRoot {
        let mut root = QuickSettingsRoot {
            panel: PANEL,
            ..QuickSettingsRoot::default()
        };
        root.settle_sections(state);
        root
    }

    fn tile(
        root: &QuickSettingsRoot,
        state: &QuickSettingsState,
        control: QuickSettingsControl,
    ) -> Rect {
        root.page_layout(state)
            .tiles
            .into_iter()
            .find(|(c, _)| *c == control)
            .map(|(_, rect)| rect)
            .expect("the tile is shown")
    }

    #[test]
    fn the_default_grid_holds_the_everyday_controls_this_machine_has() {
        let root = QuickSettingsRoot::default();
        assert_eq!(root.config().controls, QuickSettingsControl::DEFAULT);
        let mut state = laptop();
        assert_eq!(
            root.visible_controls(&state),
            [
                QuickSettingsControl::Wifi,
                QuickSettingsControl::Bluetooth,
                QuickSettingsControl::DoNotDisturb,
                QuickSettingsControl::NightLight,
                QuickSettingsControl::AirplaneMode,
                QuickSettingsControl::PowerMode,
            ],
            "VPN without profiles and rotation lock without a sensor stay hidden"
        );
        state.vpns = vec![os_actions::VpnConnectionInfo {
            name: "Work".into(),
            uuid: "work-uuid".into(),
            active: false,
            kind: "wireguard".into(),
        }];
        state.rotation_available = true;
        assert_eq!(root.control_count(&state), 8);

        // No radio, no adapter, no daemon: no inert tiles.
        let bare = QuickSettingsState::default();
        assert_eq!(
            root.visible_controls(&bare),
            [
                QuickSettingsControl::DoNotDisturb,
                QuickSettingsControl::NightLight
            ]
        );
    }

    #[test]
    fn root_controls_reflect_present_hardware_and_migrated_config() {
        // Wired only: the network slot is the wired tile.
        let root = QuickSettingsRoot::default();
        let state = QuickSettingsState {
            wired: Some(os_actions::WiredInfo {
                connected: true,
                device: Some("enp0s31f6".into()),
                ..Default::default()
            }),
            ..QuickSettingsState::default()
        };
        assert!(
            root.visible_controls(&state)
                .contains(&QuickSettingsControl::Wifi)
        );
        assert_eq!(
            QuickSettingsRoot::rendered_label(QuickSettingsControl::Wifi, &state),
            "Wired network"
        );
        assert_eq!(
            QuickSettingsRoot::section_of(QuickSettingsControl::Wifi, &state),
            None,
            "a wired network has nothing to open"
        );
        let names: Vec<_> = root
            .accessibility_snapshot(&state)
            .children
            .iter()
            .filter_map(|node| node.name.clone())
            .collect();
        assert!(names.contains(&"Wired network".to_owned()));
        assert!(!names.contains(&"Bluetooth".to_owned()));

        // A saved layout that kept Airplane mode keeps it removable without
        // a radio.
        let kept = QuickSettingsRoot::with_config(QuickSettingsConfig {
            controls: vec![
                QuickSettingsControl::Wifi,
                QuickSettingsControl::NightLight,
                QuickSettingsControl::AirplaneMode,
            ],
        });
        assert!(
            kept.edit_controls(&QuickSettingsState::default())
                .contains(&QuickSettingsControl::NightLight)
        );
        assert!(
            !kept
                .edit_controls(&QuickSettingsState::default())
                .contains(&QuickSettingsControl::MobileData)
        );
    }

    #[test]
    fn semantic_targets_cover_every_visible_root_control() {
        let state = laptop();
        let root = laid_out(&state);
        let targets = root.semantic_rects(&state);
        for required in [
            "quick_settings_edit",
            "quick_settings_settings",
            "quick_settings_lock",
            "quick_settings_power",
            "quick_settings_devices",
            "quick_settings_clipboard",
            "quick_settings_notifications",
            "quick_settings_battery",
            "quick_settings_brightness",
            "quick_settings_volume",
            "quick_settings_network",
            "quick_settings_network_details",
            "quick_settings_bluetooth",
            "quick_settings_bluetooth_details",
            "quick_settings_night_light",
            "quick_settings_power_mode",
        ] {
            assert!(
                targets
                    .iter()
                    .any(|(name, rect)| name == required && rect.area() > 0.0),
                "missing semantic target {required}"
            );
        }
        // No two targets overlap: a tile's switch and chevron are two.
        for (i, (a, ra)) in targets.iter().enumerate() {
            for (b, rb) in targets.iter().skip(i + 1) {
                if a == "quick_settings_edit" || b == "quick_settings_edit" {
                    continue;
                }
                let overlap = ra.intersect(*rb);
                assert!(
                    overlap.width() <= 1e-9 || overlap.height() <= 1e-9,
                    "{a} {ra:?} overlaps {b} {rb:?}"
                );
            }
        }
    }

    #[test]
    fn a_tile_turns_its_control_on_and_its_chevron_opens_its_section() {
        let mut state = laptop();
        let mut root = laid_out(&state);
        let wifi = tile(&root, &state, QuickSettingsControl::Wifi);
        let body = Point::new(wifi.x0 + 12.0, wifi.center().y);
        assert_eq!(
            root.handle_click(body, PANEL, &mut state),
            RootAction::Changed(PendingRootActions {
                wifi: Some(false),
                ..Default::default()
            }),
            "the body turns Wi-Fi off"
        );
        assert_eq!(state.section, None, "and opens nothing");

        let chevron = root.expand_rect(wifi).center();
        assert_eq!(
            root.handle_click(chevron, PANEL, &mut state),
            RootAction::SectionOpened(QsSection::Wifi)
        );
        assert_eq!(state.section, Some(QsSection::Wifi));
        assert!(state.wifi, "opening the list does not change the radio");

        // One section at a time: Bluetooth's chevron moves the open one.
        root.settle_sections(&state);
        let bluetooth = tile(&root, &state, QuickSettingsControl::Bluetooth);
        assert_eq!(
            root.handle_click(root.expand_rect(bluetooth).center(), PANEL, &mut state),
            RootAction::SectionOpened(QsSection::Bluetooth)
        );
        assert_eq!(state.section, Some(QsSection::Bluetooth));
        // Its chevron again closes it, and Esc closes whatever is open.
        root.settle_sections(&state);
        assert_eq!(
            root.handle_click(root.expand_rect(bluetooth).center(), PANEL, &mut state),
            RootAction::LayoutChanged
        );
        assert_eq!(state.section, None);
        state.open_section(QsSection::Battery);
        assert!(root.close_section(&mut state));
        assert_eq!(state.section, None);
        assert!(!root.close_section(&mut state));
    }

    #[test]
    fn a_section_opens_in_place_under_its_row_and_the_tiles_stay_put() {
        let mut state = laptop();
        let root = laid_out(&state);
        let before = root.page_layout(&state);
        let height_before = root.preferred_height(&state);

        state.open_section(QsSection::Wifi);
        let root = laid_out(&state);
        let after = root.page_layout(&state);
        assert_eq!(before.tiles.len(), after.tiles.len());
        let slot = after
            .sections
            .iter()
            .find(|slot| slot.section == QsSection::Wifi)
            .expect("the Wi-Fi section shows");
        let wifi = tile(&root, &state, QuickSettingsControl::Wifi);
        assert!(slot.rect.y0 > wifi.y1, "under the Wi-Fi tile's row");
        assert_eq!(slot.rect.x0, after.sliders.x0, "full width");
        assert_eq!(slot.rect.x1, after.sliders.x1);
        let grown = slot.rect.height() + RELATED_GAP;
        for ((control, a), (_, b)) in before.tiles.iter().zip(&after.tiles) {
            if a.y0 <= wifi.y0 {
                assert_eq!(a, b, "{control:?} on or above the row stays put");
            } else {
                assert!(
                    (b.y0 - a.y0 - grown).abs() < 1e-6,
                    "{control:?} moves down with it"
                );
            }
        }
        assert!((after.bottom - before.bottom - grown).abs() < 1e-6);
        assert!(
            (root.preferred_height(&state) - height_before - grown).abs() < 1e-6,
            "the panel grows by the section"
        );
    }

    #[test]
    fn opening_and_closing_a_section_moves_on_the_disclosure_motion() {
        let mut state = laptop();
        let mut root = laid_out(&state);
        let start = ui::time::now();
        let closed = root.page_layout_at(PANEL, &state, start);
        assert!(closed.sections.is_empty());

        state.open_section(QsSection::Wifi);
        root.follow_sections(&state, motion(), MotionSource::Pointer, start);
        assert!(root.is_animating(start));
        let full = sections::layout(
            QsSection::Wifi,
            &state,
            root.section_width(),
            root.section_metrics(),
        )
        .height;
        let settles = root.reveal[&QsSection::Wifi]
            .settles_at()
            .expect("it moves");
        let mut last = -1.0;
        let mut last_bottom = closed.bottom;
        let steps = 12;
        for step in 1..=steps {
            let at = start + (settles - start) * step / steps;
            let page = root.page_layout_at(PANEL, &state, at);
            let shown = page.sections.first().map_or(0.0, |slot| slot.shown);
            assert!(shown >= last, "the reveal only grows: {shown} after {last}");
            assert!(page.bottom >= last_bottom, "the page grows with it");
            last = shown;
            last_bottom = page.bottom;
        }
        assert!((last - full).abs() < 0.5, "it ends fully open");
        assert!(!root.is_animating(settles + Duration::from_millis(1)));

        // Closing runs it back to nothing; the content stays laid out at
        // full height under the shrinking clip.
        let closing = settles + Duration::from_millis(5);
        state.section = None;
        root.follow_sections(&state, motion(), MotionSource::Pointer, closing);
        let midway = root.page_layout_at(PANEL, &state, closing + Duration::from_millis(40));
        let slot = midway.sections.first().expect("still closing");
        assert!(slot.shown < full && (slot.rect.height() - full).abs() < 1e-9);
        let ends = root.reveal[&QsSection::Wifi]
            .settles_at()
            .expect("it moves");
        let gone = root.page_layout_at(PANEL, &state, ends + Duration::from_millis(1));
        assert!(gone.sections.is_empty());
        assert!((gone.bottom - closed.bottom).abs() < 1e-6);
    }

    #[test]
    fn the_wifi_section_turns_wifi_on_and_joins_networks_or_asks_for_a_password() {
        let mut state = laptop();
        state.wifi = false;
        state.radios.wifi_enabled = false;
        state.open_section(QsSection::Wifi);
        let mut root = laid_out(&state);
        let stops = root.root_stops(&state);
        assert!(stops.contains(&RootStop::Section(SectionStop::TurnOn)));
        let turn_on = root.stop_rect(RootStop::Section(SectionStop::TurnOn), &state);
        assert_eq!(
            root.handle_click(turn_on.center(), PANEL, &mut state),
            RootAction::Changed(PendingRootActions {
                wifi: Some(true),
                ..Default::default()
            })
        );

        state.wifi = true;
        root.settle_sections(&state);
        let press = |root: &mut QuickSettingsRoot, state: &mut QuickSettingsState, index: usize| {
            let rect = root.stop_rect(RootStop::Section(SectionStop::Network(index)), state);
            root.handle_click(rect.center(), PANEL, state)
        };
        assert_eq!(
            press(&mut root, &mut state, 1),
            RootAction::Wifi(WifiIntent::AskPassword("Cafe".into())),
            "a protected network this machine does not know asks for its password"
        );
        assert_eq!(
            press(&mut root, &mut state, 2),
            RootAction::Wifi(WifiIntent::Join("Library".into()))
        );
        state.wifi_networks[1].known = true;
        assert_eq!(
            press(&mut root, &mut state, 1),
            RootAction::Wifi(WifiIntent::Join("Cafe".into())),
            "a known one joins without asking"
        );

        // The password is asked in place, under its network.
        state.wifi_networks[1].known = false;
        state.wifi_password = Some(crate::shell::quick_settings::WifiPasswordPrompt::new(
            "Cafe",
        ));
        root.settle_sections(&state);
        let field = root
            .wifi_password_field_rect(&state)
            .expect("the field is inside the section");
        let cafe = root.stop_rect(RootStop::Section(SectionStop::Network(1)), &state);
        assert!(field.y0 > cafe.y1);
        let connect = root.stop_rect(RootStop::Section(SectionStop::PasswordConnect), &state);
        assert!(connect.y0 > field.y1);
        assert_eq!(
            root.handle_click(connect.center(), PANEL, &mut state),
            RootAction::Wifi(WifiIntent::SubmitPassword)
        );
    }

    #[test]
    fn the_volume_chevron_chooses_where_sound_plays_under_the_sliders() {
        let mut state = laptop();
        state.sound_outputs = vec![
            crate::shell::quick_settings::SoundOutput {
                node: 41,
                name: "Speakers".into(),
                current: true,
            },
            crate::shell::quick_settings::SoundOutput {
                node: 57,
                name: "Headphones".into(),
                current: false,
            },
        ];
        let mut root = laid_out(&state);
        root.sync_displays(&state);
        let wifi_before = tile(&root, &state, QuickSettingsControl::Wifi);
        let chevron = root.stop_rect(RootStop::SoundExpand, &state);
        let volume = root.slider_face_rect(root.sliders_block(), root.volume_slider());
        assert!(
            chevron.x0 >= volume.x1,
            "the chevron is beside the slider, not on it"
        );
        assert_eq!(
            root.handle_click(chevron.center(), PANEL, &mut state),
            RootAction::SectionOpened(QsSection::Sound)
        );
        root.settle_sections(&state);
        let headphones = root.stop_rect(RootStop::Section(SectionStop::Output(1)), &state);
        assert!(headphones.y0 > volume.y1, "under the sliders");
        assert!(
            tile(&root, &state, QuickSettingsControl::Wifi).y0 > wifi_before.y0,
            "the tiles move down to make room"
        );
        assert_eq!(
            root.handle_click(headphones.center(), PANEL, &mut state),
            RootAction::SoundOutput(57)
        );
        let speakers = root.stop_rect(RootStop::Section(SectionStop::Output(0)), &state);
        assert_eq!(
            root.handle_click(speakers.center(), PANEL, &mut state),
            RootAction::None,
            "where sound already plays has nothing to do"
        );
    }

    #[test]
    fn the_battery_row_opens_its_section_with_the_three_power_modes() {
        let mut state = laptop();
        let mut root = laid_out(&state);
        let row = root.stop_rect(RootStop::Row(RootRow::Battery), &state);
        assert_eq!(
            root.handle_click(row.center(), PANEL, &mut state),
            RootAction::SectionOpened(QsSection::Battery)
        );
        root.settle_sections(&state);
        let saver = root.stop_rect(
            RootStop::Section(SectionStop::Mode(PowerProfile::Saver)),
            &state,
        );
        assert!(saver.y0 > row.y1, "the modes are under the battery row");
        assert_eq!(
            root.handle_click(saver.center(), PANEL, &mut state),
            RootAction::Changed(PendingRootActions {
                power: Some(PowerProfile::Saver),
                ..Default::default()
            })
        );
        assert_eq!(state.power_profile, PowerProfile::Saver);
        // The Power mode tile switches the saver back off.
        let tile = tile(&root, &state, QuickSettingsControl::PowerMode);
        assert_eq!(
            root.handle_click(tile.center(), PANEL, &mut state),
            RootAction::Changed(PendingRootActions {
                power: Some(PowerProfile::Balanced),
                ..Default::default()
            })
        );
    }

    #[test]
    fn root_focus_order_matches_visible_actions() {
        // Opening has no hidden focus, and the first Tab selects Brightness.
        let mut state = laptop();
        let mut root = laid_out(&state);
        root.keyboard_focus = Some(4);
        root.reset_focus();
        assert_eq!(root.keyboard_focus, None);
        assert!(root.focus_next(&state, false));
        assert_eq!(root.focused_stop(&state), Some(RootStop::Slider(0)));

        // Shift+Tab from no selection lands on Power, whatever hardware
        // precedes it.
        let mut bare = QuickSettingsState::default();
        let mut root = laid_out(&bare);
        root.reset_focus();
        assert!(root.focus_next(&bare, true));
        assert_eq!(
            root.focused_stop(&bare),
            Some(RootStop::Footer(FooterButton::Power))
        );
        assert_eq!(
            root.activate_focused(PANEL, &mut bare),
            RootAction::OpenPowerMenu
        );
        // Device state arriving under focus keeps it on its stop.
        bare.volume_available = true;
        bare.brightness_available = true;
        assert_eq!(
            root.focused_stop(&bare),
            Some(RootStop::Footer(FooterButton::Power))
        );

        // Every stop activates as what it is.
        let mut root = laid_out(&state);
        for (stop, expected) in [
            (
                RootStop::Row(RootRow::Devices),
                RootAction::SectionOpened(QsSection::Devices),
            ),
            (
                RootStop::Row(RootRow::Clipboard),
                RootAction::SectionOpened(QsSection::Clipboard),
            ),
            (
                RootStop::Row(RootRow::Notifications),
                RootAction::Enter(QsLevel::Notifications),
            ),
            (
                RootStop::Footer(FooterButton::Settings),
                RootAction::OpenSettings(None),
            ),
            (RootStop::Footer(FooterButton::Lock), RootAction::Lock),
            (RootStop::Edit, RootAction::LayoutChanged),
        ] {
            // An opened section adds its stops: find this one afresh.
            let stops = root.root_stops(&state);
            let path = stops.iter().position(|s| *s == stop).unwrap();
            assert!(root.focus_accessibility_path(&[path], &state));
            assert_eq!(root.focused_stop(&state), Some(stop));
            assert_eq!(root.activate_focused(PANEL, &mut state), expected);
            root.edit_mode = false;
        }

        // An open section's stops follow its row, before the next row.
        state.open_section(QsSection::Wifi);
        let root = laid_out(&state);
        let stops = root.root_stops(&state);
        let expand = stops
            .iter()
            .position(|s| matches!(s, RootStop::Expand(_, QuickSettingsControl::Wifi)))
            .unwrap();
        let first_section = stops
            .iter()
            .position(|s| matches!(s, RootStop::Section(_)))
            .unwrap();
        let next_row_tile = stops
            .iter()
            .position(|s| matches!(s, RootStop::Control(_, QuickSettingsControl::DoNotDisturb)))
            .unwrap();
        assert!(expand < first_section && first_section < next_row_tile);
    }

    #[test]
    fn each_focus_stop_marks_exactly_one_accessibility_node() {
        let mut state = laptop();
        state.open_section(QsSection::Wifi);
        let mut root = laid_out(&state);
        let stops = root.root_stops(&state);
        for (index, stop) in stops.iter().enumerate() {
            assert!(root.focus_accessibility_path(&[index], &state));
            let snapshot = root.accessibility_snapshot(&state);
            assert_eq!(snapshot.children.len(), stops.len());
            let focused: Vec<_> = snapshot
                .children
                .iter()
                .enumerate()
                .filter(|(_, node)| node.state.focused)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(focused, [index], "{stop:?}");
            assert_eq!(
                snapshot.children[index]
                    .bounds
                    .map(ui::a11y::AccessBounds::to_rect),
                Some(root.stop_rect(*stop, &state)),
                "{stop:?} publishes the rect it paints"
            );
        }
    }

    #[test]
    fn the_layout_never_overlaps_and_grows_with_large_text() {
        let mut state = laptop();
        state.open_section(QsSection::Battery);
        for scale in [1.0_f32, 1.5, 2.0] {
            let mut root = QuickSettingsRoot::default();
            root.set_layout_text_scale(scale);
            root.panel = PANEL;
            root.settle_sections(&state);
            let page = root.page_layout(&state);
            let mut rects: Vec<Rect> = vec![page.sliders];
            rects.extend(page.tiles.iter().map(|(_, r)| *r));
            rects.extend(page.sections.iter().map(SectionSlot::clip));
            rects.extend(page.rows.iter().map(|(_, r)| *r));
            rects.extend(page.footer.iter().map(|(_, r)| *r));
            for (i, a) in rects.iter().enumerate() {
                for b in rects.iter().skip(i + 1) {
                    let overlap = a.intersect(*b);
                    assert!(
                        overlap.width() <= 1e-9 || overlap.height() <= 1e-9,
                        "{a:?} overlaps {b:?} at {scale}"
                    );
                }
            }
            for (_, tile) in &page.tiles {
                assert!(
                    tile.height() >= root.line_height(TextRole::Label) + root.caption_line_height(),
                    "a tile holds its two lines at {scale}"
                );
            }
        }
    }

    /// Lay `root` out in a popover `viewport` shorter than its page, then
    /// Tab through every stop: each lands fully inside the shown part of the
    /// popover, scrolled there the way a list follows its focus.
    #[test]
    fn a_short_popover_scrolls_every_stop_into_view() {
        let mut state = laptop();
        state.open_section(QsSection::Wifi);
        let mut root = QuickSettingsRoot {
            panel: PANEL,
            ..QuickSettingsRoot::default()
        };
        root.settle_sections(&state);
        let viewport = Rect::new(100.0, 48.0, 500.0, 448.0);
        let theme = Theme::dark();
        root.sync_displays(&state);
        root.sync_scroll(viewport, &theme, &state);
        root.lay_out(viewport, &state);
        assert!(root.scrolls(), "a page taller than {viewport:?} scrolls");
        let stops = root.root_stops(&state);
        root.reset_focus();
        let mut deepest: f64 = 0.0;
        for stop in &stops {
            root.focus_next(&state, false);
            assert_eq!(root.focused_stop(&state), Some(*stop));
            let shown = root.visible_rect();
            let rect = root.stop_rect(*stop, &state);
            if *stop == RootStop::Edit {
                continue;
            }
            assert!(
                rect.y0 >= shown.y0 - 1e-6 && rect.y1 <= shown.y1 + 1e-6,
                "{stop:?} at {rect:?} is not shown in {shown:?}"
            );
            deepest = deepest.max(root.scroll_offset());
        }
        assert!(deepest > 0.0, "reaching the last stops scrolled the page");
    }

    #[test]
    fn edit_mode_adds_removes_and_reorders_by_keyboard_pointer_and_explicit_remove() {
        // Keyboard reorder, then add from the available chips.
        let state = laptop();
        let mut root = QuickSettingsRoot {
            edit_mode: true,
            panel: PANEL,
            ..QuickSettingsRoot::default()
        };
        root.keyboard_focus = Some(0);
        assert!(root.reorder_focused(&state, 1));
        assert_eq!(root.config.controls[1], QuickSettingsControl::Wifi);

        // The row body starts a reorder; only its explicit action removes.
        let mut root = QuickSettingsRoot {
            edit_mode: true,
            panel: PANEL,
            ..QuickSettingsRoot::default()
        };
        let row = root.edit_row(PANEL, 1);
        let bluetooth = QuickSettingsControl::Bluetooth;
        assert_eq!(root.edit_click(row.center(), &state), RootAction::None);
        assert!(root.config.controls.contains(&bluetooth));
        let _ = root.finish_drag(&state);
        assert_eq!(
            root.edit_click(root.edit_action_rect(row).center(), &state),
            RootAction::LayoutChanged
        );
        assert!(!root.config.controls.contains(&bluetooth));
        // It is offered back as available and adds again.
        let (_, chip) = root
            .available_chips(&state)
            .into_iter()
            .find(|(c, _)| *c == bluetooth)
            .expect("Bluetooth is offered back");
        assert_eq!(
            root.edit_click(chip.center(), &state),
            RootAction::LayoutChanged
        );
        assert!(root.config.controls.contains(&bluetooth));

        // Done is the last edit stop and leaves edit mode with no stale focus.
        let edit_count = root.edit_controls(&state).len();
        root.keyboard_focus = Some(edit_count.saturating_add(1));
        let mut state = state;
        assert_eq!(
            root.activate_focused(PANEL, &mut state),
            RootAction::LayoutChanged
        );
        assert!(!root.edit_mode());
        assert_eq!(root.keyboard_focus, None);
    }

    #[test]
    fn edit_help_copy_uses_font_safe_keyboard_names() {
        let copy = QuickSettingsRoot::edit_help_copy();
        assert_eq!(copy, "Drag or press Ctrl + Up/Down to reorder");
        assert!(!copy.contains('·'), "no middle-dot join in chrome copy");
    }

    #[test]
    fn each_monitor_gets_a_named_brightness_slider() {
        let mut state = laptop();
        state.displays = vec![
            crate::shell::chrome_state::DisplayBrightness {
                connector: "eDP-1".into(),
                name: "Built-in display".into(),
                level: 0.5,
            },
            crate::shell::chrome_state::DisplayBrightness {
                connector: "DP-2".into(),
                name: "Dell U2720Q".into(),
                level: 0.75,
            },
        ];
        let mut root = laid_out(&state);
        root.sync_displays(&state);
        assert_eq!(root.slider_count(), 3);
        assert_eq!(root.slider_name(1), "Brightness, Dell U2720Q");
        assert!(
            root.semantic_rects(&state)
                .into_iter()
                .any(|(name, _)| name == "quick_settings_brightness_1")
        );
        assert_eq!(
            root.slider_commit(&state, 1),
            SliderCommit::DisplayBrightness(1, 75.0)
        );
    }

    // USER REQUIREMENT (2026-10-21): "get rid of the double card layout for
    // the brightness/volume in the quick settings, it looks bad"
    /// The sliders sit straight on the panel: rendered through the production
    /// `draw` onto the panel ground, the air around and between the slider
    /// bands is that ground, never a Control surface of its own, while each
    /// track's unfilled well still stands off that ground. An off tile in the
    /// same render keeps its card, so the probe can see one. Every preset.
    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "pixel probes inside a small, positive render"
    )]
    fn the_sliders_sit_on_the_panel_not_in_a_card_of_their_own() {
        let state = laptop();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut root = laid_out(&state);
            let ground = ui::chrome::shell::panel_ground(&theme);
            let (w, h) = (PANEL.width() as u32, PANEL.height() as u32);
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            root.draw(
                &mut scene,
                Affine::translate(-PANEL.origin().to_vec2()),
                &mut text,
                &theme,
                PANEL,
                None,
                &state,
            );
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            let mut target =
                tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4).unwrap();
            tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
                scene.encoding(),
                &mut target,
                None,
                ground,
            );
            let at = |p: Point| {
                let (x, y) = ((p.x - PANEL.x0) as usize, (p.y - PANEL.y0) as usize);
                let i = (y * w as usize + x) * 4;
                [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
            };
            let bare = at(Point::new(PANEL.x0 + 4.0, PANEL.y0 + 4.0));

            let layout = root.page_layout(&state);
            let sliders = layout.sliders;
            let first = root.slider_face_rect(sliders, 0);
            let second = root.slider_face_rect(sliders, 1);
            for probe in [
                // The block's leading edge, beside each band's glyph.
                Point::new(sliders.x0 + 3.0, first.center().y),
                Point::new(sliders.x0 + 3.0, second.center().y),
                // Between the two bands.
                Point::new(
                    first.x0 + 4.0,
                    f64::midpoint(first.y1, second.y0).min(second.y0 - 1.0),
                ),
                // Over the first track, clear of it and of the knob.
                Point::new(first.x0 + 2.0, first.y0 + 2.0),
            ] {
                assert_eq!(
                    at(probe),
                    bare,
                    "the panel ground shows at {probe:?}: no card under the sliders"
                );
            }

            for face in [first, second] {
                let well = at(Point::new(face.x1 - 4.0, face.center().y));
                let apart = well
                    .iter()
                    .zip(bare)
                    .map(|(a, b)| a.abs_diff(b))
                    .max()
                    .unwrap_or(0);
                assert!(
                    apart >= 8,
                    "the unfilled well {well:?} stands off the panel ground {bare:?}"
                );
            }

            let off = tile(&root, &state, QuickSettingsControl::DoNotDisturb);
            assert_ne!(
                at(Point::new(off.x0 + 3.0, off.center().y)),
                bare,
                "an off tile keeps its Control card, so the probe sees cards"
            );
        }
    }
}
