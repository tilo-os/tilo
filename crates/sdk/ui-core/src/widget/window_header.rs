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

/// What the window's History control shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryControl {
    /// History is closed; the control opens it.
    Closed,
    /// History is showing; the control reads as on and closes it.
    Open,
}

/// What the window says about its changes in the header row: quiet text
/// ("Working offline…"), and the one thing the person can do about it when
/// there is one (Retry), as a button beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowStatus {
    pub text: String,
    pub action: Option<StatusAction>,
}

impl WindowStatus {
    /// A status that only says something.
    #[must_use]
    pub fn says(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            action: None,
        }
    }

    /// The status, with a button beside it.
    #[must_use]
    pub fn with_action(mut self, action: StatusAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// The button beside a window's status: its label and the window's own
/// action it runs, the same one the palette, menus and assistive technology
/// reach.
#[derive(Clone)]
pub struct StatusAction {
    pub label: String,
    pub binding: tilo_actions::ActionBinding<()>,
}

impl StatusAction {
    /// The button, from the design system's components.
    #[must_use]
    pub fn button(&self) -> super::Button {
        super::Button::new(STATUS_ACTION_ID, &self.label)
            .control_size(super::ControlSize::Small)
            .on_action(self.binding.clone())
    }
}

/// The test id of the button beside a window's status.
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

/// An app that runs a History of its own inside a desktop window (an app
/// shown in a region of the host's scene). The window presents its History
/// control in the tab row, as one more control of the window, so the app
/// draws no History band of its own.
pub trait WindowHistory {
    /// The window's tab row shows (`true`) and presents the control, or is
    /// hidden, so the app's header row may hold it instead.
    fn set_window_presents_history(&mut self, tab_presents: bool);

    /// The control to show for this app's History, or `None` where it has
    /// no History to open.
    fn window_history_control(&self) -> Option<HistoryControl>;

    /// The window's History control was pressed: run it on this app's
    /// History door, as the History shortcut does.
    ///
    /// # Errors
    ///
    /// The app has no History, or its door refused the operation.
    fn invoke_window_history_control(&mut self, control: HistoryControl) -> Result<(), String>;
}

/// A widget that draws the window's header row.
pub trait WindowHeader {
    /// Show the window's History control at the trailing edge of the row,
    /// or none (`None`: the app has no History, or the host's own chrome
    /// presents it).
    fn set_history_control(&mut self, control: Option<HistoryControl>);

    /// The window's bar presents search for the window, in place
    /// ([`crate::widget::WindowSearch`]), or stops (`false`). While it does,
    /// the row drops its own button that opens the app's search
    /// ([`crate::search::APP_SEARCH_BUTTON_ID`]): one search control per
    /// window. Returns whether the row took it.
    fn set_window_presents_search(&mut self, presents: bool) -> bool {
        let _ = presents;
        false
    }

    /// Show the window's sync status ("Working offline…") as quiet text in
    /// the row, before its trailing controls, with its button (Retry) beside
    /// it when it has one, or none. Returns whether the row shows it; a row
    /// that does not leaves the host to present it.
    fn set_sync_status(&mut self, status: Option<&WindowStatus>) -> bool {
        let _ = status;
        false
    }

    /// Merge the window's bar into this header row while the window is
    /// maximized, or stop (`None`). The row reserves `bar.controls_width` at
    /// its trailing end for the window's controls, which the host paints
    /// there when the person reaches for them, so the app's header and the
    /// window's controls read as one bar instead of two stacked ones. A row
    /// whose app put its header in the window's bar
    /// ([`WindowBarSlot::hides_with_bar`]) shows only while `bar.revealed`,
    /// over the app's content. Returns whether the row took the bar; a row
    /// that does not leaves the host to float its controls over the window.
    fn set_window_bar(&mut self, bar: Option<WindowBar>) -> bool {
        let _ = bar;
        false
    }

    /// Where the row stands and where the window's controls go, after
    /// layout, while it holds the window's bar.
    fn window_bar_slot(&self) -> Option<WindowBarSlot> {
        None
    }
}

/// The window's bar, as the host offers it to a header row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBar {
    /// Room the window's controls take at the row's trailing end.
    pub controls_width: f64,
    /// Whether the person reached for the bar
    /// ([`crate::widget::TopEdgeReveal`]).
    pub revealed: bool,
    /// How far below the window's top edge the bar stands: the height of
    /// what overlays that edge now (the desktop's top bar, while it shows
    /// over a window under an auto-hiding top bar), so the two stack instead
    /// of one covering the other. A row whose header lives in the bar moves
    /// down by it; a permanent row stays put.
    pub top_inset: f64,
}

/// A header row holding the window's bar, after layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBarSlot {
    /// The bar: the header row across the window's top edge, where it stands
    /// when shown (also while hidden, so the host knows where it will be).
    pub row: Rect,
    /// Where the window's controls go: the trailing end of the row,
    /// `controls_width` wide and the row's height.
    pub controls: Rect,
    /// The app put its header in the window's bar: it shows only with the
    /// bar, over the content, instead of standing above it.
    pub hides_with_bar: bool,
}

/// Offer the window's bar to the first header row in `root`'s tree. Returns
/// whether a row took it.
pub(crate) fn offer_window_bar(root: &mut dyn Widget, bar: Option<WindowBar>) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        return header.set_window_bar(bar);
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_window_bar(child, bar))
}

/// The slot of the first header row in `root`'s tree that holds the bar.
pub(crate) fn window_bar_slot(root: &dyn Widget) -> Option<WindowBarSlot> {
    if let Some(header) = super::capability::capability::<dyn WindowHeader>(root) {
        return header.window_bar_slot();
    }
    root.children().into_iter().find_map(window_bar_slot)
}

