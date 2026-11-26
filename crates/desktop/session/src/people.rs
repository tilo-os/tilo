//! Who is here: the roster the bar's faces open, and every other person's pointer where they are
//! working. The roster can also be kept on screen, waiting at an edge of the shell with a mirror of
//! each person's screen beside their row, pressed to follow them.
//! The roster reads the shell's member rows and presence, which the peer network writes each frame
//! ([`crate::sync`]): the one feed the bar's faces and the launcher's Happening now list read too.
//! Everyone's pointer is the live session's tile pointer, drawn over the tile of the tab they are on, in
//! their own colour.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use settings_store::widgets::{PEOPLE_OVERLAY, PeopleOverlayPrefs};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop_model::Layout;
use tilo_session_core::desktop::here::Screen;
use tilo_session_core::desktop::host::chrome::ChromeIntent;
use tilo_session_core::desktop::navigation::LayoutEffect;
use tilo_session_core::desktop::people::{RosterRow, roster_rows};
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::surfaces::{Look, Published};
use tilo_ui_base::host_operation::ResourceOperationStart;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{
    motion::Appear, motion::Direction, motion::MotionValue, motion::Presence, motion::Snap,
};
use ui::kurbo::{Point, Rect, Size, Vec2};
use ui::peniko::{BlendMode, Fill};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::time::Duration;
use ui::widgets::DockEdge;
use ui::winit::event::WindowEvent;

use self::edges::{EdgeDrag, Item};
use self::roster::{PeopleHit, PeopleModel, PeopleOverlay};

mod edges;
mod roster;

/// How long after the person pressed elsewhere a press on the faces does not reopen the roster:
/// their press closed it.
const DISMISS_GRACE: Duration = Duration::from_millis(250);

/// How long a wave this person sent shows on the row it went to.
const WAVE_SHOWN: Duration = Duration::from_millis(2400);

/// What the roster's card asks the session to do: what only it can.
pub enum PeopleAsk {
    /// Open the conversation this person has with `key`'s person in Chat,
    /// its composer ready: the app is started when it is not up, and the
    /// pair's record is shown either way.
    Message { key: String },
    /// The name the person gave themselves on their own card.
    Rename(String),
    /// The picture the person chose for themselves, by its address in the content store; empty
    /// goes back to their initials.
    Picture(String),
    /// What they say they are doing, in their own words.
    Status(String),
    /// How available they say they are.
    Availability(ui::Availability),
    /// The presence colour they chose, by its id.
    Colour(String),
    /// Open the Settings page the person chose.
    Settings(String),
    /// Do the host action this person's own card offers, as the palette and a banner hand theirs
    /// back: the choice the ask on their avatar waits for is answered here too.
    HostAction(String),
    /// Go to where `who` (by its presence key) works: the window of theirs shown here, else their
    /// app, as following them brings it forward once.
    GoTo { who: String },
    /// Ask this of the agent of `client`, whose card stands for the member `who` (by its presence
    /// key): an agent this device started is stopped here, anything else through its card in Console.
    Agent {
        who: String,
        client: String,
        ask: tilo_session_core::desktop::launchpad::AgentAsk,
    },
    /// Follow the person `who` (by their presence key), or stop following when `following` is off:
    /// the bar keeps their view in front while this person follows them.
    Follow { who: String, following: bool },
    /// Wave at the person `who` (by their presence key): their bar says so for a moment.
    Wave { who: String },
    /// Add another of this person's devices: the session's sheet prints the link a new one joins
    /// with.
    PairDevice,
}

/// The colour this person wears here: the one they chose in their own settings, else the one their
/// identity derives, so their face is the same on every surface and on every device that paints
/// them.
pub(crate) fn own_rgb(identity: &str) -> (u8, u8, u8) {
    tilo_ui_base::presence::identity_rgb(own_hue(identity))
}

/// The hue of [`own_rgb`].
pub(crate) fn own_hue(identity: &str) -> tilo_ui_base::tokens::PresenceHue {
    tilo_ui_base::local_identity::own_colour()
        .unwrap_or_else(|| tilo_ui_base::tokens::PresenceHue::of_identity(identity))
}

/// The colour this person's own avatar and card wear: in a live room, the one everyone here draws
/// them in (`HereState::own_rgb`, settled over the whole room and published on their pointer), so
/// they see themselves in the colour everyone else sees; with no room settled yet, [`own_rgb`].
pub(crate) fn own_live_rgb(sync: Option<&crate::sync::Client>, identity: &str) -> (u8, u8, u8) {
    sync.and_then(|sync| sync.here().borrow().own_rgb())
        .filter(|rgb| *rgb != tilo_ui_base::presence::UNSETTLED_RGB)
        .unwrap_or_else(|| own_rgb(identity))
}

/// The name and identity of this person's face, shared by their own surfaces and their presence.
/// In a live session the name is the one they gave, and stays empty until one is known so each
/// surface localizes the unnamed label. With no peer network here (`own` is `None`) nothing
/// publishes a name, so the person goes by the name this host knows whoever is at it by, as
/// [`crate::platform::person::person_name`] reads it: their own, else the account's.
/// The identity is the root key they are here by, else the id this device sits as.
pub(crate) fn own_face(
    own: Option<&crate::sync::OwnPresence>,
    device_person: &str,
) -> (String, String) {
    let Some(own) = own else {
        return (
            crate::platform::person::person_name(
                tilo_ui_base::local_identity::load().map(|identity| identity.display_name),
                "",
            ),
            device_person.to_owned(),
        );
    };
    let identity = own
        .user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .unwrap_or(device_person)
        .to_owned();
    let name = crate::platform::person::given_name(
        tilo_ui_base::local_identity::load().map(|identity| identity.display_name),
    )
    .unwrap_or_else(|| own.label.clone());
    (name, identity)
}

