//! The window's header row, as a capability: the one place window-level
//! controls stand.
//!
//! A widget that draws the header row across the top of the window
//! provides [`WindowHeader`]: [`NavShell`](crate::widget::NavShell) does,
//! and so does a painted app that lays its top band out with
//! [`crate::chrome::HeaderRowFace`]. The host offers it the window's own
//! controls (today: History) before it builds the app. App windows never add
//! a second header when the app declines this capability; History remains
//! available through the platform action, palette and keyboard.
//!
//! A painted app keeps a [`HistoryOpener`]:
//!
//! ```ignore
//! impl WindowHeader for Editor {
//!     fn set_history_control(&mut self, control: Option<HistoryControl>) {
//!         self.history.set_control(control);
//!     }
//! }
//! // draw: let face = HeaderRowFace::new(area, theme).with_history_if(self.history.control().is_some());
//! //       self.history.paint(scene, transform, text, theme, face.history_rect());
//! // events: if self.history.handle_event(event, face.history_rect(), ctx.scale_factor) { .. }
//! // ActionControls::take_control_invocation: self.history.take()
//! ```

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::Widget;
use super::capability::capability_mut;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::DrawChild;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryControl {
    Closed,
    Open,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowStatus {
    pub text: String,
    pub action: Option<StatusAction>,
}

impl WindowStatus {
    #[must_use]
    pub fn says(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            action: None,
        }
    }

    #[must_use]
    pub fn with_action(mut self, action: StatusAction) -> Self {
        self.action = Some(action);
        self
    }
}

#[derive(Clone)]
pub struct StatusAction {
    pub label: String,
    pub binding: tilo_actions::ActionBinding<()>,
}

impl StatusAction {
    #[must_use]
    pub fn button(&self) -> super::Button {
        super::Button::new(STATUS_ACTION_ID, &self.label)
            .control_size(super::ControlSize::Small)
            .on_action(self.binding.clone())
    }
}

pub const STATUS_ACTION_ID: &str = "window-status-action";

impl PartialEq for StatusAction {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label && self.binding.address() == other.binding.address()
    }
}

impl std::fmt::Debug for StatusAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusAction")
            .field("label", &self.label)
            .field("address", self.binding.address())
            .finish()
    }
}

impl HistoryControl {
    #[must_use]
    pub fn of(open: bool) -> Self {
        if open { Self::Open } else { Self::Closed }
    }

    #[must_use]
    pub fn is_open(self) -> bool {
        self == Self::Open
    }
}

pub trait WindowHistory {
    fn set_window_presents_history(&mut self, tab_presents: bool);

    fn window_history_control(&self) -> Option<HistoryControl>;

    fn invoke_window_history_control(&mut self, control: HistoryControl) -> Result<(), String>;
}

pub trait WindowHeader {
    fn set_history_control(&mut self, control: Option<HistoryControl>);

    fn set_window_presents_search(&mut self, presents: bool) -> bool {
        let _ = presents;
        false
    }

    fn set_sync_status(&mut self, status: Option<&WindowStatus>) -> bool {
        let _ = status;
        false
    }

    /// Merge the window's bar into this header row, or stop (`None`). The row reserves
    /// `bar.controls_width` at its trailing end for the window's controls, which the host paints
    /// there, so the app's header and the window's controls read as one bar instead of two
    /// stacked ones. The row stands at every time: the window's controls come into the room it
    /// keeps for them, never over the app's title or actions. Returns whether the row took the
    /// bar; a row that does not leaves the host to float its controls over the window.
    fn set_window_bar(&mut self, bar: Option<WindowBar>) -> bool {
        let _ = bar;
        false
    }

    fn window_bar_slot(&self) -> Option<WindowBarSlot> {
        None
    }

    fn header_row_height(&self) -> f64 {
        0.0
    }