/// Hand the window's sync status to the first header row in `root`'s tree.
/// Returns whether a row shows it.
pub(crate) fn offer_sync_status(root: &mut dyn Widget, status: Option<&WindowStatus>) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        return header.set_sync_status(status);
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_sync_status(child, status))
}

/// The window's sync status in a header row: quiet text, right-aligned
/// against the row's trailing controls, announced politely when it changes.
/// A header row keeps one, paints it into the room its own layout leaves
/// ([`Self::paint`]) and lists it among its children for assistive
/// technology.
#[derive(Debug, Default)]
pub struct HeaderStatus {
    text: Option<String>,
    rect: Rect,
}

impl HeaderStatus {
    /// Set what the status says. Returns whether it changed.
    pub fn set(&mut self, text: Option<&str>) -> bool {
        let text = text.map(str::trim).filter(|text| !text.is_empty());
        if self.text.as_deref() == text {
            return false;
        }
        self.text = text.map(str::to_owned);
        if self.text.is_none() {
            self.rect = Rect::ZERO;
        }
        true
    }

    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Paint the status right-aligned in `x0..x1` of `row`, vertically
    /// centred, elided to fit. Paints nothing when the span is too narrow
    /// to say anything legible.
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

/// Hand `control` to the first header row in `root`'s tree. Returns whether
/// the app has one; the host then draws no History control of its own.
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

/// Tell the first header row in `root`'s tree whether the window's bar
/// presents search ([`WindowHeader::set_window_presents_search`]). Returns
/// whether a row took it.
pub(crate) fn offer_window_search(root: &mut dyn Widget, presents: bool) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        return header.set_window_presents_search(presents);
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_window_search(child, presents))
}

/// The window's History control in a painted header row: what the host
/// offered, the pointer's hover over it, and a press waiting to go through
/// the History door. Modelled on [`crate::search::SearchOpener`]: return
/// [`Self::take`] from `ActionControls::take_control_invocation`.
///
/// A painted header row paints the control ([`Self::paint`]) and routes its
/// pointer and its semantic node through the opener. A header row that
/// hosts retained controls (a bar of library buttons) hosts
/// [`Self::button_mut`] instead: the same History button `NavShell` shows,
/// bound to the History door, at [`HISTORY_CONTROL_ID`].
#[derive(Default)]
pub struct HistoryOpener {
    control: Option<HistoryControl>,
    pending: Option<HistoryControl>,
    hovered: bool,
    pressed: bool,
    pointer: Option<Point>,
    button: Option<super::Button>,
    /// Buttons replaced since the host last rebuilt its tree: kept alive
    /// until then, since the tree may still point at them.
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

/// The id of the window's History control in a painted header row: its
/// test id, and the action target of its semantic node.
pub const HISTORY_CONTROL_ID: &str = "window-history";

/// The accessibility target of the control's node.
const HISTORY_TARGET: &str = HISTORY_CONTROL_ID;

impl HistoryOpener {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the host offered (call from [`WindowHeader::set_history_control`]).
    pub fn set_control(&mut self, control: Option<HistoryControl>) {
        if self.control != control {
            let next = control.map(|control| {
                crate::standalone_history_host::history_control_button(HISTORY_CONTROL_ID, control)
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

    /// Whether the button was replaced since the last call: the host
    /// rebuilds its tree. Call [`Self::tree_rebuilt`] once it has.
    pub fn take_rebuilt(&mut self) -> bool {
        std::mem::take(&mut self.rebuilt)
    }

    /// The host rebuilt its tree: replaced buttons may now be dropped.
    pub fn tree_rebuilt(&mut self) {
        self.retired.clear();
    }

    /// The control to show, or `None` when the row has no History control.
    #[must_use]
    pub fn control(&self) -> Option<HistoryControl> {
        self.control
    }

    /// Whether the row shows the control.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.control.is_some()
    }

    /// Press the control: open History, or close it while it shows.
    pub fn activate(&mut self) {
        if let Some(control) = self.control {
            self.pending = Some(control);
        }
    }

    /// The press as an invocation of the History door, once.
    pub fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let control = self.pending.take()?;
        crate::standalone_history_host::history_control_binding(control)
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

    /// Route `event` to the control at `rect` (its
    /// [`HeaderRowFace::history_rect`](crate::chrome::HeaderRowFace::history_rect)).
    /// Returns whether the control took it (a press on it, and the release
    /// that follows; pointer motion only updates the hover); a release over
    /// the control after a press on it queues the press.
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

    /// Paint the control at `rect`.
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

    /// The control's node for the window's semantic tree: a toggle button
    /// named "History", pressed while History shows. Route an `Activate` on
    /// it back with [`Self::perform_action`].
    #[must_use]
    pub fn accessibility(&self, rect: Rect) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let control = self.control?;
        (rect.width() > 0.0).then(|| {
            AccessNode::new(Role::ToggleButton)
                .with_name(crate::i18n::I18nManager::lookup_plain(
                    "standalone-history-title",
                ))
                .with_action_target(HISTORY_TARGET)
                .with_bounds(rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(control.is_open()),
                )
        })
    }

    /// Whether `key_path` (from `Widget::perform_action`) addresses the
    /// node [`Self::accessibility`] published.
    #[must_use]
    pub fn is_target(key_path: &[u64]) -> bool {
        key_path
            .last()
            .is_some_and(|key| *key == crate::a11y::hash_key(&HISTORY_TARGET))
    }

    /// Activate the control from its node addressed by its action target
    /// (`Widget::perform_semantic_action`); returns whether it did.
    pub fn perform_semantic_action(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        if target == HISTORY_TARGET && matches!(action, crate::a11y::Action::Activate) {
            self.activate();
            return self.pending.is_some();
        }
        false
    }

    /// Activate the control from its node; returns whether it did.
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
