//! Who is here: the roster the bar's faces open, and every other person's pointer where they are
//! working. The roster can also be kept on screen, waiting at an edge of the desk with a mirror of
//! each person's screen beside their row, pressed to follow them.
//! The roster reads the desk's collaborators, which the collaboration plane writes each frame
//! ([`crate::sync`]): the one feed the bar's faces and the launcher's Happening now list read too.
//! Everyone's pointer is the plane's tile pointer, drawn over the tile of the tab they are on, in
//! their own colour.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use settings_store::desk_widgets::{PEOPLE_OVERLAY, PeopleOverlayPrefs};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, RosterRow,
    collaborator_roster_rows_named,
};
use tilo_desktop::desk_edges::{self, DockEdge, EdgeDrag, Item};
use tilo_desktop::here::Screen;
use tilo_desktop::launchpad::LaunchpadAppAction;
use tilo_desktop::profile_card::{
    OtherPerson, ProfileAction, ProfileCard, ProfileChoice, ProfileGroup, ProfileModel,
};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::{ActivityView, ChromeEffect, DeskEffect};
use tilo_session_core::door::{Look, Published};
use ui::host_operation::ResourceOperationStart;
use ui::kurbo::{Affine, Point, Rect, Size, Vec2};
use ui::motion::{Appear, Direction, MotionValue, Presence, Snap};
use ui::peniko::{BlendMode, Color, Fill};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Duration;
use ui::widget::Widget;
use ui::winit::event::WindowEvent;

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
    /// What they say they are doing, in their own words.
    Status(String),
    /// How available they say they are.
    Availability(ui::Availability),
    /// The presence colour they chose, by its id.
    Colour(String),
    /// Open the Settings page a card action names, such as their identities or an identity backup.
    Settings(String),
    /// Ask this of the agent of `client`, through its card in Console.
    Agent {
        client: String,
        ask: tilo_desktop::profile_card::AgentAsk,
    },
}

/// The colour this person wears here: the one they chose in their own settings, else the one their
/// identity derives, so their face is the same on every surface and on every device that paints
/// them.
pub(crate) fn own_rgb(identity: &str) -> (u8, u8, u8) {
    ui::presence::identity_rgb(own_hue(identity))
}

/// The hue of [`own_rgb`].
pub(crate) fn own_hue(identity: &str) -> ui::tokens::PresenceHue {
    ui::local_identity::own_colour()
        .unwrap_or_else(|| ui::tokens::PresenceHue::of_identity(identity))
}

/// The colour this person's own avatar and card wear: in a live room, the one everyone here draws
/// them in (`HereState::own_rgb`, settled over the whole room and published on their pointer), so
/// they see themselves in the colour everyone else sees; with no room settled yet, [`own_rgb`].
pub(crate) fn own_live_rgb(sync: Option<&crate::sync::Client>, identity: &str) -> (u8, u8, u8) {
    sync.and_then(|sync| sync.here().borrow().own_rgb())
        .filter(|rgb| *rgb != ui::presence::UNSETTLED_RGB)
        .unwrap_or_else(|| own_rgb(identity))
}

/// This person as their own surfaces show them: the name they go by here, and the identity their
/// face's letters and colour come from — the root key they are here by, else the id this device
/// sits as. The bar's own avatar and their own card both read it here, so one person wears one
/// initial and one colour wherever they see themselves.
pub(crate) fn own_face(own: &crate::sync::OwnPresence, seated: &str) -> (String, String) {
    let identity = own
        .user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .unwrap_or(seated)
        .to_owned();
    let name = crate::platform::person::desk_person_name(
        ui::local_identity::load().map(|identity| identity.display_name),
        &own.label,
    );
    (name, identity)
}

/// The card of this person themselves, where the roster lists someone else's: marked in
/// [`Shown::card_open`] while it stands there.
const SELF_CARD: &str = "";

/// How the roster kept on screen stands at its edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rest {
    /// It waits at its edge, drawn where it waits.
    Here,
    /// The next frame shows it coming in.
    Arriving,
    /// It keeps its pin but waits away from the desk: nobody else is here to show.
    Waiting,
    /// It is on its way back into the bar; once it has gone, the pin goes with it.
    Leaving,
}

/// The roster kept on screen: where it waits, how it arrived, and how it is dragged to another
/// edge of the desk. What it shows is the roster's own painting, at an edge.
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
    /// The pointer, in the desk's coordinates: where a drag of the panel is drawn under.
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
    overlay: CollaboratorsOverlay,
    rows: Vec<RosterRow>,
    /// Each person's screen as their presence says it, by roster id, `None` for someone who keeps
    /// where they are to themselves.
    screens: Vec<(String, Option<Screen>)>,
    workspace: String,
    expanded: Option<String>,
    /// Whose card the next frame opens, as the bar's face or the session asked: read once the
    /// roster has drawn the row it opens under.
    card_of: Option<String>,
    /// This person's own card is asked for: the next frame opens it in the roster's place, from the
    /// model the session builds of them.
    own_card: bool,
    /// The card that stood there goes, as the person asked for the roster instead: the next frame
    /// closes it.
    close_card: bool,
    /// Whose card stands in the roster's place, as the panel last drew it: while one does, there
    /// are no rows to read.
    card_open: Option<String>,
    /// Whom the wave they sent goes to, and since when.
    waved: Option<(String, ui::time::Instant)>,
    /// Where the pointer was over the desk: the panel opens under it.
    cursor: Option<Point>,
    /// Who else's pointer is over which tab, as the last frame drew them: their name and where they
    /// are.
    pointers: Vec<(String, String)>,
    /// Where each other person's pointer was drawn on this desk, as the last frame drew it: their
    /// name and the point, in the surface's coordinates.
    cursors: Vec<(String, (f64, f64))>,
    /// Where the desk is drawn, as the surface last laid out.
    area: Rect,
    /// The roster kept on screen at an edge, while the person keeps it there.
    pinned: Option<Pinned>,
}

