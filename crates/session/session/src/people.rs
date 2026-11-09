//! Who is here: the roster the bar's faces open, and every other person's pointer where they are
//! working.
//! The roster reads the desk's collaborators, which the collaboration plane writes each frame
//! ([`crate::sync`]): the one feed the bar's faces and the launcher's Happening now list read too.
//! Everyone's pointer is the plane's tile pointer, drawn over the tile of the tab they are on, in
//! their own colour.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use ui::time::Duration;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::{ActivityView, ChromeEffect};
use tilo_session_core::door::{Look, Published};
use ui::host_operation::ResourceOperationStart;
use ui::kurbo::{Affine, Point, Rect};
use ui::peniko::Color;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::winit::event::WindowEvent;

use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, RosterRow,
    collaborator_roster_rows_named,
};
use tilo_desktop::launchpad::LaunchpadAppAction;
use tilo_desktop::profile_card::{
    OtherPerson, ProfileAction, ProfileCard, ProfileChoice, ProfileGroup, ProfileModel,
};

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
    let chosen = settings_store::identity::COLOUR
        .get()
        .as_deref()
        .and_then(ui::tokens::PresenceHue::from_id);
    let hue = chosen.unwrap_or_else(|| ui::tokens::PresenceHue::of_identity(identity));
    ui::presence::identity_rgb(hue)
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

/// What the panel shows and where it stands, kept at rest between frames.
#[derive(Default)]
struct Shown {
    overlay: CollaboratorsOverlay,
    rows: Vec<RosterRow>,
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
}

impl Shown {
    /// Close the roster and any card standing in its place, for the next opening to find empty.
    fn close(&mut self) {
        self.expanded = None;
        self.card_of = None;
        self.own_card = false;
        self.card_open = None;
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

impl People {
    /// Open the roster, or close it again: what the bar's faces do.
    pub fn toggle(&self) {
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
        if !self.open.get() {
            return None;
        }
        self.layer().and_then(|layer| layer.a11y_tree())
    }

    fn a11y_name(&self) -> Option<String> {
        Some(crate::i18n::t("session-people"))
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        self.open.get()
            && self
                .layer()
                .is_some_and(|layer| layer.a11y_action(target, action))
    }

    fn wants_keyboard(&self) -> bool {
        self.open.get()
    }

    /// The controls the open roster and its card show, where they stand: what the host drew.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        match self.layer() {
            Some(layer) if self.open.get() => layer.controls(),
            _ => Vec::new(),
        }
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        match self.layer() {
            Some(layer) if self.open.get() => layer.press(name),
            _ => Err(format!("no control named {name}")),
        }
    }

