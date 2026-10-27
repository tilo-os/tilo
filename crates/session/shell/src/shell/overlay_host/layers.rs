//! The shell's overlay stack.
//!
//! An overlay the shell paints above the desktop — a menu, sheet, picker or
//! prompt — is a [`ShellLayer`]: it owns its state, paint, hit-testing,
//! keyboard model and accessibility subtree, and asks the host for anything
//! beyond itself by requesting a [`LayerIntent`]. It never reaches into the
//! host. What kind of surface it is, what it covers, who may use it and who
//! owns it are [`ShellLayer::surface`]'s description, not the host's
//! knowledge; `docs/architecture/surfaces.md` is the contract.
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

    /// The kind of surface `self` is, for a layer whose state is not a
    /// [`ShellLayer`] yet: the host still routes these itself (the migration
    /// in `docs/architecture/surfaces.md`). A layer that implements
    /// [`ShellLayer`] answers [`ShellLayer::surface`] instead, and the two
    /// must agree — the stack's tests hold them to it.
    pub(crate) const fn surface_kind(self) -> Surface {
        match self {
            Self::DisplayConfirmation => Surface::shell(
                Role::Consent,
                Anchor::Fill,
                Modality::SystemModal,
                Material::Floating,
                false,
            ),
            Self::WindowSwitcher => Surface::shell(
                Role::Overlay,
                Anchor::Fill,
                Modality::WindowModal,
                Material::SeeThrough,
                false,
            ),
            Self::Dictation => Surface::shell(
                Role::Overlay,
                Anchor::BottomCenter(None),
                Modality::WindowModal,
                Material::SeeThrough,
                false,
            ),
            Self::EmojiPicker => Surface::shell(
                Role::Overlay,
                Anchor::Center,
                Modality::WindowModal,
                Material::Floating,
                false,
            ),
            Self::DisplayLayouts | Self::WorkspaceSettings | Self::MemberSheet => Surface::shell(
                Role::Dialog,
                Anchor::Center,
                Modality::SystemModal,
                Material::Floating,
                false,
            ),
            Self::TabContextMenu => Surface::shell(
                Role::Popup,
                Anchor::At(Point::ZERO),
                Modality::WindowModal,
                Material::Floating,
                false,
            ),
            Self::PowerMenu => Surface::shell(
                Role::Popup,
                Anchor::TopTrailing(0.0),
                Modality::WindowModal,
                Material::Floating,
                false,
            ),
            Self::CommandPalette | Self::Overview => Surface::shell(
                Role::Overlay,
                Anchor::Center,
                Modality::WindowModal,
                Material::Floating,
                false,
            ),
            Self::Collaborators | Self::QuickSettings | Self::NowPlaying => Surface::shell(
                Role::Dialog,
                Anchor::TopTrailing(0.0),
                Modality::Passive,
                Material::Floating,
                false,
            ),
            Self::ShortcutHelp => Surface::shell(
                Role::Dialog,
                Anchor::Center,
                Modality::SystemModal,
                Material::Floating,
                false,
            ),
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

impl Band {
    /// The band a surface's input is offered in, from its modality alone:
    /// a surface that absorbs the whole system answers before anything
    /// else, one that holds a window above the pickers, and a popup above
    /// the windows. `None` for a surface that is not on screen, which takes
    /// no input at all.
    pub(crate) const fn of(lifecycle: Lifecycle, modality: Modality) -> Option<Self> {
        if !lifecycle.takes_input() {
            return None;
        }
        Some(match modality {
            Modality::SystemModal => Self::Session,
            Modality::WindowModal => Self::Modal,
            Modality::Passive => Self::Popover,
        })
    }
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

/// The surface model's vocabulary. Every surface names its kind, placement,
/// modality, owner and life, and the host derives the band its input is
/// offered in ([`Band::of`]), whether and what it makes inert
/// ([`Surface::scrim`]), the display it stands on ([`Surface::display`]) and
/// the material it is painted on, instead of asking which overlay it is.
/// The contract, and what each of the shell's surfaces says, is
/// `docs/architecture/surfaces.md`.
///
/// What a surface is for: the kind of thing the person sees standing over
/// the desktop. The kind decides the band its input is offered in
/// ([`Band::of`]), the material it is painted on, how assistive technology
/// hears it, and whether it covers the whole display — so the same role
/// behaves the same way whichever component implements it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// A menu, flyout or context menu hanging beside the thing it acts on.
    Popup,
    /// A picker or mode the person is held in until they answer it: the
    /// emoji picker, a dictation session, the Alt+Tab switcher.
    Overlay,
    /// A sheet a person asked for and works in, possibly while the rest of
    /// the desktop stays visible: Quick Settings, the workspace sheet.
    Dialog,
    /// A picker on another process's behalf, asked through the portal; the
    /// app that asked is waiting on the answer.
    Picker,
    /// A question that *is* the grant: it asks the person, who alone may
    /// answer it, and it is never opened on an app's or an agent's word.
    Consent,
    /// A short, self-dismissing message. It takes no input.
    Toast,
}

/// The material a surface's body is painted on. Never a raw colour: the
/// theme resolves it (`ui::floating_surface::Material`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Material {
    /// Floating chrome over the blurred backdrop: menus, panels, sheets.
    Floating,
    /// Always opaque, for a surface whose backdrop must not show through —
    /// a prompt for a secret over content the person does not own.
    Opaque,
    /// Chrome that must leave the pixels under it visible: the capture bar
    /// over what is being chosen, the presence markers.
    SeeThrough,
}

/// Who may work a surface while it is open.
///
/// This is not who may *answer* it: a [`Role::Consent`] surface is answered
/// by a person on this device however open it is, and this enum only says
/// who the keyboard and pointer reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Modality {
    /// Shares the desktop with everything else; what it does not use goes
    /// on to the layer below. Presence markers, readouts.
    Passive,
    /// Everything behind it is inert so nothing can be clicked into
    /// agreeing, while what it shows belongs to a window or an app: the
    /// Alt+Tab switcher, a picker raised for one app.
    WindowModal,
    /// Nothing behind it is reachable at all, and the person cannot leave
    /// the surface by accident: a consent question, first-use setup.
    SystemModal,
}

