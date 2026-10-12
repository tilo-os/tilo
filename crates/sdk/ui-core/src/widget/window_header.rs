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

/// A widget that draws the window's header row.
pub trait WindowHeader {
    /// Show the window's History control at the trailing edge of the row,
    /// or none (`None`: the app has no History, or the host's own chrome
    /// presents it).
    fn set_history_control(&mut self, control: Option<HistoryControl>);
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
                .with_state(AccessState {
                    focusable: true,
                    checked: Some(control.is_open()),
                    ..Default::default()
                })
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