    /// Who is here, as the open roster lists them: each person's name, what they are doing, and
    /// where they are. While a person's card stands in the roster's place, there are no rows to
    /// read: the card is what is read, and what is read is what the host drew of it.
    fn accessible_labels(&mut self) -> Vec<String> {
        if !self.open.get() {
            return Vec::new();
        }
        if self.shown.borrow().card_open.is_some() {
            return self
                .layer()
                .map_or_else(Vec::new, |layer| layer.accessible_labels());
        }
        let shown = self.shown.borrow();
        let snapshot = shown.overlay.accessibility_snapshot();
        let mut labels = Vec::new();
        let mut pending = vec![snapshot];
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
        notes: given.notes,
        apps: given.apps,
        intents: given.intents,
        invitable: given.invitable,
        asks: Rc::clone(&asks),
        profile: ProfileCard::default(),
        card_node: None,
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
        let Some(sync) = &self.sync else {
            return;
        };
        let mut own = sync.own();
        if own.following.take().is_some() {
            sync.set_own(own);
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

    /// Open the card a face, a wave or the launcher asked for where the roster stands, and draw it:
    /// the person's card, as the web desk shows it, from the same model. The card is the roster's
    /// detail — the person, with their devices — not a second panel beside it. Answers whether the
    /// card now stands there.
    fn draw_card(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        shown: &mut Shown,
    ) -> bool {
        if shown.close_card {
            shown.close_card = false;
            self.profile.close();
            let _ = self.profile.take_choice();
            shown.card_open = None;
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
        let (red, green, blue) = own_rgb(&identity);
        ProfileModel {
            name,
            identity,
            colour: Some(Color::from_rgb8(red, green, blue)),
            colour_id: settings_store::identity::COLOUR.get(),
            can_rename: true,
            ask_name: own.label.trim().is_empty(),
            actions: Self::own_actions(),
            availability: own.availability,
            status: own.status.clone().unwrap_or_default(),
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
                    let Some(sync) = &self.sync else {
                        continue;
                    };
                    let mut own = sync.own();
                    own.following = (own.following.as_deref() != Some(id.as_str())).then_some(id);
                    sync.set_own(own);
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
    /// roster, and the workspace it is of.
    fn read_roster(&self) -> (Vec<RosterRow>, String) {
        let own = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        let app_name = |_: &str| None;
        let mut tree = self.handle.read(Clone::clone);
        if let Some(sync) = &self.sync {
            sync.show(&mut tree, &(self.notes)(), &app_name);
        }
        let (rows, _) = collaborator_roster_rows_named(&tree, &own, None);
        let workspace = tree
            .active_workspace()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default();
        (rows, workspace)
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
            CollaboratorsHit::ToggleFollow(index) => {
                let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                let Some(sync) = &self.sync else {
                    return;
                };
                let mut own = sync.own();
                own.following = (own.following.as_deref() != Some(id.as_str())).then_some(id);
                sync.set_own(own);
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
            // No call to join from here (a call is Chat's, in an app window),
            // and nothing else to do: the roster kept the press.
            CollaboratorsHit::JoinCall(_) | CollaboratorsHit::Pin | CollaboratorsHit::Absorb => {}
        }
    }

    /// What the open panel draws: the card standing where the roster would,
    /// or the roster's own rows and its detail.
    fn draw_open(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        shown: &mut Shown,
    ) {
        // The card is the roster's detail: opened on one of its rows, it stands where the roster
        // would, and the rows wait behind it.
        let card_stands = if shown.card_of.is_some() || shown.own_card || self.profile.is_open() {
            self.draw_card(scene, transform, text, theme, presence, shown)
        } else {
            false
        };
        if card_stands {
            return;
        }
        shown.card_open = None;
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
            unavailable: None,
            exposure: None,
            // Inviting is the invite sheet's own surface here, offered from the footer while the
            // person may hand out a link into this workspace; keeping the list on screen is not
            // offered yet.
            may_invite: self.invitable.get(),
            may_pin: false,
            followed_id: followed.as_deref(),
            following_pairs: &following,
            expanded_id: expanded.as_deref(),
            watching: 0,
            waved: &waved,
            waved_until,
        };
        let cursor = shown.cursor;
        shown
            .overlay
            .draw(scene, transform, text, theme, &model, cursor);
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
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        shown: &mut Shown,
    ) {
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
        // Who is here is read once, for the rows and the card opened on one of them.
        if open || !pointers.drawn.is_empty() {
            let (rows, workspace) = self.read_roster();
            shown.rows = rows;
            if open {
                shown.workspace = workspace;
            }
        }
        if open {
            self.draw_open(scene, transform, text, theme, presence, shown);
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
        presence: &ui::presence::PresenceCtx,
        shown: &Shown,
    ) {
        let following = self.sync.as_ref().and_then(|sync| sync.own().following);
        let names = if following.is_some() || self.followed.borrow().is_some() {
            self.sync.as_ref().map_or_else(HashMap::new, |sync| {
                let here = sync.here();
                let here = here.borrow();
                here.person_names()
            })
        } else {
            HashMap::new()
        };
        let row = following.as_deref().and_then(|id| {
            names.get(id).map(|(label, _)| {
                // Their colour is the one their pointer is drawn in on this desk, so the frame and
                // the bar read as following them and nothing else here.
                let user = ui::permissions::UserId(id.to_owned());
                let mut row = RosterRow::own(
                    id.to_owned(),
                    label.clone(),
                    ui::presence::presence_colour(presence.color_for(&user)),
                    None,
                );
                row.is_agent = presence.is_agent(&user);
                row
            })
        });
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
                let left = was.is_some_and(|id| !names.contains_key(&id));
                self.banner.ended(
                    !left,
                    theme.motion(),
                    ui::tokens::MotionSource::Pointer,
                    now,
                );
            }
        }
        let area = shown.area;
        let bar = f64::from(tilo_desktop::topbar::topbar_height(theme));
        let below_bar = Rect::new(area.x0, area.y0 + bar, area.x1, area.y1);
        // A window's bar, which carries the buttons its end: the banner keeps off every one of
        // them, as it never stands on a control of the person's own.
        let title = f64::from(theme.tokens.metrics.bar_height);
        let window_bars: Vec<Rect> = self
            .handle
            .shown_tiles()
            .into_iter()
            .map(|(_, window)| Rect::new(window.x0, window.y0, window.x1, window.y0 + title))
            .collect();
        let kept = self.banner.draw(
            scene,
            transform,
            text,
            theme,
            tilo_desktop::follow_banner::BannerFrame {
                work_area: below_bar,
                viewport: below_bar,
                window_bars: &window_bars,
                cursor: shown.cursor,
                motion: theme.motion(),
            },
            following.map(|_| row),
        );
        if !kept {
            self.stop_following();
        }
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
        if self.open.get() && !self.profile.is_open() {
            Some(self.shown.borrow().overlay.accessibility_snapshot())
        } else {
            // The banner stands beside the windows, so it is read whether the roster is up or not.
            self.banner.accessibility()
        }
    }

    /// Following's Stop is the banner's own verb, pressed there by a reader as by the person.
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
        false
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
        self.draw_follow(scene, transform, text, theme, presence, &shown);
        self.drawn.set(revision);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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
        if !self.open.get() {
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
        let Some(hit) = shown.overlay.handle_event(event, ctx, true, cursor) else {
            return;
        };
        // A press away from the roster takes the card with it, as `follow` does.
        self.follow(hit, &mut shown);
    }

    /// Whether the panel owes a frame: while it is open, because its rows move with the people on
    /// the desk; closed, only once somebody else's pointer has moved or left since the last frame,
    /// a bubble or a reaction is playing, or the banner is still on its way in or out. A desk where
    /// a remote pointer rests where it was drawn asks for no frame.
    fn needs_redraw(&self) -> bool {
        let now = ui::time::now();
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