    fn column_frames(&self) -> Vec<ColumnFrame> {
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnFrame {
    pub key: &'static str,
    pub column: Rect,
    pub header: Rect,
    pub rail: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBar {
    pub controls_width: f64,
    /// The system already lists the window's actions where the person reaches
    /// them (the desktop bar's window menu, for the window in front): the row's
    /// actions that do not fit give way with no door of their own, since every
    /// one stays reachable there. `false`: nothing lists them, so what does not
    /// fit goes behind the row's door ([`crate::widget::OverflowDoor`]). Each
    /// host states it; no row guesses it from what desktop it is on.
    pub lists_actions: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBarSlot {
    pub row: Rect,
    pub controls: Rect,
}

pub(crate) fn offer_window_bar(root: &mut dyn Widget, bar: Option<WindowBar>) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        return header.set_window_bar(bar);
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_window_bar(child, bar))
}

/// Whether a header row of `content` holds the window's bar as the window's own bar: the row the
/// content draws at the window's top is the bar then — the window draws none of its own and its
/// controls stand at the row's trailing end — where a row that stands as the bar's slots, or a
/// content that draws no row at all, leaves the window its bar.
///
/// Every window asks this of its content, whatever draws it: a frame of the shell's preset
/// ([`crate::widget::WindowFrame`]), a window of the desktop's strips
/// ([`crate::widget::WindowGroup`]) and the frame the layout check stages
/// ([`crate::headless::FrameHarness`]), so a window shows one bar, never two stacked, and no two
/// of them can disagree about which.
#[must_use]
pub(crate) fn content_bar(content: &dyn Widget) -> Option<WindowBarSlot> {
    let row = root_header_row(content)?;
    (row.header_row_height() > 0.0)
        .then(|| row.window_bar_slot())
        .flatten()
        .filter(|slot| slot.row.height() > 0.0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RowFurniture {
    pub row: Rect,
    pub trailing: Rect,
    pub controls: crate::widget::window_controls::WindowControls,
    pub search: Rect,
    pub new_tab: Rect,
}

impl RowFurniture {
    /// The furniture of the row holding the bar in `slot`. Nothing a row has no room for is
    /// placed, so a narrow window's furniture never runs into what the app draws.
    pub(crate) fn of(slot: WindowBarSlot) -> Self {
        use crate::widget::window_controls::{CONTROL_GAP, CONTROL_PX, LEAD_GAP, WindowControls};
        let trailing = slot.controls;
        let controls = WindowControls::lay_out(trailing);
        if controls.pill.area() <= 0.0 {
            return Self {
                row: slot.row,
                trailing,
                ..Self::default()
            };
        }
        let centred = |height: f64, x0: f64, x1: f64| {
            let y0 = trailing.center().y - height / 2.0;
            Rect::new(x0, y0, x1, y0 + height)
        };
        let controls_end = controls.content_limit(trailing);
        let search = if controls_end - CONTROL_PX >= trailing.x0 {
            centred(CONTROL_PX, controls_end - CONTROL_PX, controls_end)
        } else {
            Rect::ZERO
        };
        let chip = crate::widget::tabs::NEW_TAB_CHIP_PX;
        let chip_end = if search.area() > 0.0 {
            search.x0 - CONTROL_GAP
        } else {
            controls_end - LEAD_GAP
        };
        let new_tab = if chip_end - chip >= trailing.x0 {
            centred(chip, chip_end - chip, chip_end)
        } else {
            Rect::ZERO
        };
        Self {
            row: slot.row,
            trailing,
            controls,
            search,
            new_tab,
        }
    }

    pub(crate) fn place_search(&self, search: &mut crate::widget::WindowSearch) {
        if self.search.area() > 0.0 {
            search.place_trailing(self.search, self.row.x0, self.row.x0);
        } else {
            search.place(Rect::ZERO, 0.0);
        }
    }

    pub(crate) fn shift(&mut self, dx: f64) {
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.row = self.row + shift;
        self.trailing = self.trailing + shift;
        self.controls.pill = self.controls.pill + shift;
        self.controls.minimize = self.controls.minimize + shift;
        self.controls.maximize = self.controls.maximize + shift;
        self.controls.close = self.controls.close + shift;
        self.search = self.search + shift;
        self.new_tab = self.new_tab + shift;
    }

    pub(crate) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        search: &mut crate::widget::WindowSearch,
        state: RowFurnitureState,
    ) {
        self.controls.paint(scene, transform, theme, state.controls);
        if self.new_tab.area() > 0.0 {
            crate::widget::tabs::paint_new_tab(
                scene,
                transform,
                text,
                theme,
                self.new_tab,
                state.new_tab_hovered,
            );
        }
        search.set_ground(theme.tokens.colors.surfaces.content);
        search.draw_child(scene, transform, text, theme, presence);
        if let Some(control) = state.keyboard {
            let rect = control.rect(self);
            if rect.area() > 0.0 {
                let radius = match control {
                    RowControl::NewTab => f64::from(theme.tokens.radii.control),
                    RowControl::Maximize | RowControl::Close => rect.height() / 2.0,
                };
                crate::widget::activation::paint_focus_ring(scene, transform, rect, radius, theme);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RowFurnitureState {
    pub controls: crate::widget::window_controls::ControlsState,
    pub new_tab_hovered: bool,
    pub keyboard: Option<RowControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowControl {
    NewTab,
    Maximize,
    Close,
}

pub(crate) const ROW_CONTROLS: [RowControl; 3] =
    [RowControl::NewTab, RowControl::Maximize, RowControl::Close];

impl RowControl {
    pub(crate) fn rect(self, furniture: &RowFurniture) -> Rect {
        match self {
            RowControl::NewTab => furniture.new_tab,
            RowControl::Maximize => furniture.controls.maximize,
            RowControl::Close => furniture.controls.close,
        }
    }

    pub(crate) fn target(self) -> &'static str {
        match self {
            RowControl::NewTab => crate::widget::tabs::NEW_TAB_TARGET,
            RowControl::Maximize => crate::widget::tabs::WINDOW_MAXIMIZE_TARGET,
            RowControl::Close => crate::widget::tabs::WINDOW_CLOSE_TARGET,
        }
    }

    pub(crate) fn name(self, resize: crate::widget::window_controls::Resize) -> String {
        use crate::widget::window_controls::WindowControl;
        match self {
            RowControl::NewTab => crate::widget::tabs::new_tab_name(),
            RowControl::Maximize => {
                crate::i18n::I18nManager::lookup_plain(WindowControl::Maximize.name_key(resize))
            }
            RowControl::Close => {
                crate::i18n::I18nManager::lookup_plain(WindowControl::Close.name_key(resize))
            }
        }
    }
}

/// The room a header row keeps at its trailing end for the window's furniture
/// ([`RowFurniture`]): the window's own controls, the window's search and its new-tab door, each
/// with the gap the bar keeps between its sections. Every window that lets its row hold the bar
/// reserves this, so its app's own controls end where the window's begin.
///
/// The furniture laid out in a row wider than any window says it: what a row keeps and what stands
/// in it are one arrangement, never two that can drift apart.
#[must_use]
pub fn merged_row_width() -> f64 {
    merged_row_width_in(WIDE_ENOUGH)
}

/// What a row `width` wide keeps at its trailing end for the window's furniture: everything
/// [`merged_row_width`] names while the bar is wider than a phone's, and the window's own controls
/// alone where it is not. A bar that narrow is the app's row, and the room its own controls need
/// there is worth more than the window's search and new-tab door, which such a row draws for
/// itself (its own search control, its own tabs `+`).
#[must_use]
pub fn merged_row_width_in(width: f64) -> f64 {
    let wide = Rect::new(0.0, 0.0, WIDE_ENOUGH, WIDE_ENOUGH);
    let slot = WindowBarSlot {
        row: wide,
        controls: wide,
    };
    let furniture = RowFurniture::of(slot);
    let leading = if merged_row_holds_search(width) {
        furniture.new_tab.x0
    } else {
        furniture.controls.pill.x0
    };
    (wide.x1 - leading).max(0.0)
}

/// Whether a bar `width` wide keeps room for the window's search beside the window's own controls
/// ([`RowFurniture::place_search`]): not where the bar is no wider than a phone's, so a row there
/// keeps its own search control instead ([`WindowHeader::set_window_presents_search`]).
#[must_use]
pub fn merged_row_holds_search(width: f64) -> bool {
    width >= f64::from(crate::tokens::Metrics::BASE.compact_breakpoint)
}

const WIDE_ENOUGH: f64 = 10_000.0;

fn is_window_frame(widget: &dyn Widget) -> bool {
    widget
        .as_any()
        .is_some_and(|any| any.is::<crate::widget::WindowFrame>())
}

pub(crate) fn root_header_row(root: &dyn Widget) -> Option<&dyn WindowHeader> {
    if let Some(row) = super::capability::capability::<dyn WindowHeader>(root) {
        return Some(row);
    }
    if is_window_frame(root) {
        return None;
    }
    root.children().into_iter().find_map(root_header_row)
}

pub(crate) fn offer_root_header_bar(root: &mut dyn Widget, bar: Option<WindowBar>) -> bool {
    if let Some(row) = super::capability::capability_mut::<dyn WindowHeader>(root) {
        return row.set_window_bar(bar);
    }
    if is_window_frame(root) {
        return false;
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_root_header_bar(child, bar))
}

pub(crate) fn window_bar_slot(root: &dyn Widget) -> Option<WindowBarSlot> {
    if let Some(header) = super::capability::capability::<dyn WindowHeader>(root) {
        return header.window_bar_slot();
    }
    root.children().into_iter().find_map(window_bar_slot)
}

pub(crate) fn offer_sync_status(root: &mut dyn Widget, status: Option<&WindowStatus>) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        return header.set_sync_status(status);
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_sync_status(child, status))
}

#[derive(Debug, Default)]
#[crate::component::not_a_component(
    shell,
    "window chrome and titles are the system's (AGENTS.md principle 3)"
)]
pub struct HeaderStatus {
    text: Option<String>,
    rect: Rect,
    dirty: bool,
}

impl HeaderStatus {
    pub fn set(&mut self, text: Option<&str>) -> bool {
        let text = text.map(str::trim).filter(|text| !text.is_empty());
        if self.text.as_deref() == text {
            return false;
        }
        self.text = text.map(str::to_owned);
        self.dirty = true;
        if self.text.is_none() {
            self.rect = Rect::ZERO;
        }
        true
    }

    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        x0: f64,
        x1: f64,
    ) {
        self.rect = Rect::ZERO;
        let Some(status) = self.text.as_deref() else {
            return;
        };
        let style = theme.text(crate::tokens::TextRole::Caption);
        let room = x1 - x0;
        let (_, line) = text.measure_style("Ag", None, &style);
        if room < f64::from(line) * 4.0 {
            return;
        }
        let shown = text.elide_to_width_style(status, crate::num::f32_of_f64(room), &style);
        let (w, h) = text.measure_style(&shown, None, &style);
        let (w, h) = (f64::from(w), f64::from(h));
        let origin = Point::new(x1 - w, row.center().y - h / 2.0);
        text.draw_style(
            scene,
            &shown,
            theme.tokens.colors.text_dim,
            transform.pre_translate(origin.to_vec2()),
            None,
            &style,
        );
        self.rect = Rect::new(origin.x, origin.y, origin.x + w, origin.y + h);
    }
}

impl Widget for HeaderStatus {
    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use super::TaffyTreeExt;
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "HeaderStatus"
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Politeness, Role};
        let text = self.text.as_deref()?;
        Some(
            AccessNode::new(Role::StaticText)
                .with_name(text)
                .with_live(Politeness::Polite),
        )
    }
}