/// How the roster kept on screen stands at its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rest {
    /// It waits at its edge, drawn where it waits.
    Here,
    /// The next frame shows it coming in.
    Arriving,
    /// It keeps its pin but waits away from the shell: nobody else is here to show.
    Waiting,
    /// It is on its way back into the bar; once it has gone, the pin goes with it.
    Leaving,
}

/// The roster kept on screen: where it waits, how it arrived, and how it is dragged to another
/// edge of the shell. What it shows is the roster's own painting, at an edge.
struct Pinned {
    /// The edge it waits at, as the person's settings keep it.
    edge: DockEdge,
    /// Whether it stands, comes in, waits away or leaves.
    rest: Rest,
    shown: Presence,
    /// The panel as drawn this frame.
    panel: Rect,
    /// A press on it, and the drag to another edge it may become.
    drag: Option<EdgeDrag<()>>,
    /// Where a drop left the panel, until the next layout springs it to its slot.
    dropped: Option<Point>,
    /// How far from its slot the panel is drawn while it springs there.
    settle: MotionValue<Point>,
    /// The pointer, in the shell's coordinates: where a drag of the panel is drawn under.
    pointer: Point,
    /// The screen's room, as the roster last laid it out: where a drop snaps.
    work: Rect,
    /// It drew at another size than the slot it was placed in: the next frame places it again.
    remeasured: bool,
}

impl Pinned {
    /// The roster waiting at `edge`; `resting` when it was pinned before this session began.
    fn new(edge: DockEdge, resting: bool) -> Self {
        let mut shown = Presence::closed();
        if resting {
            shown.snap(true);
        }
        Self {
            edge,
            rest: if resting { Rest::Here } else { Rest::Arriving },
            shown,
            panel: Rect::ZERO,
            drag: None,
            dropped: None,
            settle: MotionValue::new(Point::ZERO),
            pointer: Point::ZERO,
            work: Rect::ZERO,
            remeasured: false,
        }
    }

    /// Whether anything of it is moving: arriving, leaving, sprung to its slot, or dragged.
    fn is_moving(&self, now: ui::time::Instant) -> bool {
        self.remeasured
            || matches!(self.rest, Rest::Arriving | Rest::Leaving)
            || self.drag.is_some_and(|drag| drag.dragging)
            || self.shown.is_moving(now)
            || self.settle.is_moving(now)
            || self.dropped.is_some()
    }
}

/// What the panel shows and where it stands, kept at rest between frames.
#[derive(Default)]
struct Shown {
    overlay: PeopleOverlay,
    rows: Vec<RosterRow>,
    /// Each person's screen as their presence says it, by roster id, `None` for someone who keeps
    /// where they are to themselves.
    screens: Vec<(String, Option<Screen>)>,
    workspace: String,
    /// Whom the wave they sent goes to, and since when.
    waved: Option<(String, ui::time::Instant)>,
    /// Where the pointer was over the shell: the panel opens under it.
    cursor: Option<Point>,
    /// Who else's pointer is over which tab, as the last frame drew them: their name and where they
    /// are.
    pointers: Vec<(String, String)>,
    /// Where each other person's pointer was drawn on this session, as the last frame drew it: their
    /// name and the point, in the surface's coordinates.
    cursors: Vec<(String, (f64, f64))>,
    /// Where the shell is drawn, as the surface last laid out.
    area: Rect,
    /// The roster kept on screen at an edge, while the person keeps it there.
    pinned: Option<Pinned>,
    /// How tall the shell's bar is, as the session last told it: the roster, its cards and the
    /// banner stand under the bar the person sees. `None` until the session says.
    bar_height: Option<f64>,
}

impl Shown {
    /// Whether the roster stands kept at an edge of the shell, drawn and pressed where it waits: a
    /// pin waiting away with nobody else here does not.
    fn is_kept(&self) -> bool {
        !self.sheet()
            && self
                .pinned
                .as_ref()
                .is_some_and(|pin| pin.rest != Rest::Waiting)
    }

    /// Whether the shell is a phone's: the roster opens as a sheet at its foot and is never kept on
    /// screen, where it would stand over the one window there is ([`roster::is_sheet`]).
    fn sheet(&self) -> bool {
        roster::is_sheet(self.area)
    }
}

/// The People panel and the other people's pointers: one surface above the chrome, so the shell
/// keeps its own drawing and the panel floats over it.
pub struct People {
    /// The layer the roster and everyone else's pointers are drawn on, once the host placed it.
    layer: RefCell<Option<Rc<dyn crate::platform::surfaces::Layer>>>,
    shown: Rc<RefCell<Shown>>,
    open: Rc<Cell<bool>>,
    /// When the person last pressed elsewhere, closing the roster.
    dismissed: Rc<Cell<Option<ui::time::Instant>>>,
    /// The workspace in front answered that this person may not invite anyone into it.
    invite_withheld: Rc<Cell<bool>>,
}

/// Keep the roster on screen: it waits at the edge the person last chose, and what they see from
/// then on is the panel, not the dropdown the faces open.
/// Where the pointer is in the logical pixels the roster is laid out and pressed in: a window
/// event carries physical ones, which on a screen at twice the scale put every press twice as far
/// from the corner as the control it was made on.
fn logical(position: ui::winit::dpi::PhysicalPosition<f64>, scale: f64) -> Point {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    Point::new(position.x / scale, position.y / scale)
}

fn pin_roster(shown: &mut Shown, open: &Cell<bool>) {
    if shown.pinned.is_some() {
        return;
    }
    let edge = PEOPLE_OVERLAY.get().edge;
    shown.pinned = Some(Pinned::new(edge, false));
    open.set(false);
    keep_pinned(true, edge);
}

