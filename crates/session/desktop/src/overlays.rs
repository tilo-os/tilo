//! What stands over the desktop's windows, and in what order.
//! A menu, a picker, a panel that hangs under the bar: each is a surface, and a surface is
//! described once — its kind, where it stands, who may use it and where it is in its life — and
//! everything else follows from that description. `docs/architecture/surfaces.md` is the contract;
//! this module carries it into the session as one [`Overlays`] handle.
//! The stack answers the questions every overlay otherwise answers for itself: which surface the
//! keyboard goes to, what closing the top one restores, where a surface hangs on its display, and
//! what a press outside it closes.
//! Stacking is [`Slot::ALL`] order, topmost first: input is offered to the open surfaces in it,
//! assistive technology reads them in it, and Escape closes the first open one. It is not derived
//! from kinds — a display question answered above onboarding is a fact about the session, not about
//! either one's modality.
//! The slots the desktop's own chrome still draws (`Slot::is_chrome`) are mirrored from the widgets
//! that own their state; a press outside one closes it here where the widget says where it stands,
//! and through the widget that holds the press when the press is the chrome's. The two the session
//! puts up itself (the window menu and quick settings) declare [`Slot::session_closes`] and are
//! closed by the session's own verbs.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ui::kurbo::{Point, Rect};

use crate::topbar::TOPBAR_HEIGHT_PX;

/// Every overlay the session puts above the windows, topmost first.
/// Declaration order *is* the stacking order, as it was for the shell's layers: input is offered to
/// open surfaces in it, assistive technology reads them in it, and Escape closes the first open
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// The search palette: over everything, the window it was opened from under it.
    Search,
    /// The emoji picker.
    Emoji,
    /// A window's menu: the rows the desk offers that window.
    WindowMenu,
    /// The menu of the workspace in front, opened from its name on the bar.
    WorkspaceMenu,
    /// The workspace's name on the bar, being typed: its menu's Rename turns the name into a field.
    WorkspaceName,
    /// The menu behind the bar's Tilo mark.
    TiloMenu,
    /// The notification centre, under the bar's trailing end.
    Centre,
    /// The workspace overview.
    Overview,
    /// The strip ribbon and rail: which strip of the workspace, which window of the strip.
    Strips,
    /// Quick settings: the devices, the sound and the notifications, under the bar's trailing end
    /// or at the caret that asked for it.
    QuickSettings,
}

impl Slot {
    /// Every slot, topmost first.
    pub const ALL: [Self; 10] = [
        Self::Search,
        Self::Emoji,
        Self::WindowMenu,
        Self::WorkspaceMenu,
        Self::WorkspaceName,
        Self::TiloMenu,
        Self::Centre,
        Self::Overview,
        Self::Strips,
        Self::QuickSettings,
    ];

    /// The slot's name, as a reading, a view and an agent name it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Emoji => "emoji",
            Self::WindowMenu => "window-menu",
            Self::WorkspaceMenu => "workspace-menu",
            Self::WorkspaceName => "workspace-name",
            Self::TiloMenu => "tilo-menu",
            Self::Centre => "centre",
            Self::Overview => "overview",
            Self::Strips => "strips",
            Self::QuickSettings => "quick-settings",
        }
    }

    /// The slot `text` names, as the verbs and the readings spell it.
    #[must_use]
    pub fn named(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|slot| slot.as_str() == text)
    }

    /// What kind of surface this slot is.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Search => Role::Dialog,
            Self::Emoji | Self::Overview | Self::Strips => Role::Overlay,
            Self::WindowMenu
            | Self::WorkspaceMenu
            | Self::WorkspaceName
            | Self::TiloMenu
            | Self::Centre
            | Self::QuickSettings => Role::Popup,
        }
    }

    /// Who may work the surface while it is open: a surface that takes the keyboard holds it
    /// against everything behind it, and a press outside it closes it rather than reaching what it
    /// covers.
    #[must_use]
    pub const fn modality(self) -> Modality {
        Modality::WindowModal
    }

    /// Whose surface this is.
    #[must_use]
    pub const fn owner(self) -> Owner {
        Owner::Shell
    }

    /// Whether the surface stands over the desk beside others: a menu, a popover, the strip labels,
    /// the search, quick settings — one at a time. The overview is the desk itself, the surface
    /// everything else stands over, so it is not one of them.
    #[must_use]
    pub const fn stands_beside(self) -> bool {
        !matches!(self, Self::Overview)
    }

    /// Where the surface hangs when the verb that opened it named no place.
    #[must_use]
    pub const fn anchor(self) -> Anchor {
        match self {
            Self::Overview | Self::Strips => Anchor::Fill,
            Self::Search | Self::Emoji => Anchor::Center,
            Self::WindowMenu => Anchor::At(Point::ZERO),
            Self::WorkspaceMenu | Self::WorkspaceName | Self::TiloMenu => Anchor::TopLeading(0.0),
            Self::Centre | Self::QuickSettings => Anchor::TopTrailing(POPOVER_MARGIN),
        }
    }

    /// How much of the display the surface takes: `None` where it is as wide or as tall as its
    /// anchor leaves room for.
    #[must_use]
    pub const fn size(self) -> Size {
        match self {
            Self::QuickSettings => Size {
                width: Some(POPOVER_WIDTH),
                height: Some(POPOVER_HEIGHT),
            },
            // As tall as its rows, down to the display's margin.
            Self::Centre => Size {
                width: Some(POPOVER_WIDTH),
                height: None,
            },
            _ => Size {
                width: None,
                height: None,
            },
        }
    }

    /// Whether the desktop's chrome draws and answers this slot itself, as it still does for its
    /// menus, the picker, the overview and the strip ribbon. Each leaves this set as its own view
    /// takes it over.
    #[must_use]
    pub const fn is_chrome(self) -> bool {
        !matches!(self, Self::WindowMenu | Self::QuickSettings)
    }

    /// Whether the session's own verbs open and close this slot: the window menu and quick settings
    /// are the session's, while the chrome's slots close through the widgets that keep their state.
    #[must_use]
    pub const fn session_closes(self) -> bool {
        !self.is_chrome()
    }
}

