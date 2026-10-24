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
//! first, who a press's release belongs to, which layer Escape dismisses,
//! the order assistive technology reads open layers in, and whether any of
//! them still needs frames.
//!
//! Input reaches the stack in [`Band`]s, because shell surfaces that are not
//! overlays — a pairing question, first-login onboarding, a portal dialog —
//! sit between them: a display change's Keep/Revert answer comes before
//! onboarding, a tab menu after it. Within a band, [`LayerId`] order decides.
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

use settings_store::hardware::ShortcutAction;
use tilo_desktop::emoji_picker::EmojiPicker;
use tilo_desktop::window_switcher::{WindowAddress, WindowSwitcher, WindowThumbnails};

use super::tab_menu::{TabMenuLayer, TabMenuTarget};
use crate::shell::display_confirm::DisplayConfirmPrompt;
use crate::shell::power_menu::PowerMenu;
use crate::shell::window_menu::WindowMenuAction;

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
    /// Microphone consent and status; preserves the original editable field.
    Dictation,
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
    QuickSettings,
    /// The now-playing card beside the Quick Settings root page.
    NowPlaying,
    ShortcutHelp,
}

impl LayerId {
    pub(crate) const ALL: [Self; 15] = [
        Self::DisplayConfirmation,
        Self::WindowSwitcher,
        Self::Dictation,
        Self::EmojiPicker,
        Self::DisplayLayouts,
        Self::TabContextMenu,
        Self::PowerMenu,
        Self::CommandPalette,
        Self::MemberSheet,
        Self::Overview,
        Self::WorkspaceSettings,
        Self::Collaborators,
        Self::QuickSettings,
        Self::NowPlaying,
        Self::ShortcutHelp,
    ];

    /// Where a stacked layer's input is routed; `None` while the host still
    /// routes the overlay itself. Never decreases down [`Self::ALL`].
    pub(crate) const fn band(self) -> Option<Band> {
        match self {
            Self::DisplayConfirmation => Some(Band::Session),
            Self::WindowSwitcher | Self::Dictation => Some(Band::Chord),
            Self::EmojiPicker => Some(Band::Modal),
            Self::TabContextMenu | Self::PowerMenu => Some(Band::Popover),
            _ => None,
        }
    }
}

/// A point in the host's dispatch where the stack is offered input, topmost
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Band {
    /// Answers the whole session waits on, above onboarding, the personal
    /// display and every desktop surface.
    Session,
    /// Surfaces a held modifier keeps open, above portal dialogs and a drag
    /// that began inside an app.
    Chord,
    /// Modal pickers, below portal dialogs and the file chooser.
    Modal,
    /// Menus and flyouts, just above the windows.
    Popover,
}

/// Where layers may place themselves, in desktop coordinates.
///
/// A surface is placed with [`Self::display_for`], never against
/// [`Self::viewport`]: across a docked laptop the viewport is every display
/// at once, whose centre is the seam and whose bottom edge hangs below the
/// shorter one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerGeometry<'a> {
    /// The host's bounds: the whole desktop, every display.
    pub viewport: Rect,
    /// Every display; the viewport alone before the host knows of any.
    pub outputs: &'a [Rect],
    /// The display with focus on a partitioned desktop.
    pub focused_output: Option<Rect>,
}

impl LayerGeometry<'_> {
    /// The display a surface opens on, the one rule every shell surface is
    /// placed by: the display holding `anchor` (the control, caret or
    /// pointer it was opened from), else the focused one — the display
    /// the person is working on, which follows the pointer across
    /// displays — else the first display, the primary. Never the whole
    /// viewport while there are displays: across two of them it straddles
    /// the seam. The viewport only stands in before any display is known.
    pub(crate) fn display_for(&self, anchor: Option<Rect>) -> Rect {
        anchor
            .and_then(|anchor| {
                self.outputs
                    .iter()
                    .copied()
                    .find(|display| display.contains(anchor.center()))
            })
            .or(self.focused_output)
            .or_else(|| self.outputs.first().copied())
            .unwrap_or(self.viewport)
    }
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
        action: WindowMenuAction,
    },
    /// Tell an agent about the thing the shell's context menu was opened on
    /// ([`TabMenuLayer::take_thing`]).
    TellAgent,
    /// Start a session lifecycle action through the one session executor
    /// that the typed `session.*` actions also reach.
    Session(os_actions::SessionAction),
    /// Focus a window on the active workspace, when it still exists.
    FocusWindow(WindowAddress),
    /// The layer changed what it shows outside a frame the host already
    /// schedules.
    Repaint,
    Dictation(super::dictation_layer::Request),
    /// Open the normal Settings launcher from a missing-model prompt.
    DictationSetup,
}

/// What a layer may read and ask for while handling one event.
pub(crate) struct LayerCx<'a, 'e> {
    pub geometry: LayerGeometry<'a>,
    /// Last known pointer position; `MouseInput` carries none.
    pub cursor: Option<Point>,
    /// The configured shortcut a key press is bound to; `None` for every
    /// other event.
    pub shortcut: Option<ShortcutAction>,
    pub event: &'a mut EventCtx<'e>,
    intents: Vec<LayerIntent>,
}