/// Keep the pinned roster's edge in the person's settings, as the Desktop widgets keep theirs.
fn keep_pinned(pinned: bool, edge: DockEdge) {
    let kept = PEOPLE_OVERLAY.update(|_| PeopleOverlayPrefs { pinned, edge });
    if let Err(error) = kept {
        tracing::warn!(%error, "could not keep the roster's pin");
    }
}

impl People {
    /// Whether the workspace in front answered that this person may not invite anyone into it:
    /// the roster then says so where its invite would stand.
    pub fn withhold_invite(&self, withheld: bool) {
        self.invite_withheld.set(withheld);
    }

    /// Open the roster, or close it again: what the bar's faces do.
    pub fn toggle(&self) {
        // Kept on screen and open, the roster is already where the faces would open it: they
        // have nothing to do, and the panel's own button puts it back in the bar.
        if self.shown.borrow().is_kept() {
            return;
        }
        // Their press closed it a moment ago: this is that press, not another.
        let dismissed = self
            .dismissed
            .take()
            .is_some_and(|at| at.elapsed() < DISMISS_GRACE);
        if !dismissed {
            let open = !self.open.get();
            self.open.set(open);
        }
        self.redraw();
    }

    /// Whether the roster stands over the shell now.
    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Close the roster: the person pressed elsewhere.
    pub fn dismiss(&self) {
        if !self.open.replace(false) {
            return;
        }
        // What the roster shows is cleared before it draws again: the draw holds what it shows
        // while it draws it.
        self.dismissed.set(Some(ui::time::now()));
        self.redraw();
    }

    /// Draw again, though no input reached the panel: it was opened or closed by the session.
    fn redraw(&self) {
        if let Some(layer) = self.layer.borrow().as_ref() {
            layer.relayout();
        }
    }

    /// How tall the shell's bar is, as the session measured it: what the roster, its cards and the
    /// banner stand under. A change redraws, as a theme's does.
    pub fn set_bar_height(&self, height: f64) {
        let changed = {
            let mut shown = self.shown.borrow_mut();
            let changed = shown.bar_height != Some(height);
            shown.bar_height = Some(height);
            changed
        };
        if changed {
            self.redraw();
        }
    }

    /// Draw in the person's theme, as their settings now have it.
    pub fn set_theme(&self, theme: tilo_ui_base::Theme) {
        if let Some(layer) = self.layer.borrow().as_ref() {
            layer.set_theme(theme);
        }
    }

    /// The layer the roster is drawn on, once the host placed it.
    fn layer(&self) -> Option<Rc<dyn crate::platform::surfaces::Layer>> {
        self.layer.borrow().clone()
    }

    /// The other people's pointers as the last frame drew them: their name and the point, in the
    /// surface's coordinates.
    pub fn drawn_pointers(&self) -> Vec<(String, (f64, f64))> {
        self.shown.borrow().cursors.clone()
    }
}

impl Published for People {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        Vec::new()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        _arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        Err(ApiError::Unknown(address.clone()))
    }

    fn revision(&self) -> Option<u64> {
        // The roster is up or down, and its rows are the frame the host drew: read it again only
        // when one of those moved.
        let layer = self.layer().map_or(0, |layer| layer.revision());
        Some(layer | (u64::from(self.open.get()) << 63))
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        if !self.open.get() && !self.shown.borrow().is_kept() {
            return None;
        }
        self.layer().and_then(|layer| layer.a11y_tree())
    }

    fn a11y_name(&self) -> Option<String> {
        Some(crate::i18n::t("session-people"))
    }

    fn a11y_action(
        &mut self,
        target: ui::accesskit::NodeId,
        action: &tilo_ui_base::a11y::Action,
    ) -> bool {
        (self.open.get() || self.shown.borrow().is_kept())
            && self
                .layer()
                .is_some_and(|layer| layer.a11y_action(target, action))
    }

    fn wants_keyboard(&self) -> bool {
        self.open.get()
    }

    /// The controls the open roster and the pinned roster show, where they stand: what the host
    /// drew.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        match self.layer() {
            Some(layer) if self.open.get() || self.shown.borrow().is_kept() => layer.controls(),
            _ => Vec::new(),
        }
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        match self.layer() {
            Some(layer) if self.open.get() || self.shown.borrow().is_kept() => layer.press(name),
            _ => Err(format!("no control named {name}")),
        }
    }

    /// Who is here, as the roster lists them: each person's name, what they are doing, and
    /// where they are.
    fn accessible_labels(&mut self) -> Vec<String> {
        if !self.open.get() && !self.shown.borrow().is_kept() {
            return Vec::new();
        }
        let shown = self.shown.borrow();
        // The panel's own tree, opened from the bar or kept on screen: the same names, rows and
        // controls either way.
        let mut labels = Vec::new();
        let mut pending = vec![shown.overlay.accessibility_snapshot()];
        while let Some(node) = pending.pop() {
            labels.extend(node.name.clone());
            labels.extend(node.value.clone());
            // What the row says of the person: where they are and how they said they are.
            labels.extend(node.description.clone());
            pending.extend(node.children);
        }
        // Where everyone else is working: their pointer over a window of this session says so, beside
        // their name. A pointer the roster cannot put a name to is drawn all the same, but says
        // nothing here: "is in Notes" names nobody.
        for (who, title) in &shown.pointers {
            if who.trim().is_empty() {
                continue;
            }
            labels.push(crate::i18n::t_args(
                "people-pointer-where",
                &[("who", who.clone().into()), ("where", title.clone().into())],
            ));
        }
        labels.retain(|label| !label.trim().is_empty());
        labels
    }
}