/// How much of its display a surface covers. `None` stands for as far as the anchor leaves room.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: Option<f64>,
    pub height: Option<f64>,
}

/// The width of a popover hanging under the bar, logical pixels.
pub const POPOVER_WIDTH: f64 = 380.0;

/// The height of a popover hanging under the bar: its footprint, which no page it shows moves. What
/// a page has beyond it scrolls inside it.
pub const POPOVER_HEIGHT: f64 = 640.0;

/// The gap between a popover and the edges of its display.
pub const POPOVER_MARGIN: f64 = 8.0;

/// The kind of thing a surface is: what the person sees standing over the desktop. The kind fixes
/// how Escape and a press outside it answer, how assistive technology hears it and whether it
/// covers the whole display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A menu, flyout or context menu hanging beside the thing it acts on.
    Popup,
    /// A picker or mode the person is held in until they answer it: the emoji picker, the overview.
    Overlay,
    /// A sheet a person asked for and works in.
    Dialog,
    /// A picker on another process's behalf, asked through the portal; the app that asked is
    /// waiting on the answer.
    Picker,
    /// A question that *is* the grant: it asks the person, who alone may answer it.
    Consent,
    /// A short, self-dismissing message. It takes no input.
    Toast,
}

impl Role {
    /// The role's name, as a reading spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Popup => "popup",
            Self::Overlay => "overlay",
            Self::Dialog => "dialog",
            Self::Picker => "picker",
            Self::Consent => "consent",
            Self::Toast => "toast",
        }
    }

    /// Whether Escape closes a surface of this kind: a menu, a sheet and a
    /// picker do. An overlay answers Escape only if its own model says so,
    /// and a consent question never closes as a side effect — it answers.
    #[must_use]
    pub const fn closes_on_escape(self) -> bool {
        matches!(self, Self::Popup | Self::Dialog | Self::Picker)
    }

    /// Whether a press outside a surface of this kind closes it. A popup does; an overlay, a sheet,
    /// a picker and a consent question stay until they are answered.
    #[must_use]
    pub const fn closes_on_press_outside(self) -> bool {
        matches!(self, Self::Popup)
    }
}

/// Where a surface is placed: the display, and the edge or point it hangs against within that
/// display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    /// Centred on the display, inset from every display edge.
    Center,
    /// Hanging from the display's top bar, against its trailing edge.
    TopTrailing(f64),
    /// Hanging from the display's top bar, against its leading edge.
    TopLeading(f64),
    /// Bottom centre, unless that would cover `caret` .
    BottomCenter(Option<Rect>),
    /// Beside `at` : `gap` clear of it, on the side with room.
    Beside { at: Rect, gap: f64 },
    /// Over the thing it acts on. The surface's own view places itself.
    At(Point),
    /// The whole display.
    Fill,
}