/// Whose content a surface shows: where a surface closes, whose frame it is
/// counted in, and who hears about its outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// The compositor's own chrome: the session menu, Quick Settings.
    Shell,
    /// One app's surface, which is that app's public way of asking.
    App(String),
    /// A portal request from `caller`, which waits for the answer.
    Portal(String),
}

/// Where a surface is placed horizontally: the display, and the edge it
/// hangs against within that display. The display itself is
/// [`LayerGeometry::display_for`]'s answer for [`Self::display_of`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Anchor {
    /// Centred on the display, inset from every display edge: a sheet or a
    /// question.
    Center,
    /// Hanging from the display's top bar, against its trailing edge.
    TopTrailing(f64),
    /// Hanging from the display's top bar, against its leading edge.
    TopLeading(f64),
    /// Bottom centre, unless that would cover `caret`.
    BottomCenter(Option<Rect>),
    /// Beside `at`: `gap` clear of it, on the side with room.
    Beside { at: Rect, gap: f64 },
    /// Over the thing it acts on.
    At(Point),
    /// The whole display, or the whole desktop when the surface is drawn
    /// once for every display at once.
    Fill,
}

impl Anchor {
    /// The point [`LayerGeometry::display_for`] resolves this anchor's
    /// display from; `None` for a surface that takes the focused display.
    pub(crate) const fn display_of(&self) -> Option<Rect> {
        match *self {
            Self::Beside { at, .. } => Some(at),
            Self::At(point) => Some(Rect::new(point.x, point.y, point.x, point.y)),
            Self::Center
            | Self::TopTrailing(_)
            | Self::TopLeading(_)
            | Self::BottomCenter(_)
            | Self::Fill => None,
        }
    }
}

/// Where a surface is in its life: it is asked for, it is on screen, it is
/// on its way out and still animating, or it has closed and returns focus
/// to whatever opened it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    /// Asked for. A consent question waits here while the thing that asked
    /// for it is told what is being asked, and it never shows content
    /// before it is [`Self::Shown`].
    Requested,
    Shown,
    /// Out on its motion; it keeps no input.
    Closing,
    /// Closed. What opened it takes focus back.
    Closed,
}

impl Lifecycle {
    /// The state a surface that is on screen is in; the state a surface
    /// that is not is on its way to.
    pub(crate) const fn of(open: bool) -> Self {
        if open { Self::Shown } else { Self::Closed }
    }

    /// Whether a surface in this state takes input.
    pub(crate) const fn takes_input(self) -> bool {
        matches!(self, Self::Shown)
    }
}