/// What each window of the shell shows, by its tab: the app it runs, and the
/// record it holds (a window whose app keeps no single record names its kind).
pub type WindowRecords = Rc<dyn Fn() -> Vec<(u64, Option<String>, String)>>;

/// What the roster reads and asks of the session, beside the surfaces it is placed on.
pub struct Given {
    /// Where the panel stands and how finely it draws.
    pub scale: f64,
    /// What each window of the shell shows, by its tab: the record it holds, as
    /// the host that runs the windows reads them.
    pub notes: WindowRecords,
    /// The apps this host has, which names the windows a person works in.
    pub apps: Option<std::sync::Arc<dyn crate::platform::apps::Apps>>,
    /// The shell the roster is drawn over.
    pub handle: DesktopHandle,
    /// Who is here, when this session takes part in the peer network.
    pub sync: Option<Rc<crate::sync::Client>>,
    /// Who is here, written into a copy of the layout as its members and their presence: what the bar's
    /// faces are drawn from, so the roster they open lists the same people. A host that hears
    /// some of them on a track of its own (the runs a workspace's host speaks for) writes them
    /// in here as it does for the bar.
    pub here: Rc<dyn Fn(&mut Layout)>,
    /// Where the chrome takes the intents the roster raises.
    pub intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether the person may invite someone into the workspace in front.
    pub invitable: Rc<Cell<bool>>,
}

/// The roster on a layer of its own over the windows, and the session's handle of it.
pub fn mount(
    surfaces: &mut dyn crate::platform::surfaces::Surfaces,
    given: Given,
) -> Rc<RefCell<People>> {
    let shown = Rc::new(RefCell::new(Shown::default()));
    // Kept on screen when it was pinned before, it is already there: no motion.
    let kept = PEOPLE_OVERLAY.get();
    if kept.pinned {
        shown.borrow_mut().pinned = Some(Pinned::new(kept.edge, true));
    }
    let open = Rc::new(Cell::new(false));
    let dismissed: Rc<Cell<Option<ui::time::Instant>>> = Rc::default();
    let invite_withheld: Rc<Cell<bool>> = Rc::default();
    let panel = Panel {
        shown: Rc::clone(&shown),
        open: Rc::clone(&open),
        handle: given.handle,
        drawn: Cell::new(0),
        sync: given.sync,
        here: given.here,
        notes: given.notes,
        apps: given.apps,
        intents: given.intents,
        invitable: given.invitable,
        invite_withheld: Rc::clone(&invite_withheld),
        chats: RefCell::default(),
        motion_until: Cell::new(0.0),
    };
    let people = Rc::new(RefCell::new(People {
        layer: RefCell::new(None),
        shown,
        open: Rc::clone(&open),
        dismissed,
        invite_withheld,
    }));
    // A press anywhere else closes what the roster has open, as their own press does. The hook
    // stands beside the tree the layer shows, never in it, so holding the roster is no cycle.
    let roster = Rc::clone(&people);
    let heard = crate::platform::surfaces::Heard {
        wants_keyboard: Rc::new(move || open.get()),
        pressed_elsewhere: Rc::new(move || roster.borrow().dismiss()),
        takes_bar_point: Rc::new(|_| false),
    };
    let placement = crate::platform::surfaces::Placement {
        origin: (0, 0),
        size: (1, 1),
        scale: given.scale,
    };
    let layer = surfaces.place(
        &crate::i18n::t("session-people"),
        Box::new(panel),
        placement,
        heard,
    );
    *people.borrow().layer.borrow_mut() = Some(layer);
    surfaces.publish(people.clone());
    people
}

/// The widget: the roster floating under the bar, and every other person's pointer over the tile
/// they are on.
struct Panel {
    shown: Rc<RefCell<Shown>>,
    open: Rc<Cell<bool>>,
    handle: DesktopHandle,
    /// The presence revision this panel last drew: another person's pointer moving, arriving or
    /// leaving moves it, and a shell where nothing moved asks for no frame.
    drawn: Cell<u64>,
    sync: Option<Rc<crate::sync::Client>>,
    /// Who is here, written into the shell's member rows as the bar's faces read them
    /// ([`Given::here`]).
    here: Rc<dyn Fn(&mut Layout)>,
    /// What each window of the shell shows, by its tab: where a person's record stands, so the
    /// roster places them in the window they work in.
    notes: WindowRecords,
    /// The apps this host has, which names the windows a person works in.
    apps: Option<std::sync::Arc<dyn crate::platform::apps::Apps>>,
    /// Where the bar's intents go: inviting from the roster's footer is the bar's own invite,
    /// raised here.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether the person may invite someone into the workspace in front, as the session's loop
    /// last answered it.
    invitable: Rc<Cell<bool>>,
    /// The workspace in front said this person may not invite into it.
    invite_withheld: Rc<Cell<bool>>,
    /// What everyone here says beside their pointer, as the toolkit times it for every host.
    chats: RefCell<tilo_ui_base::presence::CursorChats>,
    /// Keep drawing until then: the newest pointer heard plus the window it eases to rest in.
    motion_until: Cell<f64>,
}

impl Panel {
    /// Follow the person with this roster id, or stop following them: the verb is the control's
    /// own — a press that stops following is not a gesture that begins it again.
    fn follow_person(&self, id: &str, following: bool) {
        if let Some(sync) = &self.sync {
            sync.set_following(following.then(|| id.to_owned()));
        }
    }

