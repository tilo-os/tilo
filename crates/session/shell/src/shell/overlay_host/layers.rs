//! The shell's overlay stack.
//!
//! An overlay the shell paints above the desktop — a menu, sheet, picker or
//! prompt — is a [`ShellLayer`]: it owns its state, paint, hit-testing,
//! keyboard model and accessibility subtree, and asks the host for anything
//! beyond itself by requesting a [`LayerIntent`]. It never reaches into the
//! host.
//!
//! [`OverlayStack`] owns the decisions every overlay otherwise re-derives:
//! the stacking order ([`LayerId`]), which open layer an event is offered to
//! first, the release a consumed press still owes, which layer Escape
//! dismisses, the order assistive technology reads open layers in, and
//! whether any of them still needs animation frames.
//!
//! [`LayerId`] names every shell overlay, including those the host still
//! routes itself; [`OverlayStack::layer`] answers `None` for those.

use std::time::Instant;

use ui::a11y::AccessNode;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::EventCtx;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};

use super::tab_menu::{TabMenuLayer, TabMenuTarget};
use crate::shell::TabMenuAction;
use crate::shell::power_menu::PowerMenu;

/// Every shell overlay, topmost first. Declaration order *is* the stacking
/// order: input is offered to open layers in it, assistive technology reads
/// them in it (a layer's index among the open ones is its semantic path
/// root), and Escape dismisses the first open one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LayerId {
    /// Keep or revert a display tool's change; above everything while up.
    DisplayConfirmation,
    /// The Alt+Tab switcher, while Alt is held and it is on screen. It owns
    /// the keyboard until Alt comes up.
    WindowSwitcher,
    /// The emoji picker; modal while open.
    EmojiPicker,
    /// The Super+P display sheet.
    DisplayLayouts,
    /// The tab right-click menu: the topmost transient, whose rows are what
    /// a reader should land on.
    TabContextMenu,
    /// The session power flyout.
    PowerMenu,
    CommandPalette,
    MemberSheet,
    Overview,
    WorkspaceSettings,
    Collaborators,
    RoomActivity,
    QuickSettings,
    /// The now-playing card beside the Quick Settings root page.
    NowPlaying,
    ShortcutHelp,
}

impl LayerId {
    pub(crate) const ALL: [Self; 15] = [
        Self::DisplayConfirmation,
        Self::WindowSwitcher,
        Self::EmojiPicker,
        Self::DisplayLayouts,
        Self::TabContextMenu,
        Self::PowerMenu,
        Self::CommandPalette,
        Self::MemberSheet,
        Self::Overview,
        Self::WorkspaceSettings,
        Self::Collaborators,
        Self::RoomActivity,
        Self::QuickSettings,
        Self::NowPlaying,
        Self::ShortcutHelp,
    ];
}

/// Whether a layer resolved an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Routed {
    /// Offer the event to the next layer down, then to the desktop.
    Pass,
    /// The event stops here and is marked handled.
    Consumed,
}

/// Something a layer needs done outside itself. The host carries each one
/// out as soon as routing returns, before the event it came from finishes
/// dispatching, and reports the outcome back to the layer that asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayerIntent {
    /// Run a tab-menu row against the tab it was opened for; the host
    /// answers with [`TabMenuLayer::settle`].
    TabMenu {
        target: TabMenuTarget,
        action: TabMenuAction,
    },
    /// Start a session lifecycle action through the one session executor
    /// that the typed `session.*` actions also reach.
    Session(os_actions::SessionAction),
}

/// What a layer may read and ask for while handling one event.
pub(crate) struct LayerCx<'a, 'e> {
    /// The host's bounds, which every layer places itself within.
    pub viewport: Rect,
    /// Last known pointer position; `MouseInput` carries none.
    pub cursor: Option<Point>,
    pub event: &'a mut EventCtx<'e>,
    intents: Vec<LayerIntent>,
}

impl<'a, 'e> LayerCx<'a, 'e> {
    pub(crate) fn new(viewport: Rect, cursor: Option<Point>, event: &'a mut EventCtx<'e>) -> Self {
        Self {
            viewport,
            cursor,
            event,
            intents: Vec::new(),
        }
    }

    pub(crate) fn request(&mut self, intent: LayerIntent) {
        self.intents.push(intent);
    }

    pub(crate) fn into_intents(self) -> Vec<LayerIntent> {
        self.intents
    }
}

/// What a layer paints with.
pub(crate) struct LayerPaint<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text_renderer: &'a mut TextRenderer,
    pub theme: &'a Theme,
    pub presence: &'a PresenceCtx,
    pub viewport: Rect,
}

/// One self-contained shell overlay.
pub(crate) trait ShellLayer {
    fn id(&self) -> LayerId;

    fn is_open(&self) -> bool;

    /// Only called while open. A layer that consumes a press does not see
    /// its release: the stack absorbs that on the layer's behalf.
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed;