impl Shown {
    /// Close the roster and any card standing in its place, for the next opening to find empty.
    fn close(&mut self) {
        self.expanded = None;
        self.card_of = None;
        self.own_card = false;
        self.card_open = None;
    }

    /// Whether the roster stands kept at an edge of the desk, drawn and pressed where it waits: a
    /// pin waiting away with nobody else here does not.
    fn standing(&self) -> bool {
        self.pinned
            .as_ref()
            .is_some_and(|pin| pin.rest != Rest::Waiting)
    }
}

/// The People panel and the other people's pointers: one surface above the chrome, so the desk
/// keeps its own drawing and the panel floats over it.
pub struct People {
    /// The layer the roster and everyone else's pointers are drawn on, once the host placed it.
    layer: RefCell<Option<Rc<dyn crate::platform::surfaces::Layer>>>,
    shown: Rc<RefCell<Shown>>,
    open: Rc<Cell<bool>>,
    /// When the person last pressed elsewhere, closing the roster.
    dismissed: Rc<Cell<Option<ui::time::Instant>>>,
    /// What the card asked of the session, carried out on its next pass.
    asks: Rc<RefCell<Vec<PeopleAsk>>>,
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
    shown.close();
    open.set(false);
    keep_pinned(true, edge);
}

/// Keep the pinned roster's edge in the person's settings, as the desk widgets keep theirs.
fn keep_pinned(pinned: bool, edge: DockEdge) {
    let kept = PEOPLE_OVERLAY.update(|_| PeopleOverlayPrefs { pinned, edge });
    if let Err(error) = kept {
        tracing::warn!(%error, "could not keep the roster's pin");
    }
}

impl People {
    /// Open the roster, or close it again: what the bar's faces do.
    pub fn toggle(&self) {
        // Kept on screen and standing, the roster is already where the faces would open it: they
        // have nothing to do, and the panel's own button puts it back in the bar.
        if self.shown.borrow().standing() {
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
            let mut shown = self.shown.borrow_mut();
            if open {
                // The roster comes in place of any card that stood there.
                shown.own_card = false;
                shown.card_of = None;
                shown.close_card = true;
            } else {
                shown.close();
            }
        }
        self.redraw();
    }

    /// Open this person's own card, or close it again: the bar's own avatar opens who they are
    /// here, in the same card as anyone else's.
    pub fn show_self(&self) {
        if self.open.get() && self.shown.borrow().card_open.as_deref() == Some(SELF_CARD) {
            self.dismiss();
            return;
        }
        {
            let mut shown = self.shown.borrow_mut();
            shown.own_card = true;
            shown.card_of = None;
        }
        self.open.set(true);
        self.dismissed.set(None);
        self.redraw();
    }

    /// Open the roster at the person `person` , their card in the roster's place: a press on them
    /// elsewhere (a face in the bar, a wave, the launcher) lands here.
    pub fn show_person(&self, person: &str) {
        // Their card stands already: the press asks for everyone here, in the card's place. The bar
        // has no people button while it shows a face, so this is how a pointer reaches the roster.
        if self.open.get() && self.shown.borrow().card_open.as_deref() == Some(person) {
            let mut shown = self.shown.borrow_mut();
            shown.own_card = false;
            shown.card_of = None;
            shown.close_card = true;
            drop(shown);
            self.redraw();
            return;
        }
        // A row this roster has not read yet still opens: the next frame reads it and the card
        // comes up under it.
        {
            let mut shown = self.shown.borrow_mut();
            shown.own_card = false;
            shown.card_of = Some(person.to_owned());
        }
        self.open.set(true);
        self.dismissed.set(None);
        self.redraw();
    }