/// What a surface *is*, as one value: its kind, placement, modality, owner
/// and where it is in its life. A surface answers [`ShellLayer::surface`]
/// with this, or `None` for the parts of it that are not a surface at all —
/// the overview's grid and the Alt+Tab switcher are whole-desktop modes
/// drawn in layer order rather than standing over it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Surface {
    pub kind: Role,
    pub anchor: Anchor,
    pub modality: Modality,
    pub owner: Owner,
    pub material: Material,
    pub lifecycle: Lifecycle,
    /// The region of the display the surface covers, when it computes one.
    /// The host falls back to the display for a modal surface and to the
    /// layer's own accessibility rect otherwise.
    pub bounds: Option<Rect>,
}

impl Surface {
    /// A surface this compositor owns, in the band its kind is offered in.
    pub(crate) const fn shell(
        kind: Role,
        anchor: Anchor,
        modality: Modality,
        material: Material,
        open: bool,
    ) -> Self {
        Self {
            kind,
            anchor,
            modality,
            owner: Owner::Shell,
            material,
            lifecycle: Lifecycle::of(open),
            bounds: None,
        }
    }

    /// The display this surface is placed on, from `geometry`: the one
    /// holding its anchor, else the focused display, else the primary.
    pub(crate) fn display(&self, geometry: LayerGeometry<'_>) -> Rect {
        geometry.display_for(self.anchor.display_of())
    }

    /// Whether the surface absorbs everything behind it until it closes.
    pub(crate) const fn is_modal(&self) -> bool {
        matches!(self.modality, Modality::WindowModal | Modality::SystemModal)
    }

    /// The region that is inert while this surface is up and modal: its own
    /// bounds, else the whole display it is placed on.
    pub(crate) fn scrim(&self, geometry: LayerGeometry<'_>) -> Option<Rect> {
        if !self.is_modal() {
            return None;
        }
        Some(self.bounds.unwrap_or_else(|| self.display(geometry)))
    }
}

/// One self-contained shell overlay.
pub(crate) trait ShellLayer {
    fn id(&self) -> LayerId;

    /// What kind of surface this is, where it stands, whether it is modal,
    /// who owns it and where it is in its life; `None` when this layer is a
    /// whole-desktop mode rather than a surface over it.
    fn surface(&self) -> Option<Surface>;

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

    /// The stacked layer for `id`, or `None` while the host still routes it.
    pub(crate) fn layer(&self, id: LayerId) -> Option<&dyn ShellLayer> {
        self.layers().into_iter().find(|layer| layer.id() == id)
    }

    pub(crate) fn layer_mut(&mut self, id: LayerId) -> Option<&mut dyn ShellLayer> {
        self.layers_mut().into_iter().find(|layer| layer.id() == id)
    }

    /// What kind of surface `id` is, stacked or not: the layer's own answer
    /// while it is stacked, else the kind the host still gives the overlay
    /// it routes itself.
    pub(crate) fn surface(&self, id: LayerId) -> Surface {
        let kind = self
            .layer(id)
            .and_then(ShellLayer::surface)
            .unwrap_or_else(|| id.surface_kind());
        Surface {
            lifecycle: Lifecycle::of(self.is_open(id)),
            ..kind
        }
    }

    /// Whether overlay `id` is on screen, by the same answer assistive
    /// technology gets ([`ShellLayer::is_shown`]).
    pub(crate) fn is_open(&self, id: LayerId) -> bool {
        self.layer(id).map_or(false, ShellLayer::is_shown)
    }

