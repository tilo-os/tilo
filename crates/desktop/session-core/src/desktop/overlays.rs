//! What stands over the desktop's windows, and in what order.
//! A menu, a picker, a panel that hangs under the bar: each is a surface, and a surface is
//! described once — its kind, where it stands, who may use it and where it is in its life — and
//! everything else follows from that description. `docs/architecture/surfaces.md` is the contract;
//! this module carries it into the session as one [`Overlays`] handle.
//! The stack answers the questions every overlay otherwise answers for itself: which surface the
//! keyboard goes to, what closing the top one restores, where a surface hangs on its display, and
//! what a press outside it closes.
//! Stacking is [`Part::ALL`] order, topmost first: input is offered to the open surfaces in it,
//! assistive technology reads them in it, and Escape closes the first open one. It is not derived
//! from kinds — a display question answered above onboarding is a fact about the session, not about
//! either one's modality.
//! Every part is the session's: the views draw what the stack names, and the session's own verbs
//! open and close every one of them.
//! The stack also holds the shell's surface up ([`Overlay`]), what the person typed in its search,
//! and Alt+Tab's walk: one reading, `desktop:overlays`, says all of it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_clock::time::{Duration, Instant};
use ui::kurbo::{Point, Rect};

/// The height of the shell's bar across the top of a display, logical pixels: popovers stand
/// under it.
pub const TOPBAR_HEIGHT_PX: f32 =
    tilo_ui_base::tokens::Metrics::BASE.bar_control_height + tilo_ui_base::tokens::Spacing::BASE.s1;

/// The bar's height under `theme`: [`TOPBAR_HEIGHT_PX`], grown with a large text size so the
/// time's line still fits with air above and below.
#[must_use]
pub fn topbar_height(theme: &tilo_ui_base::Theme) -> f32 {
    let line = tilo_ui_base::tokens::Typography::default()
        .style(tilo_ui_base::TextRole::Body)
        .line_height_px()
        * theme.text_scale;
    TOPBAR_HEIGHT_PX.max((line + 2.0 * tilo_ui_base::tokens::Spacing::BASE.s1).ceil())
}

/// How long Alt has to stay down after Tab before the switcher's panel shows: shorter than a
/// deliberate look, longer than a practised Alt+Tab, so the common "back to the last window"
/// flick switches windows and flashes nothing.
pub const REVEAL_DELAY: Duration = Duration::from_millis(120);

/// Which windows a walk of the switcher crosses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SwitcherScope {
    /// Every window of the workspace in front: Alt+Tab.
    #[default]
    Workspace,
    /// The windows of the app the window in front shows: Alt and the grave key.
    App,
}

impl SwitcherScope {
    /// The scope `name` names in the walk's action: `workspace`, or `app` for the one in front.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "workspace" | "all" => Self::Workspace,
            "app" => Self::App,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::App => "app",
        }
    }
}

/// The shell's surface over the windows: at rest, or one of the shell's own views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overlay {
    /// The shell at rest, with its bar.
    #[default]
    Rest,
    /// Every strip and window of the workspace, to search and move.
    Overview,
    /// The windows of the workspace, as Alt+Tab walks them.
    Switcher,
    /// Search and launching over the workspace.
    Launcher,
    /// The notifications.
    Centre,
    /// The workspaces the person is in.
    Workspaces,
    /// The agent composer.
    Composer,
    /// The device's quick settings.
    QuickSettings,
    /// The conversation with the session's agent.
    Conversation,
    /// What happened in the workspace, and what the person opened lately: everything, or one
    /// app's or one type's ([`Overlays::history`]).
    History,
    /// Who is here: the roster of the workspace's people and agents, and the card of one of them.
    People,
}

impl Overlay {
    /// The surface `name` names; `None` for one this build has not.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "" | "none" | "shell" => Self::Rest,
            "overview" => Self::Overview,
            "switcher" => Self::Switcher,
            "launcher" => Self::Launcher,
            "centre" => Self::Centre,
            "workspaces" => Self::Workspaces,
            "composer" => Self::Composer,
            "quick-settings" => Self::QuickSettings,
            "conversation" => Self::Conversation,
            "history" => Self::History,
            "people" => Self::People,
            _ => return None,
        })
    }

    /// Its name in the reading, and in the actions that change it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rest => "none",
            Self::Overview => "overview",
            Self::Switcher => "switcher",
            Self::Launcher => "launcher",
            Self::Centre => "centre",
            Self::Workspaces => "workspaces",
            Self::Composer => "composer",
            Self::QuickSettings => "quick-settings",
            Self::Conversation => "conversation",
            Self::History => "history",
            Self::People => "people",
        }
    }
}

/// Every overlay the session puts above the windows, topmost first.
/// Declaration order *is* the stacking order, as it was for the shell's layers: input is offered to
/// open surfaces in it, assistive technology reads them in it, and Escape closes the first open
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    /// The search palette: over everything, the window it was opened from under it.
    Search,
    /// The About sheet: this computer's name and which Tilo it runs.
    About,
    /// The emoji picker.
    Emoji,
    /// The power menu: what this session may do to itself and its machine, under the bar's
    /// trailing end.
    Power,
    /// A window's menu: the rows the shell offers that window.
    WindowMenu,
    /// The menu of the workspace in front, opened from its name on the bar.
    WorkspaceMenu,
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

impl Part {
    /// Every part, topmost first.
    pub const ALL: [Self; 10] = [
        Self::Search,
        Self::About,
        Self::Emoji,
        Self::Power,
        Self::WindowMenu,
        Self::WorkspaceMenu,
        Self::Centre,
        Self::Overview,
        Self::Strips,
        Self::QuickSettings,
    ];