impl Anchor {
    /// The rect this anchor chooses a display by: the control, the caret or the pointer it was
    /// opened from; `None` for a surface that takes the display the person works on.
    #[must_use]
    pub const fn display_of(&self) -> Option<Rect> {
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

    /// The anchor as a verb spells it, and as a reading gives it back:
    /// `center`, `fill`, `top-trailing:8`, `top-leading:0`,
    /// `bottom-center:x0,y0,x1,y1`, `beside:x0,y0,x1,y1,gap`, `at:x,y`.
    #[must_use]
    pub fn as_str(&self) -> String {
        fn numbers(values: &[f64]) -> String {
            values
                .iter()
                .map(f64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }
        match *self {
            Self::Center => "center".into(),
            Self::Fill => "fill".into(),
            Self::TopTrailing(gap) => format!("top-trailing:{gap}"),
            Self::TopLeading(gap) => format!("top-leading:{gap}"),
            Self::BottomCenter(None) => "bottom-center".into(),
            Self::BottomCenter(Some(at)) => {
                format!("bottom-center:{}", numbers(&[at.x0, at.y0, at.x1, at.y1]))
            }
            Self::Beside { at, gap } => {
                format!("beside:{}", numbers(&[at.x0, at.y0, at.x1, at.y1, gap]))
            }
            Self::At(point) => format!("at:{}", numbers(&[point.x, point.y])),
        }
    }

    /// The anchor `text` spells, as [`Self::as_str`] writes it; `None` when it is no anchor.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        fn numbers(rest: &str) -> Option<Vec<f64>> {
            if rest.trim().is_empty() {
                return Some(Vec::new());
            }
            rest.split(',')
                .map(|number| number.trim().parse().ok())
                .collect()
        }
        let (kind, rest) = text
            .split_once(':')
            .map_or((text, ""), |(kind, rest)| (kind, rest));
        match kind.trim() {
            "center" => Some(Self::Center),
            "fill" => Some(Self::Fill),
            "top-trailing" => Some(Self::TopTrailing(rest.trim().parse().ok()?)),
            "top-leading" => Some(Self::TopLeading(rest.trim().parse().ok()?)),
            "bottom-center" => match numbers(rest)?.as_slice() {
                [] => Some(Self::BottomCenter(None)),
                &[x0, y0, x1, y1] => Some(Self::BottomCenter(Some(Rect::new(x0, y0, x1, y1)))),
                _ => None,
            },
            "beside" => match numbers(rest)?.as_slice() {
                &[x0, y0, x1, y1, gap] => Some(Self::Beside {
                    at: Rect::new(x0, y0, x1, y1),
                    gap,
                }),
                _ => None,
            },
            "at" => match numbers(rest)?.as_slice() {
                &[x, y] => Some(Self::At(Point::new(x, y))),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Who may work a surface while it is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modality {
    /// Shares the desktop with everything else; what it does not use goes on to what is below. It
    /// never takes the keyboard.
    Passive,
    /// Everything behind it is inert while it is up, and it takes the keyboard: a menu, a picker, a
    /// panel a press outside closes.
    WindowModal,
    /// Nothing behind it is reachable at all, and the person cannot leave the surface by accident:
    /// a consent question, first-use setup.
    SystemModal,
}

impl Modality {
    /// The modality's name, as a reading spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::WindowModal => "window-modal",
            Self::SystemModal => "system-modal",
        }
    }

    /// Whether a surface this modal takes the keyboard from what is behind it. A passive surface
    /// never does.
    #[must_use]
    pub const fn takes_keyboard(self) -> bool {
        !matches!(self, Self::Passive)
    }
}

/// Whose surface it is: who is told about its outcome, and whose frame it is counted in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The compositor's own chrome: its menus, quick settings.
    Shell,
    /// One app's surface, which is that app's public way of asking.
    App(String),
    /// A portal request from `caller` , which waits for the answer.
    Portal(String),
}

impl Owner {
    /// The owner's name, as a reading spells it.
    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            Self::Shell => "shell".into(),
            Self::App(app) => format!("app:{app}"),
            Self::Portal(caller) => format!("portal:{caller}"),
        }
    }
}

/// Where a surface is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// Asked for, and not yet on screen.
    Requested,
    /// On screen, and taking input.
    Shown,
    /// On its way out; it takes no input.
    Closing,
    /// Closed. What opened it holds the keyboard again.
    Closed,
}

impl Lifecycle {
    /// The state of a surface that is open or not; the stack keeps no exit motion and no consent
    /// that waits to be told.
    #[must_use]
    pub const fn of(open: bool) -> Self {
        if open { Self::Shown } else { Self::Closed }
    }

    /// The state's name, as a reading spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Shown => "shown",
            Self::Closing => "closing",
            Self::Closed => "closed",
        }
    }
}

/// What holds the keyboard: the desk's windows, or a surface over them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The window in front: nothing over the desk wants the keyboard.
    Window,
    /// A surface over the desk holds the keyboard.
    Surface(Slot),
}

impl Target {
    /// The surface, while one holds the keyboard.
    #[must_use]
    pub const fn slot(self) -> Option<Slot> {
        match self {
            Self::Window => None,
            Self::Surface(slot) => Some(slot),
        }
    }

    /// The target's name, as a reading spells it.
    #[must_use]
    pub fn as_str(self) -> String {
        match self {
            Self::Window => "window".into(),
            Self::Surface(slot) => format!("overlay:{}", slot.as_str()),
        }
    }
}

/// One open surface, as the stack publishes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub slot: Slot,
    pub role: Role,
    pub anchor: Anchor,
    pub size: Size,
    pub modality: Modality,
    pub owner: Owner,
    pub lifecycle: Lifecycle,
    /// The rect the host places it at, when the anchor and its size settle one. A surface its own
    /// view places has none.
    pub place: Option<Rect>,
}

/// One slot's state in the stack.
#[derive(Clone, Debug, Default)]
struct SlotState {
    open: bool,
    /// Where this opening put it; the slot's own anchor while the verb named none.
    anchor: Option<Anchor>,
    /// Where the widget drawing it says it stands, while it is up: a menu is as big as its rows,
    /// which the slot's anchor cannot say.
    stands: Option<Rect>,
    /// Whether a press outside last closed it: it does not open again for that press, which is
    /// still being delivered.
    dismissed: bool,
    /// Whether the stack took it down and the widget drawing it has yet to follow: while it has
    /// not, the widget's own open state is the one being left behind, not a new opening to put back
    /// up. The widget's [`Overlays::close`] is how it follows.
    awaiting_close: bool,
}

/// The displays surfaces are placed on: every output's rect, and the one the person works on.
#[derive(Clone, Debug, Default)]
struct Displays {
    outputs: Vec<Rect>,
    focused: Option<Rect>,
    /// Where the person types, when a field holds the caret: the place a surface a key opens for
    /// the text stands against.
    caret: Option<Rect>,
}