    /// Every open surface, topmost first in [`LayerId`] order.
    pub(crate) fn surfaces(&self) -> impl Iterator<Item = (LayerId, Surface)> + '_ {
        LayerId::ALL
            .into_iter()
            .filter(|&id| self.is_open(id))
            .map(|id| (id, self.surface(id)))
    }

    /// The region that is inert while a modal surface is up: the topmost
    /// open one, and what it covers. A consent question about a display
    /// covers that display; the switcher covers everything.
    pub(crate) fn scrim(&self, geometry: LayerGeometry<'_>) -> Option<Rect> {
        self.surfaces()
            .find_map(|(_, surface)| surface.scrim(geometry))
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
    fn a_stacked_layer_describes_the_surface_it_is() {
        let mut stack = OverlayStack::default();
        stack.power_menu.open();
        let surface = stack.surface(LayerId::PowerMenu);
        assert_eq!(surface.kind, Role::Popup);
        assert_eq!(surface.modality, Modality::WindowModal);
        assert_eq!(surface.owner, Owner::Shell);
        assert_eq!(surface.lifecycle, Lifecycle::Shown);
        assert_eq!(
            surface.anchor,
            Anchor::TopTrailing(stack.power_menu.metrics().margin),
            "the menu hangs from the top bar by the floating margin"
        );
        // Closed, the same surface still says what it is; it just is not
        // in the life of taking input any more.
        assert!(stack.power_menu.close());
        let closed = stack.surface(LayerId::PowerMenu);
        assert_eq!(closed.lifecycle, Lifecycle::Closed);
        assert_eq!(closed.kind, Role::Popup);
    }

    #[test]
    fn an_overlay_the_host_still_routes_has_its_kind_and_its_band() {
        let stack = OverlayStack::default();
        for id in LayerId::ALL {
            let surface = stack.surface(id);
            assert_eq!(surface.lifecycle, Lifecycle::Closed, "{id:?}");
            assert_eq!(surface.owner, Owner::Shell, "{id:?}");
            if stack.layer(id).is_some() {
                continue;
            }
            // A layer that joins the stack answers for itself; until then
            // the declaration is the only answer, and it has to say what
            // the host's own routing already does.
            let band = id.band();
            assert_eq!(
                Band::of(surface.lifecycle, surface.modality),
                None,
                "{id:?} is closed"
            );
            assert!(
                band.is_some(),
                "{id:?} has a band, so it is routed as a surface"
            );
            let shown = Lifecycle::Shown;
            assert_eq!(
                Band::of(shown, surface.modality),
                Some(band.unwrap()),
                "{id:?}: its modality decides the band its input is offered in"
            );
        }
    }

    #[test]
    fn only_a_modal_surface_holds_down_what_is_behind_it() {
        let mut stack = OverlayStack::default();
        assert_eq!(stack.scrim(geometry()), None, "nothing is up");
        stack.power_menu.open();
        let scrim = stack
            .scrim(geometry())
            .expect("the menu is modal over its display");
        assert_eq!(scrim, VIEWPORT);
        // Its own panel is not the area it makes inert: the press outside
        // the panel still reaches the menu first, and closes it.
        assert!(
            stack.power_menu.panel_rect(VIEWPORT) != scrim,
            "the panel is not the whole display"
        );
        let bridge = Surface::shell(
            Role::Dialog,
            Anchor::Center,
            Modality::Passive,
            Material::Floating,
            true,
        );
        assert_eq!(bridge.scrim(geometry()), None);
        assert_eq!(
            Band::of(bridge.lifecycle, bridge.modality),
            Some(Band::Popover),
            "a passive surface shares the desktop"
        );
        assert!(
            !Surface {
                modality: Modality::SystemModal,
                ..bridge
            }
            .scrim(geometry())
            .is_none()
        );
    }

    #[test]
    fn a_surface_is_placed_on_the_display_holding_its_anchor() {
        let left = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let right = Rect::new(1280.0, 0.0, 3840.0, 1440.0);
        let outputs = [left, right];
        let geometry = LayerGeometry {
            viewport: left.union(right),
            outputs: &outputs,
            focused_output: None,
        };
        let caret = Rect::new(2000.0, 100.0, 2002.0, 120.0);
        let beside = Surface::shell(
            Role::Overlay,
            Anchor::Beside {
                at: caret,
                gap: 0.0,
            },
            Modality::WindowModal,
            Material::Floating,
            true,
        );
        assert_eq!(beside.display(geometry), right);
        assert_eq!(
            beside.scrim(geometry),
            Some(right),
            "a modal surface makes inert the display it stands on, not every display"
        );
        let centered = Surface::shell(
            Role::Dialog,
            Anchor::Center,
            Modality::SystemModal,
            Material::Floating,
            true,
        );
        assert_eq!(centered.display(geometry), right, "the focused display");
    }

    #[test]
    fn a_modal_surface_makes_inert_the_display_it_covers_alone() {
        let left = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let right = Rect::new(1280.0, 0.0, 3840.0, 1440.0);
        let outputs = [left, right];
        let geometry = LayerGeometry {
            viewport: left.union(right),
            outputs: &outputs,
            focused_output: Some(left),
        };
        // A dialog the person opened on the other display does not freeze
        // this one, so its scrim stops at its own display's edge.
        let sheet = Surface {
            bounds: Some(left.inflate(-100.0, -80.0)),
            ..Surface::shell(
                Role::Dialog,
                Anchor::Center,
                Modality::WindowModal,
                Material::Floating,
                true,
            )
        };
        assert_eq!(sheet.scrim(geometry), Some(left.inflate(-100.0, -80.0)));
        // The question the whole session waits on covers every display.
        let question = Surface {
            bounds: Some(left),
            ..Surface::shell(
                Role::Consent,
                Anchor::Fill,
                Modality::SystemModal,
                Material::Floating,
                true,
            )
        };
        assert_eq!(question.scrim(geometry), Some(left));
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