    /// The part's name, as a reading, a view and an agent name it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::About => "about",
            Self::Emoji => "emoji",
            Self::Power => "power",
            Self::WindowMenu => "window-menu",
            Self::WorkspaceMenu => "workspace-menu",
            Self::Centre => "centre",
            Self::Overview => "overview",
            Self::Strips => "strips",
            Self::QuickSettings => "quick-settings",
        }
    }

    /// The part `text` names, as the verbs and the readings spell it.
    #[must_use]
    pub fn named(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|part| part.as_str() == text)
    }

    /// What kind of surface this part is.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            Self::Search | Self::About => Role::Dialog,
            Self::Emoji | Self::Overview | Self::Strips => Role::Overlay,
            Self::Power
            | Self::WindowMenu
            | Self::WorkspaceMenu
            | Self::Centre
            | Self::QuickSettings => Role::Popup,
        }
    }

    /// Whether the surface stands over the shell beside others: a menu, a popover, the strip labels,
    /// the search, quick settings — one at a time. The overview is the shell itself, the surface
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
            Self::Search | Self::About | Self::Emoji => Anchor::Center,
            Self::WindowMenu => Anchor::At(Point::ZERO),
            Self::WorkspaceMenu => Anchor::TopLeading(0.0),
            Self::Power | Self::Centre | Self::QuickSettings => Anchor::TopTrailing(POPOVER_MARGIN),
        }
    }

    /// How much of the display the surface takes: `None` where it is as wide or as tall as its
    /// anchor leaves room for.
    #[must_use]
    pub const fn size(self) -> Size {
        match self {
            // As tall as its page or its rows, down to the display's margin: a
            // page that fits the display is never cut, and only one taller than
            // the display scrolls.
            Self::QuickSettings | Self::Centre | Self::Power => Size {
                width: Some(POPOVER_WIDTH),
                height: None,
            },
            _ => Size {
                width: None,
                height: None,
            },
        }
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

/// The height a popover's window starts at, before it asks for the height of the page it shows.
pub const POPOVER_HEIGHT: f64 = 640.0;

/// The gap between a popover and the edges of its display: the corner margin every surface the
/// shell hangs at a corner keeps (the banners, the notification centre, quick settings), so they
/// line up on one trailing edge.
pub const POPOVER_MARGIN: f64 = 8.0;

/// The kind of thing a surface is: what the person sees over the desktop. The kind fixes
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
}

impl Role {
    /// The role's name, as a reading spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Popup => "popup",
            Self::Overlay => "overlay",
            Self::Dialog => "dialog",
        }
    }

    /// Whether Escape closes a surface of this kind: a menu, a sheet and an overlay do. An overlay
    /// is a mode the person leaves, and Escape is how they leave it.
    #[must_use]
    pub const fn closes_on_escape(self) -> bool {
        matches!(self, Self::Popup | Self::Dialog | Self::Overlay)
    }

    /// Whether a press outside a surface of this kind closes it. A popup does; an overlay and a
    /// sheet stay until they are answered.
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
    /// At the caret `at` : `gap` clear of the line it sits on, below that line while the room
    /// there holds the surface and above it otherwise.
    Beside { at: Rect, gap: f64 },
    /// Over the thing it acts on: the surface covers the display holding the point, and its own
    /// view stands its card there.
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
            Self::Center | Self::TopTrailing(_) | Self::TopLeading(_) | Self::Fill => None,
        }
    }

    /// The anchor as a verb spells it, and as a reading gives it back:
    /// `center`, `fill`, `top-trailing:8`, `top-leading:0`,
    /// `beside:x0,y0,x1,y1,gap`, `at:x,y`.
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

/// What holds the keyboard: the shell's windows, or a surface over them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The window in front: nothing over the shell wants the keyboard.
    Window,
    /// A surface over the shell holds the keyboard.
    Surface(Part),
}

impl Target {
    /// The surface, while one holds the keyboard.
    #[must_use]
    pub const fn part(self) -> Option<Part> {
        match self {
            Self::Window => None,
            Self::Surface(part) => Some(part),
        }
    }

    /// The target's name, as a reading spells it.
    #[must_use]
    pub fn as_str(self) -> String {
        match self {
            Self::Window => "window".into(),
            Self::Surface(part) => format!("overlay:{}", part.as_str()),
        }
    }
}

/// One open surface, as the stack publishes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub part: Part,
    pub role: Role,
    pub anchor: Anchor,
    pub size: Size,
    /// The room the host places it in, when the anchor and its size settle one. A surface its own
    /// view places has none.
    pub place: Option<Placement>,
}

/// One part's state in the stack.
#[derive(Clone, Debug, Default)]
struct PartState {
    open: bool,
    /// Where this opening put it; the part's own anchor while the verb named none.
    anchor: Option<Anchor>,
    /// Where the widget drawing it says it stands, while it is up: a menu is as big as its rows,
    /// which the part's anchor cannot say.
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
    /// straddles the border.
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

/// Where the stack stands a surface it places: the room the surface may take on its display, and
/// the edge of that room its content rests against as it fits inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The room the surface may take: it fills it along the edges its anchor pins, and is as big as
    /// its own content inside it.
    pub room: Rect,
    /// Whether its content rests against the room's bottom edge: a surface opened above the caret
    /// it edits hangs by its bottom there, so a page shorter than the room stands clear of the
    /// caret's line instead of at the top of the room.
    pub rests_at_bottom: bool,
    /// Where it comes from and goes back to: the bar it hangs from, the caret it was opened at,
    /// or the display's middle.
    pub from: Point,
}

/// Where a surface stands on `display` , from its anchor and the room its size leaves; `None` for one
/// whose size and anchor do not settle a rect.
#[must_use]
pub fn place_on(display: Rect, anchor: Anchor, size: Size) -> Option<Placement> {
    place_under_bar(display, anchor, size, TOPBAR_HEIGHT_PX)
}