impl Displays {
    /// The display a surface opens on: the one holding `anchor` , else the one the person works on,
    /// else the first. Never the whole desktop while there are displays: across two of them it
    /// straddles the seam.
    fn display_for(&self, anchor: Option<Rect>) -> Option<Rect> {
        anchor
            .and_then(|anchor| {
                self.outputs
                    .iter()
                    .copied()
                    .find(|display| display.contains(anchor.center()))
            })
            .or(self.focused)
            .or_else(|| self.outputs.first().copied())
    }
}

/// Where a surface stands on `display` , from its anchor and the room its size leaves; `None` for a
/// surface its own view places, or one whose size and anchor do not settle a rect.
pub(crate) fn place_on(display: Rect, anchor: Anchor, size: Size) -> Option<Rect> {
    // The bar hangs above a surface anchored to the top edge.
    let top = display.y0 + f64::from(TOPBAR_HEIGHT_PX).ceil();
    let margin = POPOVER_MARGIN;
    let width = size
        .width
        .unwrap_or_else(|| (display.width() - 2.0 * margin).max(1.0));
    let height = size.height;
    let rect = match anchor {
        Anchor::TopTrailing(gap) => {
            let x1 = display.x1 - gap;
            let x0 = x1 - width;
            // A surface that names its height keeps it, down to the margin;
            // one that does not reaches to the bottom of its display.
            let y1 = height.map_or(display.y1 - gap, |height| {
                (top + height).min(display.y1 - gap)
            });
            Rect::new(x0, top, x1, y1)
        }
        Anchor::TopLeading(gap) => {
            let x0 = display.x0 + gap;
            let y1 = height.map_or(display.y1 - gap, |height| {
                (top + height).min(display.y1 - gap)
            });
            Rect::new(x0, top, x0 + width, y1)
        }
        Anchor::Center => {
            let height = height?;
            let center = display.center();
            Rect::new(
                center.x - width / 2.0,
                center.y - height / 2.0,
                center.x + width / 2.0,
                center.y + height / 2.0,
            )
        }
        Anchor::Fill => display,
        Anchor::BottomCenter(caret) => {
            let height = height?;
            let center = caret.map_or_else(|| display.center().x, |caret| caret.x1);
            let y1 = display.y1 - margin;
            Rect::new(center - width / 2.0, y1 - height, center + width / 2.0, y1)
        }
        // Beside the rect it was opened on: the side with room wins, and the surface never overlaps
        // it.
        Anchor::Beside { at, gap } => {
            let x0 = if at.x1 + gap + width <= display.x1 - margin {
                at.x1 + gap
            } else {
                (at.x0 - gap - width).max(display.x0 + margin)
            };
            let height = height.unwrap_or_else(|| (display.y1 - margin - at.y0).max(1.0));
            let y0 = at
                .y0
                .min(display.y1 - margin - height)
                .max(display.y0 + margin);
            Rect::new(x0, y0, x0 + width, y0 + height)
        }
        // Over the thing it acts on: the view decides its own card.
        Anchor::At(_) => return None,
    };
    Some(rect)
}

/// The overlays over the windows, shared between the session's loop, the keys it answers and the
/// chrome that draws some of them.
#[derive(Clone, Debug, Default)]
pub struct Overlays(Rc<RefCell<Stack>>);

impl Overlays {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for the next pass of whatever draws what the stack holds, set by the host that draws it:
    /// a surface the stack takes down is one the widget showing it has to take up a pass to close,
    /// and nothing else tells it the press went down.
    pub fn wake_with(&self, wake: Rc<std::cell::Cell<bool>>) {
        *self.0.borrow_mut().wake.borrow_mut() = Some(wake);
    }

    /// The stack changed under what draws it: let it take up a pass.
    fn woke(&self) {
        if let Some(wake) = self.0.borrow().wake.borrow().as_ref() {
            wake.set(true);
        }
    }

    /// The displays surfaces are placed on: every output, and the one the person works on.
    pub fn set_displays(&self, outputs: &[Rect], focused: Option<Rect>) {
        let mut stack = self.0.borrow_mut();
        if stack.displays.outputs.as_slice() == outputs && stack.displays.focused == focused {
            return;
        }
        stack.displays.outputs = outputs.to_vec();
        stack.displays.focused = focused;
    }

    /// The caret of the field the person types into, as the desktop last saw it: what a surface a
    /// key opens for the text stands beside.
    pub fn set_caret(&self, caret: Option<Rect>) {
        self.0.borrow_mut().displays.caret = caret;
    }

    /// The caret a surface a key opens for the text stands beside, when a field holds one.
    #[must_use]
    pub fn caret(&self) -> Option<Rect> {
        self.0.borrow().displays.caret
    }

    /// Put `slot` up, standing at `anchor` or where the slot stands by itself.
    /// One anchored surface at a time: the person looks at the one they just opened, so the others
    /// go down with it. Their widgets follow the stack ([`Self::is_open`]), which is how the chrome
    /// learns to close them.
    pub fn open(&self, slot: Slot, anchor: Option<Anchor>) {
        {
            let mut stack = self.0.borrow_mut();
            if slot.stands_beside() {
                for other in Slot::ALL {
                    if other != slot
                        && other.stands_beside()
                        && let Some(state) = stack.slots.get_mut(&other)
                        && state.open
                    {
                        state.open = false;
                        state.awaiting_close = true;
                    }
                }
            }
            let state = stack.slots.entry(slot).or_default();
            state.open = true;
            state.anchor = Some(anchor.unwrap_or_else(|| slot.anchor()));
            state.dismissed = false;
            state.awaiting_close = false;
        }
        self.woke();
    }