impl<'a, 'e> LayerCx<'a, 'e> {
    pub(crate) fn new(
        geometry: LayerGeometry<'a>,
        cursor: Option<Point>,
        event: &'a mut EventCtx<'e>,
    ) -> Self {
        Self {
            geometry,
            cursor,
            shortcut: None,
            event,
            intents: Vec::new(),
        }
    }

    pub(crate) fn with_shortcut(mut self, shortcut: Option<ShortcutAction>) -> Self {
        self.shortcut = shortcut;
        self
    }

    pub(crate) fn request(&mut self, intent: LayerIntent) {
        self.intents.push(intent);
    }

    pub(crate) fn into_intents(self) -> Vec<LayerIntent> {
        self.intents
    }
}

pub(crate) struct LayerPaint<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text_renderer: &'a mut TextRenderer,
    pub theme: &'a Theme,
    pub presence: &'a PresenceCtx,
    pub geometry: LayerGeometry<'a>,
    /// Live pictures of the desktop's windows, rendered by the compositor
    /// while something shows them.
    pub thumbnails: &'a mut dyn WindowThumbnails,
}

/// One self-contained shell overlay.
pub(crate) trait ShellLayer {
    fn id(&self) -> LayerId;

    fn is_open(&self) -> bool;

    /// On screen, and so published to assistive technology. A layer may own
    /// input a moment before it is drawn.
    fn is_shown(&self) -> bool {
        self.is_open()
    }

    /// Only called while open. A layer that consumes a press owns its
    /// release: it receives it while still open, and the stack absorbs it
    /// when the press closed the layer.
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed;