/// [`place_on`], under a bar `bar` logical pixels tall: the height the shell's own chrome was laid
/// out at, which its bar size setting moves, rather than the reference one.
#[must_use]
pub fn place_under_bar(display: Rect, anchor: Anchor, size: Size, bar: f32) -> Option<Placement> {
    // The bar hangs above a surface anchored to the top edge.
    let top = display.y0 + f64::from(bar).ceil();
    let margin = POPOVER_MARGIN;
    let width = size
        .width
        .unwrap_or_else(|| (display.width() - 2.0 * margin).max(1.0));
    let height = size.height;
    // Only a surface opened at a caret may hang by the room's bottom edge: every other one is
    // placed by its anchor and rests at the room's top, or fills it.
    let mut rests_at_bottom = false;
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
        // Over the thing it acts on, the surface covers the display and its view stands the card.
        Anchor::Fill | Anchor::At(_) => display,
        // Beside the rect it was opened on, clear of the line the person edits: the surface takes
        // the room below that line while that holds as much as it may be, and while it is the
        // roomier side; above the line only when it is the roomier side and the room below cannot
        // hold the surface whole. Either way it never covers the line.
        Anchor::Beside { at, gap } => {
            let x0 = if at.x1 + gap + width <= display.x1 - margin {
                at.x1 + gap
            } else {
                (at.x0 - gap - width).max(display.x0 + margin)
            };
            let (top, bottom) = (display.y0 + margin, display.y1 - margin);
            let below = (at.y1 + gap).max(top);
            let above = (at.y0 - gap).min(bottom);
            let wanted = height.unwrap_or_else(|| (bottom - top).max(1.0));
            let room_below = (bottom - below).max(0.0);
            let room_above = (above - top).max(0.0);
            if room_below >= wanted || room_below >= room_above {
                Rect::new(x0, below, x0 + width, below + room_below.min(wanted))
            } else {
                // It hangs by its bottom edge from the room's, so a page shorter than the room
                // stands just clear of the caret's line.
                rests_at_bottom = true;
                Rect::new(x0, above - room_above.min(wanted), x0 + width, above)
            }
        }
    };
    let from = match anchor {
        Anchor::TopTrailing(_) => Point::new(rect.x1, display.y0),
        Anchor::TopLeading(_) => Point::new(rect.x0, display.y0),
        Anchor::Beside { at, .. } if rests_at_bottom => Point::new(at.x0, at.y0),
        Anchor::Beside { at, .. } => Point::new(at.x0, at.y1),
        Anchor::At(point) => point,
        Anchor::Center | Anchor::Fill => rect.center(),
    };
    Some(Placement {
        room: rect,
        rests_at_bottom,
        from,
    })
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
    pub fn wake_with(&self, wake: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        *self.0.borrow_mut().wake.borrow_mut() = Some(wake);
    }

    /// Tell the stack how tall the shell's bar is, so a surface anchored to the display's top
    /// edge hangs under the bar the person sees rather than under the reference height.
    pub fn set_bar_height(&self, height: f32) {
        let mut stack = self.0.borrow_mut();
        if stack.bar_height != Some(height) {
            stack.bar_height = Some(height);
            stack.moved();
        }
    }

    /// The stack changed under what draws it: let it take up a pass.
    fn woke(&self) {
        if let Some(wake) = self.0.borrow().wake.borrow().as_ref() {
            wake.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// The shell's surface up now.
    #[must_use]
    pub fn here(&self) -> Overlay {
        self.0.borrow().here
    }

    /// What the person typed in the shell's search: the launcher opens where they left it.
    #[must_use]
    pub fn spotlight(&self) -> String {
        self.0.borrow().spotlight.clone()
    }

    /// Moves whenever the shell's surface or the switcher's walk changes.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.borrow().revision
    }

    /// Show `surface`. The switcher opens on the window before the one in front, as a quick
    /// Alt+Tab does.
    pub fn show(&self, surface: Overlay) {
        let mut stack = self.0.borrow_mut();
        if stack.here == surface {
            return;
        }
        stack.here = surface;
        // The history opened as a surface of its own is the whole workspace's; a scoped one is
        // opened through [`Self::history`], which names its scope after.
        stack.history_app.clear();
        stack.history_kind.clear();
        if surface == Overlay::Switcher {
            stack.opened = Some(tilo_clock::now());
            stack.scope = SwitcherScope::Workspace;
            stack.walk = 1;
        } else {
            stack.opened = None;
        }
        stack.moved();
    }

    /// Take `surface` down, when it is the one up.
    pub fn hide(&self, surface: Overlay) {
        if self.here() == surface {
            self.show(Overlay::Rest);
        }
    }

    /// Open the launcher on `query`, whether it was up or not.
    pub fn search(&self, query: &str) {
        query.clone_into(&mut self.0.borrow_mut().spotlight);
        self.show(Overlay::Launcher);
        self.0.borrow_mut().moved();
    }

    /// Forget what the person typed in the shell's search: the launcher keeps it for its own
    /// next open, but a surface that shows the windows is not filtered by a search that has
    /// done its work somewhere else.
    pub fn forget_spotlight(&self) {
        let mut stack = self.0.borrow_mut();
        if stack.spotlight.is_empty() {
            return;
        }
        stack.spotlight.clear();
        stack.moved();
    }

    /// Open the history on what the person opened lately of `app` (its canonical id) or of `kind`
    /// (a type as its app publishes it); both empty shows everything.
    pub fn history(&self, app: &str, kind: &str) {
        self.show(Overlay::History);
        let mut stack = self.0.borrow_mut();
        app.clone_into(&mut stack.history_app);
        kind.clone_into(&mut stack.history_kind);
        stack.moved();
    }

    /// The app and the type the history is narrowed to, empty for none.
    #[must_use]
    pub fn history_scope(&self) -> (String, String) {
        let stack = self.0.borrow();
        (stack.history_app.clone(), stack.history_kind.clone())
    }

    /// The walk starts again from the window in front, whichever surface was up.
    pub fn forget_walk(&self) {
        let mut stack = self.0.borrow_mut();
        stack.walk = 0;
        stack.opened = None;
    }

    /// Alt+Tab's press: open the switcher in `scope` on the window `steps` from the one in front,
    /// or walk it `steps` windows on while it is already up in that scope. A press in the other
    /// scope starts the walk again from the window in front.
    pub fn walk(&self, scope: SwitcherScope, steps: i64) {
        let mut stack = self.0.borrow_mut();
        let walking = stack.here == Overlay::Switcher && stack.scope == scope;
        stack.here = Overlay::Switcher;
        stack.scope = scope;
        if walking {
            stack.walk = stack.walk.saturating_add(steps);
        } else {
            stack.opened = Some(tilo_clock::now());
            stack.walk = steps;
        }
        stack.moved();
    }

    /// Choose the window `at` windows past the one in front: Home, End and a card under the
    /// pointer choose by place instead of crossing the windows one at a time.
    pub fn select(&self, at: i64) {
        let mut stack = self.0.borrow_mut();
        if stack.here != Overlay::Switcher {
            return;
        }
        stack.walk = at;
        stack.moved();
    }

    /// Which windows the walk crosses.
    #[must_use]
    pub fn scope(&self) -> SwitcherScope {
        self.0.borrow().scope
    }

    /// Whether the switcher's walk is up: Alt went down with Tab and has not come up since. Its
    /// panel need not show yet ([`Self::revealed`]).
    #[must_use]
    pub fn switcher_up(&self) -> bool {
        self.0.borrow().here == Overlay::Switcher
    }

    /// How far the walk stands from the window in front.
    #[must_use]
    pub fn walked(&self) -> i64 {
        self.0.borrow().walk
    }

    /// Where the walk has reached of `count` windows, `front` being the window in front's place
    /// among them: `None` while the switcher is not up. When no window is in front (`front` is
    /// `None`), the walk starts just before index `0`, so the first forward step (`walk = 1`)
    /// reaches index `0`.
    #[must_use]
    pub fn reached(&self, count: usize, front: Option<usize>) -> Option<usize> {
        if count == 0 || !self.switcher_up() {
            return None;
        }
        let origin = front.map_or(-1, |front| i64::try_from(front).unwrap_or(0));
        let count = i64::try_from(count).unwrap_or(1).max(1);
        let at = origin.saturating_add(self.walked()).rem_euclid(count);
        usize::try_from(at).ok()
    }

    /// Whether the switcher's panel shows: Alt has been held past [`REVEAL_DELAY`] since the walk
    /// began, so a quick flick switched windows without a panel.
    #[must_use]
    pub fn revealed(&self, now: Instant) -> bool {
        self.0.borrow().revealed(now)
    }

    /// When the switcher's panel comes up, while Alt is held on it and it has not yet.
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        let stack = self.0.borrow();
        let opened = stack.opened?;
        (stack.here == Overlay::Switcher && !stack.revealed(now))
            .then(|| opened.checked_add(REVEAL_DELAY))
            .flatten()
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

    /// The display the person works on, as the desktop last placed its surfaces on it: the one
    /// every surface without a place of its own stands on. `None` while the shell spans one display.
    #[must_use]
    pub fn focused_display(&self) -> Option<Rect> {
        self.0.borrow().displays.focused
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

    /// Where the surface in `part` was last seen, as its app drew it: the size a panel
    /// that fits its content asked for, which is shorter than the room when its page is.
    #[must_use]
    pub fn surface(&self, part: Part) -> Option<Rect> {
        self.0
            .borrow()
            .parts
            .get(&part)
            .and_then(|state| state.stands)
    }

    /// Put `part` up, at `anchor` or where the part stands by itself.
    /// One anchored surface at a time: the person looks at the one they just opened, so the others
    /// go down with it. Their widgets follow the stack ([`Self::is_open`]), which is how the chrome
    /// learns to close them.
    pub fn open(&self, part: Part, anchor: Option<Anchor>) {
        self.raise(part, anchor, false);
    }

    /// Put `part` up over the surface the person types in, at `anchor`: what is opened
    /// for that field (the clipboard, to paste into it) leaves the field's surface up under it,
    /// takes the keyboard from it while it stands, and gives it back as it goes. Every other
    /// anchored surface goes down, as [`Self::open`] takes them.
    pub fn open_over(&self, part: Part, anchor: Option<Anchor>) {
        self.raise(part, anchor, true);
    }

    fn raise(&self, part: Part, anchor: Option<Anchor>, over: bool) {
        {
            let mut stack = self.0.borrow_mut();
            let typed_in = over.then(|| stack.target()).flatten();
            if part.stands_beside() {
                for other in Part::ALL {
                    if other != part
                        && Some(other) != typed_in
                        && other.stands_beside()
                        && let Some(state) = stack.parts.get_mut(&other)
                        && state.open
                    {
                        state.open = false;
                        state.awaiting_close = true;
                    }
                }
            }
            let state = stack.parts.entry(part).or_default();
            state.open = true;
            state.anchor = Some(anchor.unwrap_or_else(|| part.anchor()));
            state.dismissed = false;
            state.awaiting_close = false;
            if over {
                stack.over = Some(part);
            } else if stack.over == Some(part) {
                stack.over = None;
            }
        }
        self.woke();
    }

    /// Take `part` down, as the widget drawing it does once the stack has: what the stack took down
    /// is settled by this, and a part that is already down moved nothing.
    pub fn close(&self, part: Part) {
        {
            let mut stack = self.0.borrow_mut();
            let Some(state) = stack.parts.get_mut(&part) else {
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

    /// Say where the widget drawing `part` paints it, while it is up.
    ///
    /// A surface the session places is judged by the rect its anchor and size
    /// settle; the chrome's own menus hang off the bar as big as their rows,
    /// which only the widget knows. `None` takes back the last answer, as a
    /// surface going down does.
    pub fn stands(&self, part: Part, stands: Option<Rect>) {
        let mut stack = self.0.borrow_mut();
        match stack.parts.get_mut(&part) {
            Some(state) => state.stands = stands,
            None if stands.is_some() => stack.parts.entry(part).or_default().stands = stands,
            None => {}
        }
    }

    /// A press outside `part` closed it: it stays down for that press, which is still being
    /// delivered.
    pub fn dismiss(&self, part: Part) {
        {
            let mut stack = self.0.borrow_mut();
            let state = stack.parts.entry(part).or_default();
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
        for state in stack.parts.values_mut() {
            state.dismissed = false;
        }
    }

    /// Whether `part` was dismissed by a press outside that is still being delivered, so nothing
    /// that press raised may put it back up.
    #[must_use]
    pub fn dismissed_recently(&self, part: Part) -> bool {
        self.0
            .borrow()
            .parts
            .get(&part)
            .is_some_and(|state| state.dismissed)
    }

    /// Whether `part` was taken down by the stack and the widget drawing it has yet to follow: its
    /// open state is the one being left behind, not a new opening to put back up.
    #[must_use]
    pub fn awaiting_close(&self, part: Part) -> bool {
        self.0
            .borrow()
            .parts
            .get(&part)
            .is_some_and(|state| state.awaiting_close)
    }

    /// Whether `part` is up.
    #[must_use]
    pub fn is_open(&self, part: Part) -> bool {
        self.0
            .borrow()
            .parts
            .get(&part)
            .is_some_and(|state| state.open)
    }

    /// What holds the keyboard: the topmost open surface that takes it, else the window in front.
    /// Closing a surface derives this again, which is how the one it covered takes the keyboard
    /// back.
    #[must_use]
    pub fn target(&self) -> Target {
        self.0
            .borrow()
            .target()
            .map_or(Target::Window, Target::Surface)
    }

    /// The surface opened over the one the person types in ([`Self::open_over`]), while it stands:
    /// it holds the keyboard whatever stands under it.
    #[must_use]
    pub fn over(&self) -> Option<Part> {
        self.0.borrow().over()
    }

    /// Where `part` stands now, if its anchor and size settle a room the host can place it in.
    #[must_use]
    pub fn place(&self, part: Part) -> Option<Placement> {
        self.0.borrow().entry(part).and_then(|entry| entry.place)
    }

    /// The surfaces open now, topmost first.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        let stack = self.0.borrow();
        Part::ALL
            .into_iter()
            .filter_map(|part| stack.entry(part))
            .collect()
    }

    /// The surface Escape closes, by the dismissal table: the topmost one whose kind answers Escape
    /// and that the session may close. One opened over the field the person types in
    /// ([`Self::open_over`]) stands above the surface it was opened for, so Escape puts it away
    /// first and leaves that surface up.
    #[must_use]
    pub fn escape(&self) -> Option<Part> {
        let stack = self.0.borrow();
        let closes = |part: &Part| part.role().closes_on_escape();
        stack.over().filter(closes).or_else(|| {
            Part::ALL
                .into_iter()
                .find(|part| closes(part) && stack.is_shown(*part))
        })
    }

    /// The surface a press at this point closes, by the dismissal table: the topmost popup
    /// where the point is not. What the session placed is judged by the rect its anchor settles; a
    /// surface its own widget draws says where it stands, and one that says neither is its view's
    /// to close.
    #[must_use]
    pub fn press_outside(&self, x: f64, y: f64) -> Option<Part> {
        let stack = self.0.borrow();
        Part::ALL.into_iter().find(|part| {
            if !part.role().closes_on_press_outside() {
                return false;
            }
            let Some(state) = stack.parts.get(part).filter(|state| state.open) else {
                return false;
            };
            let place = match state.stands {
                Some(place) => Some(place),
                None => stack
                    .entry(*part)
                    .and_then(|entry| entry.place)
                    .map(|placement| placement.room),
            };
            place.is_some_and(|place| !place.contains(Point::new(x, y)))
        })
    }
}

/// The stack behind the handle.
#[derive(Clone, Debug, Default)]
struct Stack {
    parts: BTreeMap<Part, PartState>,
    displays: Displays,
    /// The surface last opened over the one the person types in ([`Overlays::open_over`]): while
    /// it is up it holds the keyboard over that surface, which stays up under it.
    over: Option<Part>,
    /// Whether the button of the press that dismissed something has come up,
    /// and what it asked is yet to be carried out.
    press_over: bool,
    /// What draws the surfaces the stack holds, asked for its next pass whenever the stack changes
    /// under it ([`Overlays::wake_with`]).
    wake: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    /// The shell's surface up.
    here: Overlay,
    revision: u64,
    spotlight: String,
    /// The app and the type the history was opened on ([`Overlays::history`]).
    history_app: String,
    history_kind: String,
    /// How far Alt+Tab has walked from the window in front.
    walk: i64,
    scope: SwitcherScope,
    /// When the walk began, which decides when its panel shows ([`REVEAL_DELAY`]).
    opened: Option<Instant>,
    /// How tall the shell's bar is, in logical pixels: a surface anchored to the top edge hangs
    /// under the bar the person sees. `None` until the session tells it, when a surface stands
    /// under the reference height.
    bar_height: Option<f32>,
}

impl Stack {
    fn moved(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn revealed(&self, now: Instant) -> bool {
        self.here == Overlay::Switcher
            && self
                .opened
                .is_some_and(|at| now.saturating_duration_since(at) >= REVEAL_DELAY)
    }

    /// The surface that holds the keyboard: one opened over the field the person types in, else
    /// the topmost open one that takes it.
    fn target(&self) -> Option<Part> {
        self.over()
            .or_else(|| Part::ALL.into_iter().find(|part| self.is_shown(*part)))
    }

    /// The open surface opened over the one the person types in.
    fn over(&self) -> Option<Part> {
        self.over.filter(|part| self.is_shown(*part))
    }

    /// Whether `part` is up and taking input.
    fn is_shown(&self, part: Part) -> bool {
        self.parts.get(&part).is_some_and(|state| state.open)
    }

    /// The open surface `part` is, with where it stands now.
    fn entry(&self, part: Part) -> Option<Entry> {
        let state = self.parts.get(&part)?;
        if !state.open {
            return None;
        }
        let anchor = state.anchor.unwrap_or_else(|| part.anchor());
        let size = part.size();
        let place = self
            .displays
            .display_for(anchor.display_of())
            .and_then(|display| {
                place_under_bar(
                    display,
                    anchor,
                    size,
                    self.bar_height.unwrap_or(TOPBAR_HEIGHT_PX),
                )
            });
        Some(Entry {
            part,
            role: part.role(),
            anchor,
            size,
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

    tilo_test_env::scenario! {
        fn the_overlay_stack_opens_closes_and_hands_on_the_keyboard;

        phase every_part_says_what_surface_it_is;
        {
            for part in Part::ALL {
                let role = part.role();
                assert!(
                    !role.as_str().is_empty() && !part.as_str().is_empty(),
                    "{part:?} is named"
                );
                assert_eq!(Part::named(part.as_str()), Some(part), "{part:?}");
            }
        }

        phase only_a_menu_closes_on_a_press_outside_and_escape_answers_by_kind;
        {
            assert!(Role::Popup.closes_on_press_outside());
            for role in [Role::Overlay, Role::Dialog] {
                assert!(!role.closes_on_press_outside(), "{role:?} stays");
            }
            assert!(Role::Popup.closes_on_escape() && Role::Dialog.closes_on_escape());
            assert!(
                Role::Overlay.closes_on_escape(),
                "an overlay is a mode the person leaves, and Escape leaves it"
            );
        }

        phase one_anchored_surface_stands_at_a_time;
        {
            let overlays = overlays();
            overlays.open(Part::WorkspaceMenu, None);
            overlays.open(Part::Strips, None);
            assert!(
                !overlays.is_open(Part::WorkspaceMenu),
                "the strip labels came up, and the menu went down with them"
            );
            overlays.open(Part::QuickSettings, None);
            assert!(
                !overlays.is_open(Part::Strips),
                "and quick settings after them"
            );
            assert!(overlays.is_open(Part::QuickSettings));
            // The overview is the shell the others stand over, not one of them.
            overlays.open(Part::Overview, None);
            assert!(
                overlays.is_open(Part::QuickSettings),
                "the overview keeps what stands over it"
            );
            overlays.open(Part::Strips, None);
            assert!(
                overlays.is_open(Part::Overview),
                "and the strip labels leave the overview itself up"
            );
        }

        phase the_keyboard_goes_to_the_topmost_surface_and_comes_back_down;
        {
            let overlays = overlays();
            assert_eq!(overlays.target(), Target::Window, "the shell has it");

            overlays.open(Part::Overview, None);
            assert_eq!(overlays.target(), Target::Surface(Part::Overview));
            overlays.open(Part::QuickSettings, None);
            assert_eq!(
                overlays.target(),
                Target::Surface(Part::Overview),
                "the top of the stack holds it, not the last one opened"
            );

            overlays.close(Part::Overview);
            assert_eq!(
                overlays.target(),
                Target::Surface(Part::QuickSettings),
                "the one it covered takes it back"
            );
            overlays.close(Part::QuickSettings);
            assert_eq!(overlays.target(), Target::Window, "and the window again");
        }

        phase a_surface_opened_over_the_field_typed_in_leaves_it_up_and_gives_the_keys_back;
        {
            let overlays = overlays();
            overlays.open(Part::Search, None);
            overlays.open_over(Part::QuickSettings, None);
            assert!(
                overlays.is_open(Part::Search),
                "the search the clipboard was opened for stays up under it"
            );
            assert_eq!(overlays.target(), Target::Surface(Part::QuickSettings));
            assert_eq!(overlays.over(), Some(Part::QuickSettings));
            assert_eq!(
                overlays.escape(),
                Some(Part::QuickSettings),
                "Escape puts the clipboard away, not the search"
            );
            overlays.close(Part::QuickSettings);
            assert_eq!(overlays.over(), None);
            assert_eq!(
                overlays.target(),
                Target::Surface(Part::Search),
                "the search has the keys back"
            );

            // Opened plainly, it stands alone.
            overlays.open_over(Part::QuickSettings, None);
            overlays.open(Part::QuickSettings, None);
            assert!(!overlays.is_open(Part::Search));
            assert_eq!(overlays.over(), None);
        }

        phase escape_and_a_press_outside_close_the_top_menu_that_stands_where_it_is_not;
        {
            let overlays = overlays();
            overlays.open(Part::QuickSettings, None);
            let place = overlays.place(Part::QuickSettings).expect("placed").room;

            assert_eq!(overlays.escape(), Some(Part::QuickSettings));
            assert_eq!(
                overlays.press_outside(4.0, DISPLAY.y1 - 4.0),
                Some(Part::QuickSettings),
                "a press outside its rect"
            );
            assert_eq!(
                overlays.press_outside(place.center().x, place.center().y),
                None,
                "and none from inside it"
            );

            // The window menu takes its place: one anchored surface at a time.
            overlays.open(Part::WindowMenu, Some(Anchor::At(Point::new(40.0, 50.0))));
            assert!(
                !overlays.is_open(Part::QuickSettings),
                "the panel goes down for the menu the person just opened"
            );
            // It covers the display it was opened on, and its own view stands the card at the
            // point: a press on that display is its view's to judge.
            assert_eq!(
                overlays.place(Part::WindowMenu).map(|placement| placement.room),
                Some(DISPLAY)
            );
            assert_eq!(overlays.escape(), Some(Part::WindowMenu));
            assert_eq!(
                overlays.press_outside(4.0, DISPLAY.y1 - 4.0),
                None,
                "its view closes it on its own display"
            );
        }

        phase a_change_in_the_stack_asks_what_draws_it_for_a_pass;
        {
            let overlays = overlays();
            let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            overlays.wake_with(std::sync::Arc::clone(&asked));
            assert!(!asked.swap(false, std::sync::atomic::Ordering::Relaxed), "nothing asked yet");

            overlays.open(Part::WorkspaceMenu, None);
            assert!(asked.swap(false, std::sync::atomic::Ordering::Relaxed), "the menu came up");
            overlays.dismiss(Part::WorkspaceMenu);
            assert!(
                asked.swap(false, std::sync::atomic::Ordering::Relaxed),
                "a press outside took it down: the widget has to close it"
            );
            // The widget closing it follows the stack, which hears that: what the widget shows next is
            // a surface of its own again, not the state the stack took down.
            overlays.close(Part::WorkspaceMenu);
            assert!(
                !overlays.awaiting_close(Part::WorkspaceMenu),
                "the widget followed the stack"
            );
            assert!(asked.swap(false, std::sync::atomic::Ordering::Relaxed), "the widget followed it");
            assert!(
                !asked.swap(false, std::sync::atomic::Ordering::Relaxed),
                "and nothing is left to draw after that"
            );
            overlays.open(Part::QuickSettings, None);
            assert!(asked.swap(false, std::sync::atomic::Ordering::Relaxed), "another surface came up");
            overlays.close(Part::QuickSettings);
            assert!(asked.swap(false, std::sync::atomic::Ordering::Relaxed), "and went down");
        }

        phase a_dismissed_surface_stays_down_for_the_press_that_dismissed_it;
        {
            let overlays = overlays();
            overlays.open(Part::QuickSettings, None);
            overlays.dismiss(Part::QuickSettings);
            assert!(!overlays.is_open(Part::QuickSettings));
            assert!(
                overlays.dismissed_recently(Part::QuickSettings),
                "the press is still being delivered"
            );
            overlays.press_ended();
            assert!(
                overlays.dismissed_recently(Part::QuickSettings),
                "a quick click comes up before what it asked is carried out: \
             the toggle it raised must not raise it again"
            );
            overlays.settle_press();
            assert!(
                !overlays.dismissed_recently(Part::QuickSettings),
                "the asks are carried out: the press is over, and it opens again"
            );

            overlays.dismiss(Part::QuickSettings);
            overlays.open(Part::QuickSettings, None);
            assert!(
                !overlays.dismissed_recently(Part::QuickSettings),
                "opened again, it is nobody's dismissed surface"
            );
        }

        phase a_surface_that_says_where_it_stands_is_closed_by_a_press_that_misses_it;
        {
            let overlays = overlays();
            let panel = Rect::new(52.0, 40.0, 289.0, 240.0);
            assert_eq!(
                overlays.press_outside(640.0, 700.0),
                None,
                "nothing is up, so the press is the shell's"
            );
            overlays.open(Part::WorkspaceMenu, None);
            assert_eq!(
                overlays.press_outside(640.0, 700.0),
                None,
                "an anchor alone does not say where a menu as big as its rows stands"
            );

            overlays.stands(Part::WorkspaceMenu, Some(panel));
            assert_eq!(
                overlays.press_outside(640.0, 700.0),
                Some(Part::WorkspaceMenu),
                "a press on the shell misses the panel"
            );
            assert_eq!(
                overlays.press_outside(panel.center().x, panel.center().y),
                None,
                "a press on the panel is the menu's own"
            );

            overlays.stands(Part::WorkspaceMenu, None);
            assert_eq!(
                overlays.press_outside(640.0, 700.0),
                None,
                "the widget takes its word back when the menu goes"
            );
        }

        phase the_reading_lists_the_open_surfaces_topmost_first;
        {
            let overlays = overlays();
            assert!(overlays.entries().is_empty(), "nothing is up");
            overlays.open(Part::QuickSettings, None);
            overlays.open(Part::Overview, None);
            let entries = overlays.entries();
            assert_eq!(
                entries.iter().map(|entry| entry.part).collect::<Vec<_>>(),
                vec![Part::Overview, Part::QuickSettings],
                "the overview is the shell the panel stands over, not a second popup"
            );
            let overview = &entries[0];
            assert_eq!(overview.role, Role::Overlay);
            assert_eq!(overlays.target().as_str(), "overlay:overview");
        }
    }

    tilo_test_env::scenario! {
        fn an_overlay_stands_where_its_anchor_puts_it;

        phase a_popover_under_the_bars_right_end_leaves_the_margin_and_the_bar;
        {
            let overlays = overlays();
            overlays.open(Part::QuickSettings, None);
            let place = overlays.place(Part::QuickSettings).expect("placed").room;
            let bar = f64::from(TOPBAR_HEIGHT_PX).ceil();
            assert_eq!(place.x1, DISPLAY.x1 - POPOVER_MARGIN);
            assert_eq!(place.x0, DISPLAY.x1 - POPOVER_MARGIN - POPOVER_WIDTH);
            assert_eq!(place.y0, bar, "under the bar");
            assert_eq!(place.width(), POPOVER_WIDTH);
            assert_eq!(
                place.y1,
                DISPLAY.y1 - POPOVER_MARGIN,
                "its room reaches the display's margin: the page it shows is never cut short of it"
            );
        }

        phase a_popover_opens_beside_the_caret_it_was_asked_at;
        {
            let overlays = overlays();
            let caret = Rect::new(600.0, 300.0, 602.0, 320.0);
            overlays.open(
                Part::QuickSettings,
                Some(Anchor::Beside {
                    at: caret,
                    gap: 4.0,
                }),
            );
            let placement = overlays.place(Part::QuickSettings).expect("placed");
            assert_eq!(placement.room.x0, caret.x1 + 4.0, "clear of the caret");
            assert!(placement.room.x0 >= caret.x1, "and never over it");
            assert_eq!(placement.room.width(), POPOVER_WIDTH);
            assert_eq!(
                placement.room.y0,
                caret.y1 + 4.0,
                "under the line the person types on"
            );
            assert_eq!(
                placement.room.y1,
                DISPLAY.y1 - POPOVER_MARGIN,
                "taking the room below it, down to the display's margin"
            );
            assert!(!placement.rests_at_bottom, "hanging from its top edge");

            // With no room on the right of the caret, it opens on the left.
            let edge = Rect::new(1200.0, 300.0, 1202.0, 320.0);
            overlays.open(
                Part::QuickSettings,
                Some(Anchor::Beside { at: edge, gap: 4.0 }),
            );
            let placement = overlays.place(Part::QuickSettings).expect("placed");
            assert_eq!(placement.room.x1, edge.x0 - 4.0);
            assert!(placement.room.x1 <= edge.x0, "still clear of the caret");
        }

        phase a_pane_at_the_caret_never_covers_the_line_the_person_edits;
        {
            let overlays = overlays();
            // Halfway down: the room below the line holds the whole pane, so it goes there.
            let middle = Rect::new(600.0, 200.0, 602.0, 220.0);
            overlays.open(
                Part::QuickSettings,
                Some(Anchor::Beside {
                    at: middle,
                    gap: 6.0,
                }),
            );
            let placement = overlays.place(Part::QuickSettings).expect("placed");
            assert_eq!(placement.room.y0, middle.y1 + 6.0, "just under the line");
            assert_eq!(
                placement.room.y1,
                DISPLAY.y1 - POPOVER_MARGIN,
                "as far down as the display's margin"
            );
            assert!(!placement.rests_at_bottom, "hanging from its top edge");
            assert!(
                placement.room.y0 >= middle.y1,
                "the pane stands clear of the line: {:?} against {middle:?}",
                placement.room
            );

            // Near the bottom: no room below the line holds it, so it hangs above, by its bottom edge,
            // and still covers nothing of the line.
            let low = Rect::new(600.0, 700.0, 602.0, 720.0);
            overlays.open(
                Part::QuickSettings,
                Some(Anchor::Beside { at: low, gap: 6.0 }),
            );
            let placement = overlays.place(Part::QuickSettings).expect("placed");
            assert_eq!(placement.room.y1, low.y0 - 6.0, "just over the line");
            assert!(placement.rests_at_bottom, "hanging by its bottom edge");
            assert_eq!(
                placement.room.y0,
                DISPLAY.y0 + POPOVER_MARGIN,
                "the room above it, up to the display's margin"
            );
            assert!(
                placement.room.y1 <= low.y0,
                "the pane stands clear of the line: {:?} against {low:?}",
                placement.room
            );
        }

        phase the_caret_a_key_opens_at_outlives_the_displays_changing;
        {
            let overlays = overlays();
            let caret = Rect::new(600.0, 300.0, 602.0, 320.0);
            overlays.set_caret(Some(caret));
            // The outputs came and went while the person typed: where they type is still where the
            // surface a key opens for the text stands.
            overlays.set_displays(&[Rect::new(0.0, 0.0, 1000.0, 700.0)], Some(DISPLAY));
            assert_eq!(overlays.caret(), Some(caret));
            overlays.open(
                Part::QuickSettings,
                overlays.caret().map(|at| Anchor::Beside { at, gap: 6.0 }),
            );
            let placement = overlays.place(Part::QuickSettings).expect("placed");
            assert_eq!(placement.room.x0, caret.x1 + 6.0, "clear of the caret");
            assert!(
                placement.room.y0 >= caret.y1,
                "under the line it types on, never over it"
            );
            assert_eq!(
                placement.room.height(),
                700.0 - POPOVER_MARGIN - caret.y1 - 6.0,
                "the room below the line, which is less than the surface may be"
            );
        }

        phase a_surface_is_placed_on_the_display_holding_its_anchor;
        {
            let left = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let right = Rect::new(1280.0, 0.0, 3840.0, 1440.0);
            let overlays = Overlays::new();
            overlays.set_displays(&[left, right], Some(left));
            // On the right display's trailing edge, though the person works on the left one.
            overlays.open(Part::QuickSettings, Some(Anchor::TopTrailing(8.0)));
            let place = overlays.place(Part::QuickSettings).expect("placed").room;
            assert_eq!(place.x1, left.x1 - 8.0, "the display the person works on");

            let caret = Rect::new(2000.0, 100.0, 2002.0, 120.0);
            overlays.open(
                Part::QuickSettings,
                Some(Anchor::Beside {
                    at: caret,
                    gap: 0.0,
                }),
            );
            let place = overlays.place(Part::QuickSettings).expect("placed").room;
            assert!(
                place.x0 >= right.x0,
                "the display holding the caret it was opened from"
            );

            // A window's menu opened on the right display covers that display, from the point the
            // person pressed, though the person worked on the left one.
            let pressed = Point::new(1800.0, 60.0);
            overlays.open(Part::WindowMenu, Some(Anchor::At(pressed)));
            let placement = overlays.place(Part::WindowMenu).expect("placed");
            assert_eq!(placement.room, right, "the display holding the press");
            assert_eq!(placement.from, pressed);
            assert_eq!(
                overlays.press_outside(100.0, 100.0),
                Some(Part::WindowMenu),
                "a press on the other display closes it"
            );
        }

        phase an_anchor_reads_back_the_way_a_verb_spells_it;
        {
            for anchor in [
                Anchor::Center,
                Anchor::Fill,
                Anchor::TopTrailing(8.0),
                Anchor::TopLeading(0.0),
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
    }

    #[test]
    fn a_shell_surface_shows_and_hides_by_name() {
        let overlays = Overlays::new();
        assert_eq!(overlays.here(), Overlay::Rest);
        overlays.show(Overlay::Overview);
        assert_eq!(overlays.here(), Overlay::Overview);
        overlays.hide(Overlay::Launcher);
        assert_eq!(overlays.here(), Overlay::Overview, "another one stays up");
        overlays.hide(Overlay::Overview);
        assert_eq!(overlays.here(), Overlay::Rest);
        overlays.search("quotes");
        assert_eq!(overlays.here(), Overlay::Launcher);
        assert_eq!(overlays.spotlight(), "quotes");
        overlays.forget_spotlight();
        assert_eq!(overlays.spotlight(), "");
        assert_eq!(
            overlays.here(),
            Overlay::Launcher,
            "forgetting the query leaves the launcher where it stands"
        );
        for name in [
            "none",
            "shell",
            "overview",
            "switcher",
            "launcher",
            "centre",
            "workspaces",
            "composer",
            "quick-settings",
            "conversation",
        ] {
            let surface = Overlay::named(name).unwrap_or_else(|| panic!("{name} is a surface"));
            assert!(Overlay::named(surface.name()).is_some());
        }
        assert!(Overlay::named("gallery").is_none());
    }

    #[test]
    fn a_flick_shows_nothing_and_holding_reveals_the_panel() {
        let overlays = Overlays::new();
        let opened = tilo_clock::now();
        overlays.walk(SwitcherScope::Workspace, 1);
        assert_eq!(overlays.here(), Overlay::Switcher);
        assert!(!overlays.revealed(opened + Duration::from_millis(100)));
        let deadline = overlays
            .deadline(opened + Duration::from_millis(100))
            .expect("one wake is due when the delay is up");
        assert!(deadline >= opened && deadline <= opened + Duration::from_millis(200));
        assert!(overlays.revealed(opened + Duration::from_millis(200)));
        assert_eq!(overlays.deadline(opened + Duration::from_millis(200)), None);
        overlays.forget_walk();
        overlays.show(Overlay::Rest);
        assert!(!overlays.revealed(opened + Duration::from_secs(60)));
    }

    #[test]
    fn the_walk_carries_its_scope_and_a_press_in_the_other_one_starts_it_again() {
        let overlays = Overlays::new();
        overlays.walk(SwitcherScope::App, 1);
        assert_eq!(overlays.scope(), SwitcherScope::App);
        overlays.walk(SwitcherScope::App, 1);
        assert_eq!(overlays.walked(), 2, "the same scope walks on");
        overlays.walk(SwitcherScope::Workspace, 1);
        assert_eq!(overlays.scope(), SwitcherScope::Workspace);
        assert_eq!(
            overlays.walked(),
            1,
            "the other scope starts from the front"
        );
        overlays.walk(SwitcherScope::Workspace, -1);
        assert_eq!(overlays.walked(), 0);
        assert_eq!(overlays.reached(3, Some(1)), Some(1));
    }

    #[test]
    fn choosing_by_place_reaches_that_window_and_a_surface_down_is_no_walk() {
        let overlays = Overlays::new();
        overlays.select(2);
        assert_eq!(
            overlays.reached(4, Some(1)),
            None,
            "nothing walks while it is down"
        );
        overlays.walk(SwitcherScope::App, 1);
        overlays.select(2);
        assert_eq!(overlays.reached(4, Some(1)), Some(3));
        overlays.select(0);
        assert_eq!(overlays.reached(4, Some(1)), Some(1));
        overlays.show(Overlay::Rest);
        assert!(!overlays.switcher_up());
    }

    #[test]
    fn with_no_window_in_front_the_first_step_reaches_the_first_window() {
        let overlays = Overlays::new();
        overlays.walk(SwitcherScope::Workspace, 1);
        assert_eq!(
            overlays.reached(2, None),
            Some(0),
            "the first step reaches index 0 rather than skipping it"
        );
        assert_eq!(
            overlays.reached(1, None),
            Some(0),
            "a lone window not in front is reached"
        );
        overlays.walk(SwitcherScope::Workspace, 1);
        assert_eq!(overlays.reached(2, None), Some(1));
        overlays.walk(SwitcherScope::Workspace, -1);
        assert_eq!(overlays.reached(2, None), Some(0));
        overlays.walk(SwitcherScope::Workspace, -1);
        assert_eq!(
            overlays.reached(2, None),
            Some(1),
            "wraps to the last window"
        );
        // With a window in front, nothing changes.
        overlays.show(Overlay::Rest);
        overlays.walk(SwitcherScope::Workspace, 1);
        assert_eq!(overlays.reached(2, Some(0)), Some(1));
    }
}