    /// Take `slot` down, as the widget drawing it does once the stack has: what the stack took down
    /// is settled by this, and a slot that is already down moved nothing.
    pub fn close(&self, slot: Slot) {
        {
            let mut stack = self.0.borrow_mut();
            let Some(state) = stack.slots.get_mut(&slot) else {
                return;
            };
            let followed = std::mem::take(&mut state.awaiting_close);
            if !state.open && !followed {
                return;
            }
            state.open = false;
        }
        self.woke();
    }

    /// Say where the widget drawing `slot` paints it, while it is up.
    ///
    /// A surface the session places is judged by the rect its anchor and size
    /// settle; the chrome's own menus hang off the bar as big as their rows,
    /// which only the widget knows. `None` takes back the last answer, as a
    /// surface going down does.
    pub fn stands(&self, slot: Slot, stands: Option<Rect>) {
        let mut stack = self.0.borrow_mut();
        match stack.slots.get_mut(&slot) {
            Some(state) => state.stands = stands,
            None if stands.is_some() => stack.slots.entry(slot).or_default().stands = stands,
            None => {}
        }
    }

    /// A press outside `slot` closed it: it stays down for that press, which is still being
    /// delivered.
    pub fn dismiss(&self, slot: Slot) {
        {
            let mut stack = self.0.borrow_mut();
            let state = stack.slots.entry(slot).or_default();
            state.open = false;
            state.dismissed = true;
            state.awaiting_close = true;
        }
        self.woke();
    }

    /// The button that press came from is up: the press is over. What it closed stays down until
    /// the asks it raised have been carried out ([`Self::settle_press`]), because a quick click
    /// delivers its release before them.
    pub fn press_ended(&self) {
        self.0.borrow_mut().press_over = true;
    }

    /// What the press asked has been carried out: what it closed may be raised again.
    pub fn settle_press(&self) {
        let mut stack = self.0.borrow_mut();
        if !std::mem::take(&mut stack.press_over) {
            return;
        }
        for state in stack.slots.values_mut() {
            state.dismissed = false;
        }
    }

    /// Whether `slot` was dismissed by a press outside that is still being delivered, so nothing
    /// that press raised may put it back up.
    #[must_use]
    pub fn dismissed_recently(&self, slot: Slot) -> bool {
        self.0
            .borrow()
            .slots
            .get(&slot)
            .is_some_and(|state| state.dismissed)
    }

    /// Whether `slot` was taken down by the stack and the widget drawing it has yet to follow: its
    /// open state is the one being left behind, not a new opening to put back up.
    #[must_use]
    pub fn awaiting_close(&self, slot: Slot) -> bool {
        self.0
            .borrow()
            .slots
            .get(&slot)
            .is_some_and(|state| state.awaiting_close)
    }

    /// Whether `slot` is up.
    #[must_use]
    pub fn is_open(&self, slot: Slot) -> bool {
        self.0
            .borrow()
            .slots
            .get(&slot)
            .is_some_and(|state| state.open)
    }

    /// What holds the keyboard: the topmost open surface that takes it, else the window in front.
    /// Closing a surface derives this again, which is how the one it covered takes the keyboard
    /// back.
    #[must_use]
    pub fn target(&self) -> Target {
        let stack = self.0.borrow();
        Slot::ALL
            .into_iter()
            .find(|slot| stack.is_shown(*slot) && slot.modality().takes_keyboard())
            .map_or(Target::Window, Target::Surface)
    }

    /// Where `slot` stands now, if its anchor and size settle a rect the host can place it at.
    #[must_use]
    pub fn place(&self, slot: Slot) -> Option<Rect> {
        self.0.borrow().entry(slot).and_then(|entry| entry.place)
    }

    /// The surfaces open now, topmost first.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        let stack = self.0.borrow();
        Slot::ALL
            .into_iter()
            .filter_map(|slot| stack.entry(slot))
            .collect()
    }

    /// The surface Escape closes, by the dismissal table: the topmost one whose kind answers Escape
    /// and that the session may close.
    #[must_use]
    pub fn escape(&self) -> Option<Slot> {
        let stack = self.0.borrow();
        Slot::ALL.into_iter().find(|slot| {
            slot.session_closes() && slot.role().closes_on_escape() && stack.is_shown(*slot)
        })
    }

    /// The surface a press at this point closes, by the dismissal table: the topmost popup standing
    /// where the point is not. What the session placed is judged by the rect its anchor settles; a
    /// surface its own widget draws says where it stands, and one that says neither is its view's
    /// to close.
    #[must_use]
    pub fn press_outside(&self, x: f64, y: f64) -> Option<Slot> {
        let stack = self.0.borrow();
        Slot::ALL.into_iter().find(|slot| {
            if !slot.role().closes_on_press_outside() {
                return false;
            }
            let Some(state) = stack.slots.get(slot).filter(|state| state.open) else {
                return false;
            };
            let place = match state.stands {
                Some(place) => Some(place),
                None if slot.session_closes() => stack.entry(*slot).and_then(|entry| entry.place),
                None => None,
            };
            place.is_some_and(|place| !place.contains(Point::new(x, y)))
        })
    }
}