    /// Called every frame, open or not, so a closing layer can animate out.
    fn draw(&mut self, cx: &mut LayerPaint<'_>);

    /// The layer's subtree while open; its children's indices are the
    /// semantic paths [`Self::accessibility_rect`] and
    /// [`Self::focus_accessibility`] receive.
    fn accessibility(&self, geometry: LayerGeometry<'_>) -> Option<AccessNode>;

    fn accessibility_rect(&self, geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect>;

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

    /// Advance state that moves with time rather than input, once per
    /// frame; `true` when that changed what the layer shows.
    fn tick(&mut self, _now: Instant) -> bool {
        false
    }
}

/// The overlays that have joined the stack, and the arbitration they share.
#[derive(Default)]
pub(crate) struct OverlayStack {
    pub(crate) display_confirm: DisplayConfirmPrompt,
    pub(crate) window_switcher: WindowSwitcher,
    pub(crate) dictation: super::dictation_layer::DictationSurface,
    pub(crate) emoji_picker: EmojiPicker,
    pub(crate) tab_menu: TabMenuLayer,
    pub(crate) power_menu: PowerMenu,
    /// The layer that consumed a press whose release has not arrived yet.
    release_owner: Option<LayerId>,
}

impl OverlayStack {
    /// Topmost first; must follow [`LayerId`]'s order.
    fn layers(&self) -> [&dyn ShellLayer; 6] {
        [
            &self.display_confirm,
            &self.window_switcher,
            &self.dictation,
            &self.emoji_picker,
            &self.tab_menu,
            &self.power_menu,
        ]
    }

    fn layers_mut(&mut self) -> [&mut dyn ShellLayer; 6] {
        [
            &mut self.display_confirm,
            &mut self.window_switcher,
            &mut self.dictation,
            &mut self.emoji_picker,
            &mut self.tab_menu,
            &mut self.power_menu,
        ]
    }

    pub(crate) fn any_shown(&self) -> bool {
        self.layers().into_iter().any(ShellLayer::is_shown)
    }

    /// The stacked layer for `id`, or `None` while the host still routes it.
    pub(crate) fn layer(&self, id: LayerId) -> Option<&dyn ShellLayer> {
        self.layers().into_iter().find(|layer| layer.id() == id)
    }

    pub(crate) fn layer_mut(&mut self, id: LayerId) -> Option<&mut dyn ShellLayer> {
        self.layers_mut().into_iter().find(|layer| layer.id() == id)
    }

    /// Whether [`Self::route`] could resolve anything in `band`: a layer in
    /// it is open or still owed a release.
    pub(crate) fn listens(&self, band: Band) -> bool {
        self.release_owner.and_then(LayerId::band) == Some(band)
            || self
                .layers()
                .into_iter()
                .any(|layer| layer.id().band() == Some(band) && layer.is_open())
    }

    /// Whether a menu or flyout ([`Band::Popover`]) is open. Such a layer
    /// hangs over application windows and models its own hover, so while
    /// it is up the shell must own the seat: otherwise motion over the part
    /// covering a window goes to that window, and the menu's next press is
    /// judged against a stale pointer.
    pub(crate) fn popover_open(&self) -> bool {
        self.layers()
            .into_iter()
            .any(|layer| layer.id().band() == Some(Band::Popover) && layer.is_open())
    }

    /// Offer `event` to the open layers in `band`, topmost first. `true`
    /// when one of them resolved it; `cx.event.handled` is then set.
    pub(crate) fn route(
        &mut self,
        band: Band,
        event: &WindowEvent,
        cx: &mut LayerCx<'_, '_>,
    ) -> bool {
        let button = match event {
            WindowEvent::MouseInput { state, .. } => Some(*state),
            _ => None,
        };
        if button == Some(ElementState::Released)
            && let Some(owner) = self
                .release_owner
                .filter(|owner| owner.band() == Some(band))
        {
            self.release_owner = None;
            if let Some(layer) = self.layer_mut(owner).filter(|layer| layer.is_open()) {
                layer.handle_event(event, cx);
            }
            *cx.event.handled = true;
            return true;
        }
        let consumer = self
            .layers_mut()
            .into_iter()
            .filter(|layer| layer.id().band() == Some(band) && layer.is_open())
            .find_map(|layer| {
                (layer.handle_event(event, cx) == Routed::Consumed).then(|| layer.id())
            });
        let Some(consumer) = consumer else {
            return false;
        };
        if button == Some(ElementState::Pressed) {
            self.release_owner = Some(consumer);
        }
        *cx.event.handled = true;
        true
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

    /// Advance every layer's clock; `true` when any of them changed.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        self.layers_mut()
            .into_iter()
            .fold(false, |changed, layer| layer.tick(now) | changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use taffy::TaffyTree;
    use winit::event::MouseButton;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    fn geometry() -> LayerGeometry<'static> {
        LayerGeometry {
            viewport: VIEWPORT,
            outputs: &[VIEWPORT],
            focused_output: None,
        }
    }

    fn button(state: ElementState) -> WindowEvent {
        ui::synthetic_input::mouse_input(state, MouseButton::Left)
    }

    /// Route `event` to `band` with the pointer at `cursor`;
    /// `(consumed, handled)`.
    fn route_band(
        stack: &mut OverlayStack,
        band: Band,
        event: &WindowEvent,
        cursor: Point,
    ) -> (bool, bool) {
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(taffy::Style::default()).expect("leaf");
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut event_cx = EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(&mut doc),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        };
        let mut cx = LayerCx::new(geometry(), Some(cursor), &mut event_cx);
        let consumed = stack.route(band, event, &mut cx);
        assert!(
            cx.into_intents().is_empty(),
            "a dismissal asks nothing of the host"
        );
        (consumed, handled)
    }

    fn route(stack: &mut OverlayStack, event: &WindowEvent, cursor: Point) -> (bool, bool) {
        route_band(stack, Band::Popover, event, cursor)
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
    fn a_layer_is_routed_in_a_band_exactly_when_it_is_stacked() {
        let stack = OverlayStack::default();
        for id in LayerId::ALL {
            assert_eq!(id.band().is_some(), stack.layer(id).is_some(), "{id:?}");
        }
        let bands: Vec<Band> = LayerId::ALL.iter().filter_map(|id| id.band()).collect();
        assert!(
            bands.windows(2).all(|pair| pair[0] <= pair[1]),
            "a layer higher in the stack is never offered input after a lower one: {bands:?}"
        );
    }

    #[test]
    fn a_release_is_owed_only_in_the_band_that_took_its_press() {
        let mut stack = OverlayStack::default();
        stack.power_menu.open();
        let outside = Point::new(VIEWPORT.x0 + 4.0, VIEWPORT.y1 - 4.0);
        assert_eq!(
            route(&mut stack, &button(ElementState::Pressed), outside),
            (true, true)
        );
        assert!(
            !stack.power_menu.is_open(),
            "the press dismissed the flyout"
        );
        assert!(stack.listens(Band::Popover), "its release is still owed");
        for band in [Band::Session, Band::Chord, Band::Modal] {
            assert!(!stack.listens(band), "{band:?}");
            assert_eq!(
                route_band(&mut stack, band, &button(ElementState::Released), outside),
                (false, false),
                "{band:?} sits above the surfaces between it and the flyout"
            );
        }
        assert_eq!(
            route(&mut stack, &button(ElementState::Released), outside),
            (true, true)
        );
        assert!(!stack.listens(Band::Popover), "nothing is open or owed");
    }

    #[test]
    fn a_popup_is_placed_on_the_display_holding_its_anchor() {
        let left = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let right = Rect::new(1280.0, 0.0, 3840.0, 1440.0);
        let viewport = left.union(right);
        let caret = |x: f64| Some(Rect::new(x, 100.0, x + 2.0, 120.0));
        let mut geometry = LayerGeometry {
            viewport,
            outputs: &[left, right],
            focused_output: None,
        };
        assert_eq!(geometry.display_for(caret(2000.0)), right);
        assert_eq!(
            geometry.display_for(None),
            left,
            "with no display focused, the first (primary) display: never the \
             whole viewport, which straddles the seam"
        );
        geometry.focused_output = Some(right);
        assert_eq!(geometry.display_for(caret(10.0)), left);
        assert_eq!(geometry.display_for(caret(9000.0)), right);
        assert_eq!(geometry.display_for(None), right);
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