pub(crate) fn offer_history_control(
    root: &mut dyn Widget,
    control: Option<HistoryControl>,
) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        header.set_history_control(control);
        return true;
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_history_control(child, control))
}

pub(crate) fn offer_window_search(root: &mut dyn Widget, presents: bool) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        return header.set_window_presents_search(presents);
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_window_search(child, presents))
}

#[derive(Default)]
pub struct HistoryOpener {
    control: Option<HistoryControl>,
    pending: Option<HistoryControl>,
    hovered: bool,
    pressed: bool,
    pointer: Option<Point>,
    button: Option<super::Button>,
    retired: Vec<super::Button>,
    rebuilt: bool,
}

impl std::fmt::Debug for HistoryOpener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryOpener")
            .field("control", &self.control)
            .field("pending", &self.pending)
            .field("hovered", &self.hovered)
            .finish_non_exhaustive()
    }
}

pub const HISTORY_CONTROL_ID: &str = "window-history";

const HISTORY_TARGET: &str = HISTORY_CONTROL_ID;

impl HistoryOpener {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_control(&mut self, control: Option<HistoryControl>) {
        if self.control != control {
            let next = control.map(|control| {
                crate::history_mode::door::history_control_button(HISTORY_CONTROL_ID, control)
            });
            if let Some(old) = std::mem::replace(&mut self.button, next) {
                self.retired.push(old);
            }
            self.rebuilt = true;
        }
        self.control = control;
        if control.is_none() {
            self.hovered = false;
            self.pressed = false;
        }
    }