    /// Milliseconds since the epoch, on the clock presence is heard on.
    #[expect(
        clippy::cast_precision_loss,
        reason = "milliseconds since the epoch are exact in an f64 for the life of a session"
    )]
    fn presence_ms() -> f64 {
        ui::time::unix_now_ms() as f64
    }

    /// What this host calls the app `app`: the name the app lists of the machine or the page
    /// carry for it, and none where it lists no name for it.
    fn host_name(&self, app: &str) -> Option<String> {
        self.apps
            .as_deref()
            .map(|apps| apps.name(app))
            .filter(|name| !name.trim().is_empty())
    }

    /// Read who is here, as the live session wrote them into the shell's member rows this frame: the
    /// roster, each person's screen with its windows named by the records this session shows, and the
    /// workspace it is of: the panel's title, kept on screen or opened from the bar.
    fn read_roster(&self, shown: &mut Shown) {
        let windows = (self.notes)();
        let mut tree = self.handle.read(Clone::clone);
        (self.here)(&mut tree);
        let mut rows = roster_rows(&tree);
        let workspace = tree.active_workspace().map_or_else(
            || tilo_session_core::desktop::names::workspace(""),
            |workspace| tilo_session_core::desktop::names::workspace(&workspace.name),
        );
        let front = tree
            .active_workspace()
            .map(|workspace| workspace.id().to_string());
        // What each window here shows, by the digest presence names it with: what a person's window
        // on the same record is called here, by the one rule every surface names a window by.
        let titled: BTreeMap<String, String> = crate::sync::layout::bindings_of(&tree, &windows)
            .into_iter()
            .filter_map(|(tab, _app, digest)| {
                let title = crate::app_names::tab_title(tree.tab(tab)?);
                (!title.trim().is_empty()).then_some((digest, title))
            })
            .collect();
        // Each person's screen, its windows named with what they show where this session holds that
        // record, else with the app's name.
        let screens = match (self.sync.as_ref(), front.clone()) {
            (Some(sync), Some(workspace)) => sync
                .members(&workspace)
                .into_iter()
                .map(|member| {
                    let name_of = |app: &str, record: &str| {
                        titled.get(record).cloned().or_else(|| self.host_name(app))
                    };
                    let screen = member
                        .layout
                        .as_ref()
                        .and_then(|layout| Screen::of(layout, name_of));
                    (member.publisher, screen)
                })
                .collect(),
            _ => Vec::new(),
        };
        // Whom each person here follows, as their presence says: "Following you" on the row of
        // someone following this person, "Following Ada" on someone else's.
        if let (Some(sync), Some(workspace)) = (self.sync.as_ref(), front.as_deref()) {
            let pairs: Vec<(String, String)> = sync
                .members(workspace)
                .into_iter()
                .filter_map(|member| Some((member.publisher, member.following?)))
                .collect();
            tilo_session_core::desktop::people::note_following(
                &mut rows,
                &pairs,
                Some(&sync.publisher().to_string()),
            );
        }
        shown.rows = rows;
        shown.screens = screens;
        shown.workspace = workspace;
    }

    /// Act on what a press or a key on the roster meant.
    fn follow(&self, hit: PeopleHit, shown: &mut Shown) {
        match hit {
            PeopleHit::Dismiss => {
                self.open.set(false);
            }
            PeopleHit::Follow(index) => {
                if let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) {
                    self.follow_person(&id, true);
                }
            }
            PeopleHit::Unfollow(index) => {
                if let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) {
                    self.follow_person(&id, false);
                }
            }
            PeopleHit::Wave(index) => {
                let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                if let Some(sync) = &self.sync {
                    sync.wave(&id);
                    shown.waved = Some((id, ui::time::now()));
                }
            }
            // The window software works in: brought forward, so what it is on stands in front of
            // this person.
            PeopleHit::OpenTab(index) => {
                let tab = shown
                    .rows
                    .get(index)
                    .and_then(|row| row.working_in.as_ref())
                    .map(|(tab, _)| *tab);
                if let Some(tab) = tab {
                    let command = LayoutEffect::Focus {
                        tab: tab.to_string(),
                    };
                    if let Err(error) = self.handle.dispatch(command) {
                        tracing::warn!(%error, "the window software works in could not come forward");
                    }
                }
            }
            // Inviting from here is the bar's own invite: the roster offers its footer only while
            // the person may hand out a link into this workspace.
            PeopleHit::Invite => {
                self.intents.borrow_mut().push(ChromeIntent::Invite);
            }
            // Keeping the roster on screen moves it out of the bar to an edge of the shell; pressed
            // where it stands, it goes back into the bar.
            PeopleHit::Pin => {
                if shown.pinned.is_some() {
                    let edge = shown
                        .pinned
                        .as_ref()
                        .map_or(DockEdge::Right, |pin| pin.edge);
                    if let Some(pin) = shown.pinned.as_mut() {
                        pin.rest = Rest::Leaving;
                    }
                    keep_pinned(false, edge);
                } else {
                    pin_roster(shown, self.open.as_ref());
                }
            }
            // No call to join from here (a call is Chat's, in an app window),
            // and nothing else to do: the roster kept the press.
            PeopleHit::Absorb => {}
        }
    }

    /// How many times what the shell's people say of themselves changed.
    fn presence_revision(&self) -> u64 {
        self.sync
            .as_ref()
            .map_or(0, |sync| sync.presence_revision())
    }

    /// Where everyone here points, drawn by the one rule both hosts draw pointers by: over the
    /// tile of the tab it is on, in the colour and under the name the workspace knows them by,
    /// with what they say beside it. What each tab is called names where they are.
    fn draw_pointers(&self, ctx: &mut tilo_ui_base::DrawCtx<'_>, shown: &mut Shown) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let open = self.open.get();
        let tiles: Vec<tilo_live::presence::Tile> = self
            .handle
            .shown_tiles()
            .into_iter()
            .map(|(tab, area)| {
                tilo_live::presence::Tile::new(tab, area.x0, area.y0, area.width(), area.height())
            })
            .collect();
        let mut labels: HashMap<u64, String> = HashMap::new();
        let mut pointers = tilo_session_core::presence::Pointers::default();
        let no_presence = tilo_ui_base::presence::PresenceCtx::default();
        if let Some(sync) = &self.sync
            && let Some((workspace_id, named)) = self.handle.read(|tree| {
                let workspace = tree.active_workspace()?;
                let named: HashMap<u64, String> = workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .map(|tab| (tab.id, tab.title.clone()))
                    .collect();
                Some((workspace.id().to_string(), named))
            })
        {
            labels = named;
            let members = sync.members(&workspace_id);
            if !members.is_empty() {
                pointers = tilo_session_core::presence::pointers(
                    &members,
                    &sync.here().borrow().faces(),
                    &tiles,
                    &mut self.chats.borrow_mut(),
                    env.presence.unwrap_or(&no_presence),
                    ui::time::now(),
                    Self::presence_ms(),
                    theme.motion(),
                );
            }
        }
        if let Some(until) = pointers.motion_until {
            self.motion_until.set(self.motion_until.get().max(until));
        }
        // Who is here is read once, for the rows and for the pinned roster's cards.
        if open || shown.pinned.is_some() || !pointers.drawn.is_empty() {
            self.read_roster(shown);
        }
        shown.pointers = pointers
            .drawn
            .iter()
            .zip(&pointers.tabs)
            .filter_map(|(cursor, tab)| {
                labels
                    .get(tab)
                    .map(|title| (cursor.label.clone(), title.clone()))
            })
            .collect();
        shown.cursors = pointers
            .drawn
            .iter()
            .map(|cursor| (cursor.label.clone(), cursor.pos))
            .collect();
        if !pointers.drawn.is_empty() {
            let area = shown.area;
            tilo_ui_base::presence::draw_peer_cursors(
                scene,
                transform,
                area,
                text,
                theme,
                &pointers.drawn,
            );
        }
    }

    /// The roster, drawn where it stands: under the bar as the faces open it, or kept on screen at
    /// an edge with a mirror of each person's screen. One painting for both.
    fn draw_roster(&self, ctx: &mut tilo_ui_base::DrawCtx<'_>, shown: &mut Shown) {
        // The roster is the panel's only while it is up or kept at an edge: closed, it draws
        // nothing, so the surface stands transparent and a press under the bar reaches the bar.
        if (shown.pinned.is_none() || shown.sheet()) && !self.open.get() {
            return;
        }
        // The bar's faces open the roster where they stand, kept on screen or not.
        if self.open.get() {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text, theme, mut env) = ctx.parts();
            self.draw_one_roster(
                &mut tilo_ui_base::DrawCtx::new(scene, transform, text, theme, env.clone()),
                shown,
                None,
            );
            return;
        }
        self.draw_pinned(ctx, shown);
    }

    /// The roster kept at an edge, where its pin placed it and layered over what it covers.
    fn draw_pinned(&self, ctx: &mut tilo_ui_base::DrawCtx<'_>, shown: &mut Shown) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let now = tilo_ui_base::motion::frame_now();
        let motion = theme.motion();
        if let Some(pin) = shown.pinned.as_mut() {
            match pin.rest {
                Rest::Leaving => {
                    pin.shown.show(
                        false,
                        Appear::SPEC,
                        motion,
                        tilo_ui_base::motion::input_source(),
                        now,
                    );
                    if !pin.shown.is_visible(now) {
                        shown.pinned = None;
                    }
                }
                // Kept at an edge with nobody else here, the roster has nothing to say but that:
                // it waits away from the shell, its pin kept, and comes in when someone arrives.
                _ if shown.rows.is_empty() => {
                    if pin.rest != Rest::Waiting {
                        pin.rest = Rest::Waiting;
                        pin.shown.snap(false);
                    }
                }
                Rest::Arriving | Rest::Waiting => {
                    pin.rest = Rest::Here;
                    pin.shown.show(
                        true,
                        Appear::SPEC,
                        motion,
                        tilo_ui_base::motion::input_source(),
                        now,
                    );
                }
                Rest::Here => {}
            }
        }
        let Some(pin) = shown.pinned.as_ref() else {
            return;
        };
        if !pin.shown.is_visible(now) {
            // Waiting away with nothing to show, or on its way back into the bar.
            return;
        }
        let area = shown.area;
        let bar = shown.bar_height.unwrap_or_else(|| {
            f64::from(tilo_session_core::desktop::overlays::topbar_height(theme))
        });
        let placed = {
            let Some(pin) = shown.pinned.as_mut() else {
                return;
            };
            Self::place_pinned(pin, area, bar, theme, now);
            let anchor = match pin.edge {
                DockEdge::Left | DockEdge::BottomLeft => {
                    Point::new(pin.panel.x0, pin.panel.center().y)
                }
                DockEdge::Bottom => Point::new(pin.panel.center().x, pin.panel.y1),
                DockEdge::Right | DockEdge::BottomRight => {
                    Point::new(pin.panel.x1, pin.panel.center().y)
                }
            };
            let pose = Appear::pose(
                pin.shown.progress(now),
                Direction::of(pin.shown.is_open()),
                pin.panel,
                anchor,
                motion,
            );
            (pose, pin.panel)
        };
        let (pose, slot) = placed;
        let placed = pose.applied_to(transform);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            placed,
            &slot.inflate(32.0, 32.0),
        );
        self.draw_one_roster(
            &mut tilo_ui_base::DrawCtx::new(scene, placed, text, theme, env.clone()),
            shown,
            Some(slot),
        );
        scene.pop_layer();
        // What the roster made of its slot is where it now stands: the next placement centres it
        // by that, and a press on its header drags the panel the person sees.
        let drawn = shown.overlay.panel_rect();
        if let Some(pin) = shown.pinned.as_mut() {
            // Drawn at another size than its slot was made for, it is placed again by the size it
            // drew at on the next frame, which it asks for now: otherwise it stands where the guess
            // put it until the next input, and that input's own frame moves it from under the press.
            pin.remeasured = drawn.size() != pin.panel.size();
            pin.panel = drawn;
        }
    }

    /// The roster's one painting, with the model this session builds of who is here.
    fn draw_one_roster(
        &self,
        ctx: &mut tilo_ui_base::DrawCtx<'_>,
        shown: &mut Shown,
        panel: Option<Rect>,
    ) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let pinned = shown.pinned.is_some() && !shown.sheet();
        let sheet = shown.sheet();
        let followed = self.sync.as_ref().and_then(|sync| sync.own().following);
        let workspace = shown.workspace.clone();
        let rows = shown.rows.clone();
        let screens = shown.screens.clone();
        let waved = shown
            .waved
            .as_ref()
            .filter(|(_, at)| at.elapsed() < WAVE_SHOWN)
            .map(|(id, _)| id.clone())
            .into_iter()
            .collect::<Vec<_>>();
        let waved_until = shown
            .waved
            .as_ref()
            .filter(|(_, at)| at.elapsed() < WAVE_SHOWN)
            .and_then(|(_, at)| at.checked_add(WAVE_SHOWN));
        let model = PeopleModel {
            host: shown.area,
            bar_height: shown.bar_height.unwrap_or_else(|| {
                f64::from(tilo_session_core::desktop::overlays::topbar_height(theme))
            }),
            workspace: &workspace,
            rows: &rows,
            invite: if self.invitable.get() {
                tilo_session_core::desktop::host::chrome::Invite::Offered
            } else if self.invite_withheld.get() {
                tilo_session_core::desktop::host::chrome::Invite::Withheld
            } else {
                tilo_session_core::desktop::host::chrome::Invite::Unavailable
            },
            may_pin: !pinned && !sheet,
            pinned,
            panel,
            screens: &screens,
            followed_id: followed.as_deref(),
            waved: &waved,
            waved_until,
        };
        let cursor = shown.cursor;
        shown
            .overlay
            .draw(scene, transform, text, theme, &model, cursor);
    }

    /// Where the pinned roster waits this frame: its slot at its edge, under a drag, or springing
    /// back from a drop.
    fn place_pinned(pin: &mut Pinned, area: Rect, bar: f64, theme: &Theme, now: ui::time::Instant) {
        let work = Rect::new(area.x0, area.y0 + bar, area.x1, area.y1);
        pin.work = work;
        // The roster lays itself out in whatever rect it is given: its slot is the one it measured
        // last, so the panel keeps the height its own painting asked for.
        let size = if pin.panel.area() > 0.0 {
            pin.panel.size()
        } else {
            Size::new(
                f64::from(theme.tokens.metrics.panel_width_utility),
                work.height() * 0.5,
            )
        };
        let slot = edges::place(
            work,
            &[Item {
                id: (),
                edge: pin.edge,
                glance: size,
                open: size,
                has_content: true,
            }],
            &[],
            false,
        );
        let rest = slot.first().map_or_else(
            || Rect::from_origin_size(work.origin(), size),
            |slot| slot.glance,
        );
        // On a shell too narrow to keep the roster beside the work (a phone), an edge's margin
        // leaves it lopsided over the one window there is: it stands centred across the shell, its
        // gutters equal, at the height its edge gives it.
        let rest = if work.width() < 2.0 * size.width {
            rest.with_origin(Point::new(work.center().x - size.width / 2.0, rest.y0))
        } else {
            rest
        };
        // The roster waits where its slot does, moved by the spring back from a drop and by a drag
        // still in flight.
        let mut at = rest.origin();
        if let Some(dropped) = pin.dropped.take() {
            // Where it was let go, from where its slot waits: the spring starts there.
            let from = Vec2::new(dropped.x - at.x, dropped.y - at.y);
            pin.settle.snap(Point::new(from.x, from.y));
            let timing = Snap::SPEC.timing(
                Direction::Enter,
                theme.motion(),
                tilo_ui_base::tokens::MotionSource::Gesture,
            );
            pin.settle.set(Point::ZERO, timing, now);
        }
        let settle = pin.settle.get(now);
        at = Point::new(at.x + settle.x, at.y + settle.y);
        if let Some(shift) = pin.drag.and_then(|drag| drag.shift(pin.pointer)) {
            at = Point::new(at.x + shift.x, at.y + shift.y);
        }
        pin.panel = rest.with_origin(at);
        pin.remeasured = false;
    }

    /// Whether `event` is the pointer's away from the kept roster: neither on the panel where it
    /// stands nor dragging it.
    fn away_from_kept(&self, event: &WindowEvent) -> bool {
        if !matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        ) {
            return false;
        }
        let shown = self.shown.borrow();
        let Some(pin) = shown.pinned.as_ref() else {
            return false;
        };
        pin.drag.is_none() && shown.cursor.is_none_or(|point| !pin.panel.contains(point))
    }

    /// What a drag of the pinned roster asks: the pointer moved, a press started on it, or it was
    /// let go at an edge.
    fn handle_pinned(
        &self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        let cursor = shown.cursor;
        let mut handled = false;
        if shown.sheet() {
            // A phone's roster is never kept on screen: there is no panel of it to drag.
            return false;
        }
        {
            let Some(pin) = shown.pinned.as_mut() else {
                return false;
            };
            if pin.rest == Rest::Waiting {
                // Waiting away with nobody else here: there is no panel of it to press.
                return false;
            }
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    pin.pointer = logical(*position, ctx.scale_factor);
                    if let Some(drag) = &mut pin.drag {
                        handled = drag.moved(pin.pointer);
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    // The pointer left the shell: a press in flight lets go where it was drawn.
                    if let Some(drag) = pin.drag.take()
                        && drag.dragging
                    {
                        pin.dropped = Some(pin.panel.origin());
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: ui::winit::event::MouseButton::Left,
                    ..
                } => {
                    match state {
                        ui::winit::event::ElementState::Pressed => {
                            // A press on the panel itself, off its controls, drags it: the roster's
                            // own painting took the controls.
                            if let Some(point) = cursor
                                && pin.panel.contains(point)
                            {
                                pin.drag = Some(EdgeDrag::new((), point));
                                pin.pointer = point;
                            }
                        }
                        ui::winit::event::ElementState::Released => {
                            if let Some(drag) = pin.drag.take()
                                && let Some(edge) =
                                    cursor.and_then(|point| drag.release(pin.work, point))
                            {
                                pin.edge = edge;
                                pin.dropped = Some(pin.panel.origin());
                                keep_pinned(true, edge);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        *ctx.handled |= handled;
        handled
    }
}

impl Widget for Panel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use tilo_ui_base::widget::TaffyTreeExt;
        let style = Style {
            position: ui::taffy::Position::Absolute,
            inset: ui::taffy::Rect {
                left: ui::taffy::LengthPercentageAuto::length(0.0),
                right: ui::taffy::LengthPercentageAuto::length(0.0),
                top: ui::taffy::LengthPercentageAuto::length(0.0),
                bottom: ui::taffy::LengthPercentageAuto::length(0.0),
            },
            ..Style::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.shown.borrow_mut().area = tilo_ui_base::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.shown.borrow().area
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let shown = self.shown.borrow();
        // Each control says where it stands and carries what pressing it does, so a reader
        // presses the roster's own buttons as the person does.
        let roster = (self.open.get() || shown.is_kept())
            .then(|| shown.overlay.accessibility_with_targets());
        drop(shown);
        roster
    }

    /// The pinned roster's controls are its own, pressed there by a reader as by the person.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        if !matches!(action, tilo_ui_base::a11y::Action::Activate) {
            return false;
        }
        // The roster's own controls, as its tree names them.
        let pressed = self.shown.borrow().overlay.hit_for_target(target);
        let Some(hit) = pressed else {
            return false;
        };
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        self.follow(hit, &mut shown);
        true
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let revision = self.presence_revision();
        // What the roster shows is read here, while its state is borrowed.
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        self.draw_pointers(
            &mut tilo_ui_base::DrawCtx::new(scene, transform, text, theme, env.clone()),
            &mut shown,
        );
        self.draw_roster(
            &mut tilo_ui_base::DrawCtx::new(scene, transform, text, theme, env.clone()),
            &mut shown,
        );
        self.drawn.set(revision);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        // Where the pointer is, as the panel draws by it: under the pointer of the bar's faces and
        // every other person's name.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.shown.borrow_mut().cursor = Some(logical(*position, ctx.scale_factor));
        }
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.shown.borrow_mut().cursor = None;
        }
        // Kept on screen, the roster is pressed by the person's own pointer: it draws and answers
        // where it stands, and never takes the keyboard.
        let kept = self.shown.borrow().is_kept();
        if !self.open.get() && !kept {
            self.handle_pinned(event, ctx);
            return;
        }
        // Kept on screen, it answers only where it stands: the pointer away from it is the shell's.
        // The layer covers the whole shell where the host has one canvas (the page), so claiming a
        // press there would take it from the window under it, its Maximize among them.
        if !self.open.get() && self.away_from_kept(event) {
            self.handle_pinned(event, ctx);
            return;
        }
        // The roster's state is its own handle.
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        let cursor = shown.cursor;
        let hit = shown.overlay.handle_event(event, ctx, true, cursor);
        let Some(hit) = hit else {
            return;
        };
        // Kept on screen, a press on its header drags it to another edge.
        let drags = shown.is_kept()
            && shown
                .cursor
                .is_some_and(|point| shown.overlay.header_rect().contains(point));
        self.follow(hit, &mut shown);
        drop(shown);
        if drags {
            self.handle_pinned(event, ctx);
        }
    }

    /// Whether the panel owes a frame: while it is open, because its rows move with the people on
    /// the shell; closed, only once somebody else's pointer has moved or left since the last frame,
    /// or a bubble or a reaction is playing. A shell where a remote pointer rests where it was
    /// drawn asks for no frame.
    fn needs_redraw(&self) -> bool {
        let now = tilo_ui_base::motion::frame_now();
        if self.shown.borrow().pinned.is_some() {
            // The pinned roster draws while its cards arrive and move, and when what the workspace
            // says of its people changed: a pointer at rest asks for no frame.
            return self
                .shown
                .borrow()
                .pinned
                .as_ref()
                .is_some_and(|pin| pin.is_moving(now))
                || self.presence_revision() != self.drawn.get()
                || self.chats.borrow().animating(now)
                || Self::presence_ms() < self.motion_until.get();
        }
        self.open.get()
            || self.presence_revision() != self.drawn.get()
            || self.chats.borrow().animating(now)
            || Self::presence_ms() < self.motion_until.get()
    }

    fn name(&self) -> &'static str {
        "PeoplePanel"
    }

    fn test_id(&self) -> Option<&str> {
        Some("people-panel")
    }
}