    /// What the card asked since the last pass.
    pub fn take_asks(&self) -> Vec<PeopleAsk> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// Whether the roster stands over the desk now.
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
        self.shown.borrow_mut().close();
        self.dismissed.set(Some(ui::time::Instant::now()));
        self.redraw();
    }

    /// Draw again, though no input reached the panel: it was opened or closed by the session.
    fn redraw(&self) {
        if let Some(layer) = self.layer.borrow().as_ref() {
            layer.relayout();
        }
    }

    /// Draw in the person's theme, as their settings now have it.
    pub fn set_theme(&self, theme: ui::Theme) {
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
    ) -> Result<ResourceOperationStart, DoorError> {
        Err(DoorError::Unknown(address.clone()))
    }

    fn revision(&self) -> Option<u64> {
        // The roster is up or down, and its rows are the frame the host drew: read it again only
        // when one of those moved.
        let layer = self.layer().map_or(0, |layer| layer.revision());
        Some(layer | (u64::from(self.open.get()) << 63))
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        if !self.open.get() && !self.shown.borrow().standing() {
            return None;
        }
        self.layer().and_then(|layer| layer.a11y_tree())
    }

    fn a11y_name(&self) -> Option<String> {
        Some(crate::i18n::t("session-people"))
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        (self.open.get() || self.shown.borrow().standing())
            && self
                .layer()
                .is_some_and(|layer| layer.a11y_action(target, action))
    }

    fn wants_keyboard(&self) -> bool {
        self.open.get()
    }

    /// The controls the open roster, its card and the pinned roster show, where they stand: what
    /// the host drew.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        match self.layer() {
            Some(layer) if self.open.get() || self.shown.borrow().standing() => layer.controls(),
            _ => Vec::new(),
        }
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        match self.layer() {
            Some(layer) if self.open.get() || self.shown.borrow().standing() => layer.press(name),
            _ => Err(format!("no control named {name}")),
        }
    }

    /// Who is here, as the roster lists them: each person's name, what they are doing, and
    /// where they are. While a person's card stands in the roster's place, there are no rows to
    /// read: the card is what is read, and what is read is what the host drew of it.
    fn accessible_labels(&mut self) -> Vec<String> {
        if !self.open.get() && !self.shown.borrow().standing() {
            return Vec::new();
        }
        if self.shown.borrow().card_open.is_some() {
            return self
                .layer()
                .map_or_else(Vec::new, |layer| layer.accessible_labels());
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
        // Where everyone else is working: their pointer over a window of this desk says so, beside
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

/// What the roster reads and asks of the session, beside the surfaces it is placed on.
pub struct Given {
    /// Where the panel stands and how finely it draws.
    pub scale: f64,
    /// This device's person, as the panel names them before presence says otherwise.
    pub seated: String,
    /// What each window of the desk shows, by its tab: the record it holds, as
    /// the host that runs the windows reads them.
    pub notes: Rc<dyn Fn() -> std::collections::BTreeMap<u64, String>>,
    /// The apps this host has, as a card offers to write to the person.
    pub apps: Option<std::sync::Arc<dyn crate::platform::apps::Apps>>,
    /// The desk the roster is drawn over.
    pub handle: DesktopHandle,
    /// Who is here, when this session takes part in the collaboration plane.
    pub sync: Option<Rc<crate::sync::Client>>,
    /// Who is here, written into a copy of the desk's tree as its collaborators: what the bar's
    /// faces are drawn from, so the roster they open lists the same people. A host that hears
    /// some of them on a track of its own (the runs a workspace's host speaks for) writes them
    /// in here as it does for the bar.
    pub here: Rc<dyn Fn(&mut DesktopTree)>,
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
    let asks: Rc<RefCell<Vec<PeopleAsk>>> = Rc::default();
    let open = Rc::new(Cell::new(false));
    let dismissed: Rc<Cell<Option<ui::time::Instant>>> = Rc::default();
    let panel = Panel {
        shown: Rc::clone(&shown),
        open: Rc::clone(&open),
        handle: given.handle,
        drawn: Cell::new(0),
        seated: given.seated,
        sync: given.sync,
        here: given.here,
        notes: given.notes,
        apps: given.apps,
        intents: given.intents,
        invitable: given.invitable,
        asks: Rc::clone(&asks),
        profile: ProfileCard::default(),
        card_node: None,
        live_card: Cell::new(false),
        chats: RefCell::default(),
        motion_until: Cell::new(0.0),
        followed: RefCell::default(),
        banner: tilo_desktop::follow_banner::FollowBanner::default(),
    };
    let people = Rc::new(RefCell::new(People {
        layer: RefCell::new(None),
        shown,
        open: Rc::clone(&open),
        dismissed,
        asks,
    }));
    // A press anywhere else closes what the roster has open, as their own press does. The hook
    // stands beside the tree the layer shows, never in it, so holding the roster is no cycle.
    let roster = Rc::clone(&people);
    let heard = crate::platform::surfaces::Heard {
        wants_keyboard: Rc::new(move || open.get()),
        pressed_elsewhere: Rc::new(move || roster.borrow().dismiss()),
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
    /// leaving moves it, and a desk where nothing moved asks for no frame.
    drawn: Cell<u64>,
    /// The id this device sits as, which the person's own face falls back to before their presence
    /// names the root key they are here by.
    seated: String,
    sync: Option<Rc<crate::sync::Client>>,
    /// Who is here, written into the desk's collaborators as the bar's faces read them
    /// ([`Given::here`]).
    here: Rc<dyn Fn(&mut DesktopTree)>,
    /// What each window of the desk shows, by its tab: where a person's record stands, so the
    /// roster places them in the window they work in.
    notes: Rc<dyn Fn() -> std::collections::BTreeMap<u64, String>>,
    /// The apps this host has: whether Chat stands here decides whether a card offers to write to
    /// the person.
    apps: Option<std::sync::Arc<dyn crate::platform::apps::Apps>>,
    /// Where the bar's intents go: inviting from the roster's footer is the bar's own invite,
    /// raised here.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether the person may invite someone into the workspace in front, as the session's loop
    /// last answered it.
    invitable: Rc<Cell<bool>>,
    /// What the card asked of the session.
    asks: Rc<RefCell<Vec<PeopleAsk>>>,
    /// The one card of a person, as any host of it shows them.
    profile: ProfileCard,
    /// Where the card stands among this panel's children, while it is open.
    card_node: Option<NodeId>,
    /// Someone else's card no longer says what the rows say of them: the next layout builds it
    /// again, as the frame that opens one does ([`Self::settle_card`]).
    live_card: Cell<bool>,
    /// What everyone here says beside their pointer, as the toolkit times it for every host.
    chats: RefCell<ui::presence::CursorChats>,
    /// Keep drawing until then: the newest pointer heard plus the window it eases to rest in.
    motion_until: Cell<f64>,
    /// Whose view this device follows, as the last frame told the banner.
    followed: RefCell<Option<String>>,
    /// The "Following Ada" banner, as the page draws it, round the desk this panel floats over.
    banner: tilo_desktop::follow_banner::FollowBanner,
}

impl Panel {
    /// Stop following, and the workspace hears it once.
    fn stop_following(&self) {
        if let Some(sync) = &self.sync {
            sync.set_following(None);
        }
    }

    /// Follow the person with this roster id, or stop following them: one path for a card, a row
    /// and the banner, and the verb is the control's own — a press that stops following is not a
    /// gesture that begins it again.
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

    /// Leave the panel: what a press away from it does, whether the roster or a person's card stood
    /// there.
    fn dismiss(&mut self) {
        self.shown.borrow_mut().close();
        self.profile.close();
        self.open.set(false);
    }

    /// Open the card a face, a wave or the launcher asked for where the roster stands, or close the
    /// one the roster came back in place of: the person's card, as the web desk shows it, from the
    /// same model. The card is the roster's detail — the person, with their devices — not a second
    /// panel beside it.
    ///
    /// Opening a card builds its widgets anew and drops the ones that stood there, so it is done
    /// as the panel is laid out, never while it draws: the frame's layout maps point at the
    /// widgets it laid out, and input read through them until the next frame (the pointer's
    /// shape, a tooltip) would reach freed ones.
    fn settle_card(&mut self, theme: &Theme) {
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        if shown.close_card {
            shown.close_card = false;
            self.profile.close();
            let _ = self.profile.take_choice();
            shown.card_open = None;
        }
        // The card standing is someone else's: what the roster says of them now is what it says,
        // beside the row under it (an agent narrates every action). Built before the card is laid
        // out, as it is opened here, never while the frame draws.
        let standing = shown
            .card_open
            .clone()
            .filter(|who| who != SELF_CARD)
            .and_then(|who| self.person_model(&who, &shown.rows));
        if let Some(model) = standing {
            self.profile.updated(&model);
        }
        let asked = if shown.own_card {
            shown.own_card = false;
            Some(SELF_CARD.to_owned())
        } else {
            shown.card_of.take()
        };
        if let Some(who) = asked {
            let model = if who == SELF_CARD {
                Some(self.own_model())
            } else {
                self.person_model(&who, &shown.rows)
            };
            if let Some(model) = model {
                self.profile.close();
                let _ = self.profile.take_choice();
                self.profile
                    .toggle(&model, roster_place(shown.area, theme), shown.area);
                shown.card_open = self.profile.is_open().then_some(who);
            }
        }
    }

    /// Whether a card was asked to open or close since the panel was last laid out
    /// ([`Self::settle_card`]).
    fn card_asked(&self) -> bool {
        let shown = self.shown.borrow();
        shown.close_card || shown.own_card || shown.card_of.is_some()
    }

    /// Draw the card standing in the roster's place. Answers whether one stands there.
    fn draw_card(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) -> bool {
        if self.profile.is_open() {
            self.profile
                .draw_overlay(scene, transform, text, theme, presence);
        }
        self.profile.is_open()
    }

    /// The card of the person `who`, as this roster knows them: who they are,
    /// what they are doing, and what this person can do with them. `None` for
    /// someone the roster no longer lists.
    fn person_model(&self, who: &str, rows: &[RosterRow]) -> Option<ProfileModel> {
        let sync = self.sync.as_ref()?;
        let row = rows.iter().find(|row| row.id == who)?;
        let here = sync.here();
        let here = here.borrow();
        // Chat holds the conversation each pair of people shares, on the record that person's root
        // key names; someone whose key is not known here (a guest, before the host's own line
        // arrives) has none.
        let key = here.user_of(&row.id).map(str::to_owned);
        let linked = key
            .as_deref()
            .map(|key| here.linked_of(key).to_vec())
            .unwrap_or_default();
        // A conversation is Chat's, on the root key both people know: a guest this desk knows no
        // key for, and a device without Chat, have none to open.
        let can_message = !row.is_agent && key.is_some() && self.chat_installed();
        // Their devices are the person's, whichever of them the roster lists.
        let devices = if row.is_agent {
            Vec::new()
        } else {
            here.devices_of(&row.id)
        };
        Some(ProfileModel {
            name: row.label.clone(),
            // The face's letters and colour are the person's: the same on every device they are
            // here on.
            identity: key.clone().unwrap_or_else(|| row.id.clone()),
            colour: Some(row.color),
            key,
            devices,
            other: Some(OtherPerson {
                id: row.id.clone(),
                status: row.status.clone(),
                place: row.activity.clone(),
                presence: (!row.is_agent).then_some(row.availability),
                following: sync.own().following.as_deref() == Some(row.id.as_str()),
                can_follow: row.followable,
                can_message,
                linked,
                agent: row
                    .is_agent
                    .then(|| tilo_desktop::profile_card::AgentFacts {
                        client: ui::presence::agent_client(&row.label).to_owned(),
                    }),
            }),
            ..ProfileModel::default()
        })
    }

    /// This person's own card: who they are here, as the bar names them, what they say of
    /// themselves, the devices they are here on, and what they can do about their identity. The
    /// same card as anyone else's, with the rows only their own host offers.
    fn own_model(&self) -> ProfileModel {
        let own = self
            .sync
            .as_ref()
            .map_or_else(crate::sync::OwnPresence::default, |sync| sync.own());
        let (name, identity) = own_face(&own, &self.seated);
        let devices = self.sync.as_ref().map_or_else(Vec::new, |sync| {
            let here = sync.here();
            let here = here.borrow();
            here.devices_of(&sync.publisher().to_string())
        });
        // The identity's key is the person's on every device: the card shows it short, with Copy,
        // and the roster looks every face up by it.
        let key = own.user.clone();
        let (red, green, blue) = own_live_rgb(self.sync.as_deref(), &identity);
        ProfileModel {
            name,
            identity,
            colour: Some(Color::from_rgb8(red, green, blue)),
            colour_id: settings_store::identity::COLOUR.get(),
            can_rename: true,
            ask_name: own.label.trim().is_empty(),
            actions: Self::own_actions(),
            availability: own.availability,
            // Their own words: what this device read off their steps is not theirs to edit.
            status: own
                .status
                .as_ref()
                .filter(|status| !status.automatic)
                .map(|status| status.text.clone())
                .unwrap_or_default(),
            offers_presence: true,
            key,
            devices,
            ..ProfileModel::default()
        }
    }

    /// What the session offers this person about who they are: the pages where their identities are
    /// kept and backed up, and where another of their devices is paired.
    fn own_actions() -> Vec<ProfileAction> {
        vec![
            ProfileAction {
                id: "identities".to_owned(),
                title: crate::i18n::t("profile-action-identities"),
                group: ProfileGroup::Identity,
                icon: ui::icons::Icon::Fingerprint,
            },
            ProfileAction {
                id: "identity-backup".to_owned(),
                title: crate::i18n::t("profile-action-backup"),
                group: ProfileGroup::Identity,
                icon: ui::icons::Icon::Download,
            },
            ProfileAction {
                id: "pair-device".to_owned(),
                title: crate::i18n::t("profile-action-pair-device"),
                group: ProfileGroup::Devices,
                icon: ui::icons::Icon::MonitorSmartphone,
            },
        ]
    }

    /// The Settings page an own-card action names.
    fn own_settings_link(id: &str) -> Option<&'static str> {
        match id {
            "identities" => Some("settings://users"),
            "identity-backup" => Some("settings://about"),
            "pair-device" => Some("settings://devices_sync"),
            _ => None,
        }
    }

    /// Whether this device has the app that holds a person's conversations.
    fn chat_installed(&self) -> bool {
        self.apps
            .as_ref()
            .is_some_and(|apps| apps.installed(&LaunchpadAppAction::message_person("").app_id))
    }

    /// Carry out what the person chose in the card.
    fn take_choice(&mut self) {
        while let Some(choice) = self.profile.take_choice() {
            match choice {
                ProfileChoice::Follow(id) => {
                    let following = self
                        .sync
                        .as_ref()
                        .is_none_or(|sync| sync.own().following.as_deref() != Some(id.as_str()));
                    self.follow_person(&id, following);
                }
                ProfileChoice::Message(key) => {
                    self.asks.borrow_mut().push(PeopleAsk::Message { key });
                }
                ProfileChoice::Agent { client, ask } => {
                    self.asks
                        .borrow_mut()
                        .push(PeopleAsk::Agent { client, ask });
                }
                // The rows of this person's own card: the session keeps their name, what they say
                // of themselves and how available they are, and opens the pages their actions name.
                // Nothing here reaches anyone else's card.
                ProfileChoice::Rename(name) => {
                    self.asks.borrow_mut().push(PeopleAsk::Rename(name));
                }
                ProfileChoice::Status(status) => {
                    self.asks.borrow_mut().push(PeopleAsk::Status(status));
                }
                ProfileChoice::Availability(chosen) => {
                    self.asks.borrow_mut().push(PeopleAsk::Availability(chosen));
                }
                // The hints' switch is the person's record, which every device reads: the chrome
                // folds it in as their settings move.
                ProfileChoice::ControlHints(on) => {
                    let mut learned = settings_store::control_hints::LEARNED.get();
                    learned.enabled = on;
                    if let Err(error) = settings_store::control_hints::LEARNED.set(learned) {
                        tracing::warn!(%error, "could not keep the control hints' switch");
                    }
                }
                ProfileChoice::Colour(id) => {
                    self.asks.borrow_mut().push(PeopleAsk::Colour(id));
                }
                ProfileChoice::Action(id) => {
                    if let Some(link) = Self::own_settings_link(&id) {
                        self.asks
                            .borrow_mut()
                            .push(PeopleAsk::Settings(link.to_owned()));
                    }
                }
                // Nobody else's card asks these of this session, and this person's own keeps none
                // of them yet: what people here see is the workspace's own, and the desktop's hints
                // its own.
                ProfileChoice::Link(_)
                | ProfileChoice::Unlink(_)
                | ProfileChoice::Accept(_)
                | ProfileChoice::Decline(_)
                | ProfileChoice::Sharing(_)
                | ProfileChoice::Visibility(_) => {}
            }
        }
    }

    /// Read who is here, as the plane wrote them into the desk's collaborators this frame: the
    /// roster, each person's screen with its windows named by the records this desk shows, and the
    /// workspace it is of: the panel's title, kept on screen or opened from the bar.
    fn read_roster(&self, shown: &mut Shown) {
        let own = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        let records = (self.notes)();
        let mut tree = self.handle.read(Clone::clone);
        (self.here)(&mut tree);
        let (rows, _) = collaborator_roster_rows_named(&tree, &own, None);
        let workspace = tree
            .active_workspace()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default();
        let front = tree
            .active_workspace()
            .map(|workspace| workspace.id().to_string());
        // What each window here shows, by the digest presence names it with: what a person's window
        // on the same record is called here.
        let titled: BTreeMap<String, String> = crate::sync::layout::bindings(&tree, &records)
            .into_iter()
            .filter_map(|(tab, digest)| {
                let title = tree.tab(tab)?.title.trim().to_owned();
                (!title.is_empty()).then_some((digest, title))
            })
            .collect();
        // Each person's screen, its windows named with what they show where this desk holds that
        // record, else with the app's name.
        let apps = self.apps.as_deref();
        let screens = match (self.sync.as_ref(), front) {
            (Some(sync), Some(workspace)) => sync
                .members(&workspace)
                .into_iter()
                .map(|member| {
                    let name_of = |app: &str, record: &str| {
                        titled.get(record).cloned().or_else(|| {
                            let name = apps.map_or_else(String::new, |apps| apps.name(app));
                            (!name.trim().is_empty()).then_some(name)
                        })
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
        shown.rows = rows;
        shown.screens = screens;
        shown.workspace = workspace;
        // The card standing is someone else's: when what the rows say of them moved (an agent
        // narrates each action), this frame asks for the layout that builds the card with it.
        // Widgets are never built while the frame draws ([`Self::settle_card`]).
        if let Some(who) = shown.card_open.clone().filter(|who| who != SELF_CARD)
            && self.profile.is_open()
            && let Some(model) = self.person_model(&who, &shown.rows)
            && !self.profile.shows(&model)
        {
            self.live_card.set(true);
        }
    }

    /// Act on what a press or a key on the roster meant.
    fn follow(&mut self, hit: CollaboratorsHit, shown: &mut Shown) {
        match hit {
            CollaboratorsHit::Dismiss => {
                shown.close();
                self.profile.close();
                self.open.set(false);
            }
            CollaboratorsHit::ToggleDetails(index) => {
                let id = shown.rows.get(index).map(|row| row.id.clone());
                shown.expanded = shown
                    .expanded
                    .clone()
                    .filter(|open| Some(open) != id.as_ref())
                    .or(id);
            }
            CollaboratorsHit::Follow(index) => {
                if let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) {
                    self.follow_person(&id, true);
                }
            }
            CollaboratorsHit::Unfollow(index) => {
                if let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) {
                    self.follow_person(&id, false);
                }
            }
            CollaboratorsHit::Wave(index) => {
                let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                if let Some(sync) = &self.sync {
                    sync.wave(&id);
                    shown.waved = Some((id, ui::time::Instant::now()));
                }
            }
            // The window software works in: brought forward, so what it is on stands in front of
            // this person.
            CollaboratorsHit::OpenTab(index) => {
                let tab = shown
                    .rows
                    .get(index)
                    .and_then(|row| row.working_in.as_ref())
                    .map(|(tab, _)| *tab);
                if let Some(tab) = tab {
                    let command = DeskEffect::Focus {
                        tab: tab.to_string(),
                    };
                    if let Err(error) = self.handle.dispatch(command) {
                        tracing::warn!(%error, "the window software works in could not come forward");
                    }
                }
            }
            // Their recent changes, where this workspace's history has them.
            CollaboratorsHit::OpenChanges(index) | CollaboratorsHit::OpenChange(index, _) => {
                let Some(author) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                self.open_activity(&author);
            }
            // Inviting from here is the bar's own invite: the roster offers its footer only while
            // the person may hand out a link into this workspace.
            CollaboratorsHit::Invite => {
                self.intents.borrow_mut().push(ChromeIntent::Invite);
            }
            // Keeping the roster on screen moves it out of the bar to an edge of the desk; pressed
            // where it stands, it goes back into the bar.
            CollaboratorsHit::Pin => {
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
            CollaboratorsHit::JoinCall(_) | CollaboratorsHit::Absorb => {}
        }
    }

    /// How many times what the desk's people say of themselves changed.
    fn presence_revision(&self) -> u64 {
        self.sync
            .as_ref()
            .map_or(0, |sync| sync.presence_revision())
    }

    /// Open this workspace's activity at what `author` changed.
    fn open_activity(&self, author: &str) {
        let workspace = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.slot.0.to_string())
        });
        let Some(workspace) = workspace else {
            return;
        };
        let command = ChromeEffect::Activity {
            workspace,
            view: ActivityView::CatchUp,
            author: Some(author.to_owned()),
        };
        if let Err(error) = self.handle.dispatch(command) {
            tracing::warn!(%error, "activity could not open for that person");
        }
    }

    /// Where everyone here points, drawn by the one rule both hosts draw pointers by: over the
    /// tile of the tab it is on, in the colour and under the name the workspace knows them by,
    /// with what they say beside it. What each tab is called names where they are.
    fn draw_pointers(
        &self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        shown: &mut Shown,
    ) {
        let _ = presence;
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
                    &tiles,
                    &mut self.chats.borrow_mut(),
                    presence,
                    ui::time::now(),
                    Self::presence_ms(),
                    theme.motion(),
                );
            }
        }
        if let Some(until) = pointers.motion_until {
            self.motion_until.set(self.motion_until.get().max(until));
        }
        // Who is here is read once, for the rows and the card opened on one of them, for the pinned
        // roster's cards, and for the banner that names whose view this is.
        let following = self.sync.as_ref().and_then(|sync| sync.own().following);
        if open || shown.pinned.is_some() || !pointers.drawn.is_empty() || following.is_some() {
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
            ui::presence::draw_peer_cursors(scene, transform, area, text, theme, &pointers.drawn);
        }
    }

    /// While this person follows someone, the banner names whose view this is and offers Stop: the
    /// same banner the page draws, from the same widget.
    fn draw_follow(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        shown: &Shown,
    ) {
        let following = self.sync.as_ref().and_then(|sync| sync.own().following);
        // The roster's own row for them: the same face, name and colour the roster lists.
        let row = following
            .as_deref()
            .and_then(|id| shown.rows.iter().find(|row| row.id == id).cloned());
        let was = self.followed.replace(following.clone());
        if was != following {
            let now = ui::time::now();
            if let Some(row) = row.clone() {
                self.banner.started(
                    Some(row),
                    theme.motion(),
                    ui::tokens::MotionSource::Pointer,
                    now,
                );
            } else {
                // They left: following them ends with them. Still here, the person's own input
                // took their view back, and the banner says so a moment.
                let left = was
                    .as_deref()
                    .is_some_and(|id| !shown.rows.iter().any(|row| row.id == id));
                self.banner.ended(
                    !left,
                    theme.motion(),
                    ui::tokens::MotionSource::Pointer,
                    now,
                );
            }
        }
        let area = shown.area;
        let bar_h = f64::from(tilo_desktop::topbar::topbar_height(theme));
        let below_bar = Rect::new(area.x0, area.y0 + bar_h, area.x1, area.y1);
        // The banner docks in the top bar, above every window's content: it stands on nothing the
        // person reads, at any width.
        let bar = Rect::new(area.x0, area.y0, area.x1, area.y0 + bar_h);
        let kept = self.banner.draw(
            scene,
            transform,
            text,
            theme,
            tilo_desktop::follow_banner::BannerFrame {
                work_area: below_bar,
                bar,
                cursor: shown.cursor,
                motion: theme.motion(),
            },
            following.map(|_| row),
        );
        if !kept {
            self.stop_following();
        }
    }

    /// The roster, drawn where it stands: under the bar as the faces open it, or kept on screen at
    /// an edge with a mirror of each person's screen. One painting for both.
    fn draw_roster(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        shown: &mut Shown,
    ) {
        let now = ui::motion::frame_now();
        let motion = theme.motion();
        // The roster is the panel's only while it is up or kept at an edge: closed, it draws
        // nothing, so the surface stands transparent and a press under the bar reaches the bar.
        if shown.pinned.is_none() && !self.open.get() {
            return;
        }
        // The bar's faces open the roster where they stand, kept on screen or not.
        if self.open.get() {
            self.draw_one_roster(scene, transform, text, theme, presence, shown, None);
            return;
        }
        if let Some(pin) = shown.pinned.as_mut() {
            match pin.rest {
                Rest::Leaving => {
                    pin.shown
                        .show(false, Appear::SPEC, motion, ui::motion::input_source(), now);
                    if !pin.shown.is_visible(now) {
                        shown.pinned = None;
                    }
                }
                // Kept at an edge with nobody else here, the roster has nothing to say but that:
                // it waits away from the desk, its pin kept, and comes in when someone arrives.
                _ if shown.rows.is_empty() => {
                    if pin.rest != Rest::Waiting {
                        pin.rest = Rest::Waiting;
                        pin.shown.snap(false);
                    }
                }
                Rest::Arriving | Rest::Waiting => {
                    pin.rest = Rest::Here;
                    pin.shown
                        .show(true, Appear::SPEC, motion, ui::motion::input_source(), now);
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
        let standing = {
            let Some(pin) = shown.pinned.as_mut() else {
                return;
            };
            Self::place_pinned(pin, area, theme, now);
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
        let (pose, slot) = standing;
        let placed = pose.applied_to(transform);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            placed,
            &slot.inflate(32.0, 32.0),
        );
        self.draw_one_roster(scene, placed, text, theme, presence, shown, Some(slot));
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
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        shown: &mut Shown,
        panel: Option<Rect>,
    ) {
        let card_stands = self.draw_card(scene, transform, text, theme, presence);
        if card_stands {
            return;
        }
        shown.card_open = None;
        let pinned = shown.pinned.is_some();
        let own_id = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        let following = self.sync.as_ref().map_or_else(Vec::new, |sync| {
            let target = sync.own().following.unwrap_or_default();
            if target.is_empty() {
                Vec::new()
            } else {
                vec![(own_id, target)]
            }
        });
        let followed = self.sync.as_ref().and_then(|sync| sync.own().following);
        let expanded = shown.expanded.clone();
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
        let model = CollaboratorsModel {
            host: shown.area,
            workspace: &workspace,
            rows: &rows,
            may_invite: self.invitable.get(),
            may_pin: !pinned,
            pinned,
            panel,
            screens: &screens,
            followed_id: followed.as_deref(),
            following_pairs: &following,
            expanded_id: expanded.as_deref(),
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
    fn place_pinned(pin: &mut Pinned, area: Rect, theme: &Theme, now: ui::time::Instant) {
        let bar = f64::from(tilo_desktop::topbar::topbar_height(theme));
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
        let slot = desk_edges::place(
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
                ui::tokens::MotionSource::Gesture,
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

    /// What a drag of the pinned roster asks: the pointer moved, a press started on it, or it was
    /// let go at an edge.
    fn handle_pinned(&self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        let cursor = shown.cursor;
        let mut handled = false;
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
                    // The pointer left the desk: a press in flight lets go where it was drawn.
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
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt;
        self.settle_card(theme);
        // The card belongs to the roster: a roster that closed took it with it, and the card's
        // place among these children with that.
        if !self.open.get() {
            self.profile.close();
        }
        self.card_node = self
            .profile
            .is_open()
            .then(|| self.profile.build_layout(taffy, theme, map));
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
        match self.card_node {
            Some(card) => taffy.widget_node(style, &[card], map, self),
            None => taffy.widget_leaf(style, map, self),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.shown.borrow_mut().area = ui::widget::layout_rect(taffy, node, origin);
        if let Some(card) = self.card_node {
            self.profile.apply_layout(taffy, card, origin);
        }
    }

    fn get_rect(&self) -> Rect {
        self.shown.borrow().area
    }

    fn children(&self) -> Vec<&dyn Widget> {
        match self.card_node {
            Some(_) if self.profile.is_open() => vec![&self.profile],
            _ => Vec::new(),
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match self.card_node {
            Some(_) if self.profile.is_open() => vec![&mut self.profile],
            _ => Vec::new(),
        }
    }

    fn traps_focus(&self) -> bool {
        self.profile.traps_focus()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.profile.modal_capture()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.profile.wants_pointer_capture()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let shown = self.shown.borrow();
        if (self.open.get() || shown.standing()) && !self.profile.is_open() {
            // Each control says where it stands and carries what pressing it does, so a reader
            // presses the roster's own buttons as the person does.
            return Some(shown.overlay.accessibility_with_targets());
        }
        drop(shown);
        // The banner stands beside the windows, so it is read whether the roster is up or not.
        self.banner.accessibility()
    }

    /// Following's Stop is the banner's own verb, and the pinned roster's controls are its own,
    /// pressed there by a reader as by the person.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        if target == tilo_desktop::follow_banner::STOP_TARGET
            && matches!(action, ui::a11y::Action::Activate)
        {
            self.stop_following();
            return true;
        }
        if !matches!(action, ui::a11y::Action::Activate) {
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

    /// The card opening or closing changes this panel's tree: the row it stands under is laid out
    /// again, and a card that went away leaves no node behind.
    fn take_structure_dirty(&mut self) -> bool {
        let open = self.profile.is_open();
        let card = self.profile.take_structure_dirty();
        card || open != self.card_node.is_some()
            || self.card_asked()
            || self.live_card.replace(false)
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let revision = self.presence_revision();
        // The card is this panel's own, in the roster's place: what it shows is read here, while
        // the roster's state is borrowed.
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        if !self.open.get() {
            // The card stood in the panel: closed with it, never left for the next opening to find.
            self.profile.close();
        }
        self.draw_pointers(scene, transform, text, theme, presence, &mut shown);
        self.draw_roster(scene, transform, text, theme, presence, &mut shown);
        self.draw_follow(scene, transform, text, theme, &shown);
        self.drawn.set(revision);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Where the pointer is, as the panel draws by it: under the pointer of the bar's faces, the
        // banner's Stop and every other person's name.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.shown.borrow_mut().cursor = Some(logical(*position, ctx.scale_factor));
        }
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.shown.borrow_mut().cursor = None;
        }
        // The banner's Stop takes its press whether the roster is open or not: following is the
        // person's view, not the roster's.
        if let WindowEvent::MouseInput {
            state: ui::winit::event::ElementState::Pressed,
            ..
        } = event
            && let Some(point) = self.shown.borrow().cursor
            && self.banner.stop_rect().contains(point)
        {
            self.stop_following();
            *ctx.handled = true;
            return;
        }
        // Kept on screen, the roster is pressed by the person's own pointer: it draws and answers
        // where it stands, and never takes the keyboard.
        let kept = self.shown.borrow().standing();
        if !self.open.get() && !kept {
            self.handle_pinned(event, ctx);
            return;
        }
        // The card stands where the roster would, so there are no rows under it to press: it takes
        // the keyboard and the presses itself, and a press away from it leaves the panel as a press
        // away from the roster does.
        if self.profile.is_open()
            && let Some(node) = self.card_node
        {
            self.profile
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            // A press on one of the card's controls is the control's own verb, answered where the
            // widget hosts it: the same drain a window's host runs for what its tree publishes.
            let path = ctx.path;
            ui::widget::controls::start_pending_control_invocations(
                &mut self.profile,
                &mut ctx.edits(),
                path,
            );
            let modal = self.profile.modal_capture().is_some();
            self.take_choice();
            if !self.profile.is_open() {
                // The card closed itself: it stood where the roster would,
                // so there is nothing left standing here.
                self.dismiss();
                *ctx.handled = true;
                return;
            }
            if *ctx.handled || modal {
                *ctx.handled = true;
                return;
            }
        }
        // The roster's state is its own handle, so what a press does may change the card this panel
        // hosts in its place.
        let roster = Rc::clone(&self.shown);
        let mut shown = roster.borrow_mut();
        let cursor = shown.cursor;
        let hit = shown.overlay.handle_event(event, ctx, true, cursor);
        let Some(hit) = hit else {
            return;
        };
        // A press away from the roster takes the card with it.
        if matches!(hit, CollaboratorsHit::Dismiss) {
            self.profile.close();
        }
        // Kept on screen, a press on its header drags it to another edge: a card's own press is
        // the card's.
        let drags = shown.standing()
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
    /// the desk; closed, only once somebody else's pointer has moved or left since the last frame,
    /// a bubble or a reaction is playing, or the banner is still on its way in or out. A desk where
    /// a remote pointer rests where it was drawn asks for no frame.
    fn needs_redraw(&self) -> bool {
        let now = ui::time::now();
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
                || self.banner.is_moving(now)
                || Self::presence_ms() < self.motion_until.get();
        }
        self.open.get()
            || self.presence_revision() != self.drawn.get()
            || self.chats.borrow().animating(now)
            || self.banner.is_moving(now)
            || Self::presence_ms() < self.motion_until.get()
    }

    fn name(&self) -> &'static str {
        "PeoplePanel"
    }

    fn test_id(&self) -> Option<&str> {
        Some("people-panel")
    }
}

/// Where the roster stands on the desk: under the bar, at the desk's right edge. A card opened for
/// one of its rows stands exactly there, in its place.
fn roster_place(area: Rect, theme: &Theme) -> Rect {
    let metrics = &theme.tokens.metrics;
    let spacing = &theme.tokens.spacing;
    let width = f64::from(metrics.panel_width_list)
        .min((area.width() - 2.0 * f64::from(spacing.s4)).max(240.0));
    let right = area.x1 - f64::from(spacing.s3);
    let top =
        area.y0 + f64::from(tilo_desktop::topbar::topbar_height(theme)) + f64::from(spacing.s2);
    Rect::new(right - width, top, right, top)
}

#[cfg(all(test, target_os = "linux"))]
#[path = "people_scale_tests.rs"]
mod scale_tests;