    /// The retained History button for a header row that hosts library
    /// controls: lay it out at the row's History rect, keep it among the
    /// host's children, draw it and route its events like the others. Its
    /// press goes through the History door by itself.
    pub fn button(&self) -> Option<&super::Button> {
        self.button.as_ref()
    }

    pub fn button_mut(&mut self) -> Option<&mut super::Button> {
        self.button.as_mut()
    }

    pub fn take_rebuilt(&mut self) -> bool {
        std::mem::take(&mut self.rebuilt)
    }

    pub fn tree_rebuilt(&mut self) {
        self.retired.clear();
    }

    #[must_use]
    pub fn control(&self) -> Option<HistoryControl> {
        self.control
    }

    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.control.is_some()
    }

    pub fn activate(&mut self) {
        if let Some(control) = self.control {
            self.pending = Some(control);
        }
    }

    pub fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let control = self.pending.take()?;
        crate::history_mode::door::history_control_binding(control)
            .invoke(())
            .ok()
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    #[must_use]
    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    pub fn handle_event(&mut self, event: &WindowEvent, rect: Rect, scale_factor: f64) -> bool {
        if self.control.is_none() || rect.width() <= 0.0 {
            self.hovered = false;
            self.pressed = false;
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let scale = if scale_factor > 0.0 {
                    scale_factor
                } else {
                    1.0
                };
                let p = Point::new(position.x / scale, position.y / scale);
                self.pointer = Some(p);
                self.hovered = rect.contains(p);
                false
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered = false;
                self.pointer = None;
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let over = self.pointer.is_some_and(|p| rect.contains(p));
                self.pressed = over;
                over
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let was = std::mem::take(&mut self.pressed);
                let over = self.pointer.is_some_and(|p| rect.contains(p));
                if was && over {
                    self.activate();
                }
                was
            }
            _ => false,
        }
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        if let Some(control) = self.control {
            crate::chrome::paint_history_control(
                scene,
                transform,
                text,
                theme,
                rect,
                control,
                self.hovered,
            );
        }
    }

    #[must_use]
    pub fn accessibility(&self, rect: Rect) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let control = self.control?;
        (rect.width() > 0.0).then(|| {
            AccessNode::new(Role::ToggleButton)
                .with_name(crate::i18n::I18nManager::lookup_plain("history-mode-title"))
                .with_action_target(HISTORY_TARGET)
                .with_bounds(rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(control.is_open()),
                )
        })
    }

    #[must_use]
    pub fn is_target(key_path: &[u64]) -> bool {
        key_path
            .last()
            .is_some_and(|key| *key == crate::a11y::hash_key(&HISTORY_TARGET))
    }

    pub fn perform_semantic_action(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        if target == HISTORY_TARGET && matches!(action, crate::a11y::Action::Activate) {
            self.activate();
            return self.pending.is_some();
        }
        false
    }

    pub fn perform_action(&mut self, action: &crate::a11y::Action, key_path: &[u64]) -> bool {
        if Self::is_target(key_path) && matches!(action, crate::a11y::Action::Activate) {
            self.activate();
            return self.pending.is_some();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_on_the_control_goes_through_the_history_door() {
        let rect = Rect::new(900.0, 12.0, 932.0, 44.0);
        let mut opener = HistoryOpener::new();
        let device = winit::event::DeviceId::dummy();
        let at = |x: f64, y: f64| WindowEvent::CursorMoved {
            device_id: device,
            position: winit::dpi::PhysicalPosition::new(x, y),
        };
        let button = |state| WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Left,
        };
        assert!(!opener.handle_event(&at(910.0, 20.0), rect, 1.0));
        assert!(opener.accessibility(rect).is_none(), "no History, no node");

        opener.set_control(Some(HistoryControl::Closed));
        assert!(
            !opener.handle_event(&at(910.0, 20.0), rect, 1.0),
            "hover is not consumed"
        );
        assert!(opener.is_hovered());
        assert!(opener.handle_event(&button(ElementState::Pressed), rect, 1.0));
        assert!(opener.handle_event(&button(ElementState::Released), rect, 1.0));
        let invocation = opener.take().expect("the press is queued");
        assert_eq!(invocation.address.action, "open");
        assert!(opener.take().is_none(), "once");

        opener.set_control(Some(HistoryControl::Open));
        let node = opener.accessibility(rect).expect("a node");
        assert_eq!(node.state.checked, Some(true));
        assert!(opener.perform_action(&crate::a11y::Action::Activate, &[node.key.expect("keyed")]));
        assert_eq!(opener.take().map(|i| i.address.action), Some("hide".into()));
    }
}