/// The stack behind the handle.
#[derive(Clone, Debug, Default)]
struct Stack {
    slots: BTreeMap<Slot, SlotState>,
    displays: Displays,
    /// Whether the button of the press that dismissed something has come up,
    /// and what it asked is yet to be carried out.
    press_over: bool,
    /// What draws the surfaces the stack holds, asked for its next pass whenever the stack changes
    /// under it ([`Overlays::wake_with`]).
    wake: std::cell::RefCell<Option<Rc<std::cell::Cell<bool>>>>,
}

impl Stack {
    /// Whether `slot` is up and taking input.
    fn is_shown(&self, slot: Slot) -> bool {
        self.slots.get(&slot).is_some_and(|state| state.open)
    }

    /// The open surface `slot` is, with where it stands now.
    fn entry(&self, slot: Slot) -> Option<Entry> {
        let state = self.slots.get(&slot)?;
        if !state.open {
            return None;
        }
        let anchor = state.anchor.unwrap_or_else(|| slot.anchor());
        let size = slot.size();
        let place = self
            .displays
            .display_for(anchor.display_of())
            .and_then(|display| place_on(display, anchor, size));
        Some(Entry {
            slot,
            role: slot.role(),
            anchor,
            size,
            modality: slot.modality(),
            owner: slot.owner(),
            lifecycle: Lifecycle::of(true),
            place,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    fn overlays() -> Overlays {
        let overlays = Overlays::new();
        overlays.set_displays(&[DISPLAY], Some(DISPLAY));
        overlays
    }

    #[test]
    fn every_slot_says_what_surface_it_is() {
        for slot in Slot::ALL {
            let role = slot.role();
            assert!(
                !role.as_str().is_empty() && !slot.as_str().is_empty(),
                "{slot:?} is named"
            );
            assert_eq!(slot.owner(), Owner::Shell, "{slot:?}");
            assert_eq!(Slot::named(slot.as_str()), Some(slot), "{slot:?}");
            // A surface that takes the keyboard is one a press outside may close; one that never
            // takes it is not.
            assert!(
                !role.closes_on_press_outside() || slot.modality().takes_keyboard(),
                "{slot:?}: a flyout takes the keyboard while it stands"
            );
        }
    }

    #[test]
    fn only_a_menu_closes_on_a_press_outside_and_escape_answers_by_kind() {
        assert!(Role::Popup.closes_on_press_outside());
        for role in [
            Role::Overlay,
            Role::Dialog,
            Role::Picker,
            Role::Consent,
            Role::Toast,
        ] {
            assert!(!role.closes_on_press_outside(), "{role:?} stays");
        }
        assert!(Role::Popup.closes_on_escape() && Role::Dialog.closes_on_escape());
        assert!(
            !Role::Overlay.closes_on_escape(),
            "an overlay answers Escape only if its own model says so"
        );
        assert!(!Role::Consent.closes_on_escape(), "a question never does");
    }

    #[test]
    fn one_anchored_surface_stands_at_a_time() {
        let overlays = overlays();
        overlays.open(Slot::TiloMenu, None);
        overlays.open(Slot::Strips, None);
        assert!(
            !overlays.is_open(Slot::TiloMenu),
            "the strip labels came up, and the menu went down with them"
        );
        overlays.open(Slot::QuickSettings, None);
        assert!(
            !overlays.is_open(Slot::Strips),
            "and quick settings after them"
        );
        assert!(overlays.is_open(Slot::QuickSettings));
        // The overview is the desk the others stand over, not one of them.
        overlays.open(Slot::Overview, None);
        assert!(
            overlays.is_open(Slot::QuickSettings),
            "the overview keeps what stands over it"
        );
        overlays.open(Slot::Strips, None);
        assert!(
            overlays.is_open(Slot::Overview),
            "and the strip labels leave the overview itself up"
        );
    }

    #[test]
    fn a_popover_under_the_bars_right_end_leaves_the_margin_and_the_bar() {
        let overlays = overlays();
        overlays.open(Slot::QuickSettings, None);
        let place = overlays.place(Slot::QuickSettings).expect("placed");
        let bar = f64::from(TOPBAR_HEIGHT_PX).ceil();
        assert_eq!(place.x1, DISPLAY.x1 - POPOVER_MARGIN);
        assert_eq!(place.x0, DISPLAY.x1 - POPOVER_MARGIN - POPOVER_WIDTH);
        assert_eq!(place.y0, bar, "under the bar");
        assert_eq!(place.width(), POPOVER_WIDTH);
        assert_eq!(
            place.height(),
            POPOVER_HEIGHT,
            "its footprint, not the rest of the display"
        );
        assert!(
            place.y1 < DISPLAY.y1 - POPOVER_MARGIN,
            "with the desk it does not cover left below it"
        );
    }

    #[test]
    fn a_popover_opens_beside_the_caret_it_was_asked_at() {
        let overlays = overlays();
        let caret = Rect::new(600.0, 300.0, 602.0, 320.0);
        overlays.open(
            Slot::QuickSettings,
            Some(Anchor::Beside {
                at: caret,
                gap: 4.0,
            }),
        );
        let place = overlays.place(Slot::QuickSettings).expect("placed");
        assert_eq!(place.x0, caret.x1 + 4.0, "clear of the caret");
        assert!(place.x0 >= caret.x1, "and never over it");
        assert_eq!(place.width(), POPOVER_WIDTH);
        assert_eq!(
            place.y1,
            DISPLAY.y1 - POPOVER_MARGIN,
            "its footprint reaches the display's margin"
        );
        assert!(
            place.y0 <= caret.y0,
            "raised only as far as its footprint needs to stay on the display"
        );

        // With no room on the right of the caret, it opens on the left.
        let edge = Rect::new(1200.0, 300.0, 1202.0, 320.0);
        overlays.open(
            Slot::QuickSettings,
            Some(Anchor::Beside { at: edge, gap: 4.0 }),
        );
        let place = overlays.place(Slot::QuickSettings).expect("placed");
        assert_eq!(place.x1, edge.x0 - 4.0);
        assert!(place.x1 <= edge.x0, "still clear of the caret");
    }

    #[test]
    fn the_caret_a_key_opens_at_outlives_the_displays_changing() {
        let overlays = overlays();
        let caret = Rect::new(600.0, 300.0, 602.0, 320.0);
        overlays.set_caret(Some(caret));
        // The outputs came and went while the person typed: where they type is still where the
        // surface a key opens for the text stands.
        overlays.set_displays(&[Rect::new(0.0, 0.0, 1000.0, 700.0)], Some(DISPLAY));
        assert_eq!(overlays.caret(), Some(caret));
        overlays.open(
            Slot::QuickSettings,
            overlays.caret().map(|at| Anchor::Beside { at, gap: 6.0 }),
        );
        let place = overlays.place(Slot::QuickSettings).expect("placed");
        assert_eq!(place.x0, caret.x1 + 6.0, "clear of the caret");
        assert!(
            place.y0 <= caret.y0,
            "level with it, or raised only as far as its footprint needs"
        );
        assert_eq!(
            place.height(),
            POPOVER_HEIGHT,
            "at the height the surface asks for"
        );
    }

    #[test]
    fn a_surface_is_placed_on_the_display_holding_its_anchor() {
        let left = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let right = Rect::new(1280.0, 0.0, 3840.0, 1440.0);
        let overlays = Overlays::new();
        overlays.set_displays(&[left, right], Some(left));
        // On the right display's trailing edge, though the person works on the left one.
        overlays.open(Slot::QuickSettings, Some(Anchor::TopTrailing(8.0)));
        let place = overlays.place(Slot::QuickSettings).expect("placed");
        assert_eq!(place.x1, left.x1 - 8.0, "the display the person works on");

        let caret = Rect::new(2000.0, 100.0, 2002.0, 120.0);
        overlays.open(
            Slot::QuickSettings,
            Some(Anchor::Beside {
                at: caret,
                gap: 0.0,
            }),
        );
        let place = overlays.place(Slot::QuickSettings).expect("placed");
        assert!(
            place.x0 >= right.x0,
            "the display holding the caret it was opened from"
        );
    }

    #[test]
    fn the_keyboard_goes_to_the_topmost_surface_and_comes_back_down() {
        let overlays = overlays();
        assert_eq!(overlays.target(), Target::Window, "the desk has it");

        overlays.open(Slot::Overview, None);
        assert_eq!(overlays.target(), Target::Surface(Slot::Overview));
        overlays.open(Slot::QuickSettings, None);
        assert_eq!(
            overlays.target(),
            Target::Surface(Slot::Overview),
            "the top of the stack holds it, not the last one opened"
        );

        overlays.close(Slot::Overview);
        assert_eq!(
            overlays.target(),
            Target::Surface(Slot::QuickSettings),
            "the one it covered takes it back"
        );
        overlays.close(Slot::QuickSettings);
        assert_eq!(overlays.target(), Target::Window, "and the window again");
    }

    #[test]
    fn escape_and_a_press_outside_close_the_top_menu_that_stands_where_it_is_not() {
        let overlays = overlays();
        overlays.open(Slot::QuickSettings, None);
        let place = overlays.place(Slot::QuickSettings).expect("placed");

        assert_eq!(overlays.escape(), Some(Slot::QuickSettings));
        assert_eq!(
            overlays.press_outside(4.0, DISPLAY.y1 - 4.0),
            Some(Slot::QuickSettings),
            "a press outside its rect"
        );
        assert_eq!(
            overlays.press_outside(place.center().x, place.center().y),
            None,
            "and none from inside it"
        );

        // The window menu takes its place: one anchored surface at a time.
        overlays.open(Slot::WindowMenu, Some(Anchor::At(Point::new(40.0, 50.0))));
        assert!(
            !overlays.is_open(Slot::QuickSettings),
            "the panel goes down for the menu the person just opened"
        );
        // And it stands where its own view draws it: nothing the session can judge, so nothing it
        // closes.
        assert_eq!(overlays.place(Slot::WindowMenu), None);
        assert_eq!(overlays.escape(), Some(Slot::WindowMenu));
        assert_eq!(
            overlays.press_outside(4.0, DISPLAY.y1 - 4.0),
            None,
            "its view closes it, the session has no rect to judge"
        );
    }

    #[test]
    fn a_change_in_the_stack_asks_what_draws_it_for_a_pass() {
        let overlays = overlays();
        let asked = Rc::new(std::cell::Cell::new(false));
        overlays.wake_with(Rc::clone(&asked));
        assert!(!asked.replace(false), "nothing asked yet");

        overlays.open(Slot::WorkspaceMenu, None);
        assert!(asked.replace(false), "the menu came up");
        overlays.dismiss(Slot::WorkspaceMenu);
        assert!(
            asked.replace(false),
            "a press outside took it down: the widget has to close it"
        );
        // The widget closing it follows the stack, which hears that: what the widget shows next is
        // a surface of its own again, not the state the stack took down.
        overlays.close(Slot::WorkspaceMenu);
        assert!(
            !overlays.awaiting_close(Slot::WorkspaceMenu),
            "the widget followed the stack"
        );
        assert!(asked.replace(false), "the widget followed it");
        assert!(
            !asked.replace(false),
            "and nothing is left to draw after that"
        );
        overlays.open(Slot::QuickSettings, None);
        assert!(asked.replace(false), "another surface came up");
        overlays.close(Slot::QuickSettings);
        assert!(asked.replace(false), "and went down");
    }

    #[test]
    fn a_dismissed_surface_stays_down_for_the_press_that_dismissed_it() {
        let overlays = overlays();
        overlays.open(Slot::QuickSettings, None);
        overlays.dismiss(Slot::QuickSettings);
        assert!(!overlays.is_open(Slot::QuickSettings));
        assert!(
            overlays.dismissed_recently(Slot::QuickSettings),
            "the press is still being delivered"
        );
        overlays.press_ended();
        assert!(
            overlays.dismissed_recently(Slot::QuickSettings),
            "a quick click comes up before what it asked is carried out: \
             the toggle it raised must not raise it again"
        );
        overlays.settle_press();
        assert!(
            !overlays.dismissed_recently(Slot::QuickSettings),
            "the asks are carried out: the press is over, and it opens again"
        );

        overlays.dismiss(Slot::QuickSettings);
        overlays.open(Slot::QuickSettings, None);
        assert!(
            !overlays.dismissed_recently(Slot::QuickSettings),
            "opened again, it is nobody's dismissed surface"
        );
    }

    #[test]
    fn a_surface_that_says_where_it_stands_is_closed_by_a_press_that_misses_it() {
        let overlays = overlays();
        let panel = Rect::new(52.0, 40.0, 289.0, 240.0);
        assert_eq!(
            overlays.press_outside(640.0, 700.0),
            None,
            "nothing is up, so the press is the desk's"
        );
        overlays.open(Slot::WorkspaceMenu, None);
        assert_eq!(
            overlays.press_outside(640.0, 700.0),
            None,
            "an anchor alone does not say where a menu as big as its rows stands"
        );

        overlays.stands(Slot::WorkspaceMenu, Some(panel));
        assert_eq!(
            overlays.press_outside(640.0, 700.0),
            Some(Slot::WorkspaceMenu),
            "a press on the desk misses the panel"
        );
        assert_eq!(
            overlays.press_outside(panel.center().x, panel.center().y),
            None,
            "a press on the panel is the menu's own"
        );

        overlays.stands(Slot::WorkspaceMenu, None);
        assert_eq!(
            overlays.press_outside(640.0, 700.0),
            None,
            "the widget takes its word back when the menu goes"
        );
    }

    #[test]
    fn an_anchor_reads_back_the_way_a_verb_spells_it() {
        for anchor in [
            Anchor::Center,
            Anchor::Fill,
            Anchor::TopTrailing(8.0),
            Anchor::TopLeading(0.0),
            Anchor::BottomCenter(None),
            Anchor::BottomCenter(Some(Rect::new(1.0, 2.0, 3.0, 4.0))),
            Anchor::Beside {
                at: Rect::new(600.0, 300.0, 602.0, 320.0),
                gap: 4.0,
            },
            Anchor::At(Point::new(40.0, 50.0)),
        ] {
            let text = anchor.as_str();
            assert_eq!(Anchor::parse(&text), Some(anchor), "{text}");
        }
        assert_eq!(Anchor::parse("nowhere"), None);
        assert_eq!(Anchor::parse("at:1"), None);
        assert_eq!(Anchor::parse("beside:1,2,3,4"), None);
    }

    #[test]
    fn the_reading_lists_the_open_surfaces_topmost_first() {
        let overlays = overlays();
        assert!(overlays.entries().is_empty(), "nothing is up");
        overlays.open(Slot::QuickSettings, None);
        overlays.open(Slot::Overview, None);
        let entries = overlays.entries();
        assert_eq!(
            entries.iter().map(|entry| entry.slot).collect::<Vec<_>>(),
            vec![Slot::Overview, Slot::QuickSettings],
            "the overview is the desk the panel stands over, not a second popup"
        );
        let overview = &entries[0];
        assert_eq!(overview.role, Role::Overlay);
        assert_eq!(overview.modality, Modality::WindowModal);
        assert_eq!(overview.lifecycle, Lifecycle::Shown);
        assert_eq!(overview.owner, Owner::Shell);
        assert_eq!(overlays.target().as_str(), "overlay:overview");
    }
}