    /// Called every frame, open or not, so a closing layer can animate out.
    fn draw(&mut self, cx: &mut LayerPaint<'_>);

    /// The layer's subtree while open; its children's indices are the
    /// semantic paths [`Self::accessibility_rect`] and
    /// [`Self::focus_accessibility`] receive.
    fn accessibility(&self, viewport: Rect) -> Option<AccessNode>;

    fn accessibility_rect(&self, viewport: Rect, path: &[usize]) -> Option<Rect>;

    fn focus_accessibility(&mut self, path: &[usize]) -> bool;

    /// The intent activating semantic `target` stands for, when this open
    /// layer publishes it.
    fn semantic_intent(&self, _target: &str) -> Option<LayerIntent> {
        None
    }

    /// Close in response to Escape; `true` when the layer was open.
    fn dismiss(&mut self) -> bool;

    fn is_animating(&self, _now: Instant) -> bool {
        false
    }
}

/// The overlays that have joined the stack, and the arbitration they share.
#[derive(Default)]
pub(crate) struct OverlayStack {
    pub(crate) tab_menu: TabMenuLayer,
    pub(crate) power_menu: PowerMenu,
    /// A layer consumed a press whose release has not arrived yet.
    owed_release: bool,
}

impl OverlayStack {
    /// Topmost first; must follow [`LayerId`]'s order.
    fn layers(&self) -> [&dyn ShellLayer; 2] {
        [&self.tab_menu, &self.power_menu]
    }

    fn layers_mut(&mut self) -> [&mut dyn ShellLayer; 2] {
        [&mut self.tab_menu, &mut self.power_menu]
    }

    /// The stacked layer for `id`, or `None` while the host still routes it.
    pub(crate) fn layer(&self, id: LayerId) -> Option<&dyn ShellLayer> {
        self.layers().into_iter().find(|layer| layer.id() == id)
    }

    pub(crate) fn layer_mut(&mut self, id: LayerId) -> Option<&mut dyn ShellLayer> {
        self.layers_mut().into_iter().find(|layer| layer.id() == id)
    }

    /// Offer `event` to the open layers, topmost first. `true` when one of
    /// them resolved it; `cx.event.handled` is then set.
    pub(crate) fn route(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> bool {
        let button = match event {
            WindowEvent::MouseInput { state, .. } => Some(*state),
            _ => None,
        };
        if button == Some(ElementState::Released) && std::mem::take(&mut self.owed_release) {
            *cx.event.handled = true;
            return true;
        }
        let consumed = self
            .layers_mut()
            .into_iter()
            .filter(|layer| layer.is_open())
            .any(|layer| layer.handle_event(event, cx) == Routed::Consumed);
        if consumed {
            self.owed_release |= button == Some(ElementState::Pressed);
            *cx.event.handled = true;
        }
        consumed
    }

    pub(crate) fn draw(&mut self, id: LayerId, cx: &mut LayerPaint<'_>) {
        if let Some(layer) = self.layer_mut(id) {
            layer.draw(cx);
        }
    }

    /// The intent behind activating semantic `target` on an open layer.
    pub(crate) fn semantic_intent(&self, target: &str) -> Option<LayerIntent> {
        self.layers()
            .into_iter()
            .filter(|layer| layer.is_open())
            .find_map(|layer| layer.semantic_intent(target))
    }

    /// Escape: close the topmost open layer.
    pub(crate) fn dismiss_topmost(&mut self) -> bool {
        self.layers_mut()
            .into_iter()
            .filter(|layer| layer.is_open())
            .any(ShellLayer::dismiss)
    }

    pub(crate) fn is_animating(&self, now: Instant) -> bool {
        self.layers()
            .into_iter()
            .any(|layer| layer.is_animating(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use taffy::TaffyTree;
    use winit::event::{DeviceId, MouseButton};

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    fn button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }

    /// Route `event` with the pointer at `cursor`; `(consumed, handled)`.
    fn route(stack: &mut OverlayStack, event: &WindowEvent, cursor: Point) -> (bool, bool) {
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(taffy::Style::default()).expect("leaf");
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut event_cx = EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: &mut doc,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        };
        let mut cx = LayerCx::new(VIEWPORT, Some(cursor), &mut event_cx);
        let consumed = stack.route(event, &mut cx);
        assert!(
            cx.into_intents().is_empty(),
            "a dismissal asks nothing of the host"
        );
        (consumed, handled)
    }

    #[test]
    fn stacked_layers_follow_the_declared_stacking_order() {
        let stack = OverlayStack::default();
        let ids: Vec<LayerId> = stack.layers().iter().map(|layer| layer.id()).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
        for id in &ids {
            assert_eq!(stack.layer(*id).map(ShellLayer::id), Some(*id));
        }
        assert!(stack.layer(LayerId::CommandPalette).is_none());
    }

    #[test]
    fn a_dismissing_press_owes_exactly_its_own_release() {
        let mut stack = OverlayStack::default();
        stack.power_menu.open();
        let outside = Point::new(VIEWPORT.x0 + 4.0, VIEWPORT.y1 - 4.0);

        assert_eq!(
            route(&mut stack, &button(ElementState::Pressed), outside),
            (true, true),
            "the press outside the flyout dismisses it and stops there"
        );
        assert!(!stack.power_menu.is_open());
        assert_eq!(
            route(&mut stack, &button(ElementState::Released), outside),
            (true, true),
            "its release must not reach the window beneath"
        );
        assert_eq!(
            route(&mut stack, &button(ElementState::Released), outside),
            (false, false),
            "only the one release is owed"
        );
    }

    #[test]
    fn escape_dismisses_only_an_open_layer() {
        let mut stack = OverlayStack::default();
        assert!(!stack.dismiss_topmost());
        stack.power_menu.open();
        assert!(stack.dismiss_topmost());
        assert!(!stack.power_menu.is_open());
        assert!(!stack.dismiss_topmost());
    }
}
