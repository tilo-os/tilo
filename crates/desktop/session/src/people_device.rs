//! Who is here, as the shell reads it: every member of the workspace in front who is here now, the
//! person at this device, whom they follow and whom the bar has to tell them about.
//!
//! The roster is the session's reading of presence and the workspace's member rows, published as
//! `session:people` for a shell in another language to draw. Nothing is kept beyond what a reading
//! cannot derive: the card the person opened (shared with the cursor the shell's own press leaves),
//! and the last wave or arrival the bar still shows.
//!
//! What each person's row says: their name, the colour the workspace gives them, where they are
//! (the window they are in, or the app when this shell shows no window of it), what they say they
//! are doing, how available they are, the call they are in, whom they follow and when they joined,
//! whether this person follows them and whether they follow this person. Software is a member too:
//! an agent's or a bot's row says which it is, and an agent's says the window it works in. What the
//! row's words are is the shell's, so each language says them its own way; the reading carries the
//! facts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use collab_types::Member;
use serde_json::{Map, Value};
use tilo_actions::ApiError;
use tilo_desktop_model::Layout;
use tilo_device_surfaces::people::{
    People, PeopleEffect, Person, PersonAction, PersonDevice, PersonLinked, PresenceColour,
};
use tilo_service::{Service, Started, decode, offer, reading};

/// The resource this session publishes the roster as.
pub(crate) const RESOURCE: &str = "session:people";

/// The card the roster shows for the person at this device, instead of anyone else's.
pub(crate) const SELF: &str = "self";

/// How long the bar says who waved at the person: the waver's face bounces meanwhile.
pub(crate) const WAVE_SHOWN: ui::time::Duration = ui::time::Duration::from_secs(6);
/// How long the wave this person sent shows as sent on the row it went to.
pub(crate) const WAVE_SENT_SHOWN: ui::time::Duration = ui::time::Duration::from_millis(2400);
/// How long the bar says who arrived where the person is.
pub(crate) const ARRIVAL_SHOWN: ui::time::Duration = ui::time::Duration::from_secs(4);
/// How long the bar says "Stopped following" after the person's own input ended the following.
pub(crate) const STOPPED_SHOWN: ui::time::Duration = ui::time::Duration::from_secs(1);

/// How long after the last keystroke the person's own card keeps what was typed: the pause a
/// person takes when they have finished writing a name or a status, without reaching for Enter.
pub(crate) const TYPING_SETTLE: ui::time::Duration = ui::time::Duration::from_millis(700);

/// The settings page that opens everything that acts as the person, and taking any of it back.
const DEVICES_ACCESS: &str = "devices-access";
/// The person's own settings, at the top of their card.
const OPEN_SETTINGS: &str = "settings";
/// Another of the person's devices: Settings' devices page prints the ticket it joins with.
const PAIR_DEVICE: &str = "pair-device";
/// The host's action that opens the picture picker, which the card offers beside the avatar.
const SET_PHOTO: &str = "set-photo";
/// The host's action that takes the person's picture away again.
const RESET_PHOTO: &str = "reset-photo";

/// The Settings page an own-card action names, as [`crate::people::PeopleAsk::Settings`] takes it.
fn own_settings_link(id: &str) -> Option<&'static str> {
    match id {
        OPEN_SETTINGS => Some("settings://"),
        DEVICES_ACCESS => Some("settings://devices_access"),
        PAIR_DEVICE => Some("settings://devices_sync"),
        _ => None,
    }
}

/// How long after the person pressed elsewhere a press on the faces does not reopen the roster:
/// their press closed it.
pub(crate) const DISMISS_GRACE: ui::time::Duration = ui::time::Duration::from_millis(250);

/// The people resource, as the session's catalogue holds it.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "People", effects = PeopleEffect)]
pub struct PeopleRoot;

/// Who is here, and the verbs that change what the bar says of them.
pub(crate) struct PeopleDevice {
    handle: tilo_session_core::desktop::state::DesktopHandle,
    /// Who is here, placed into a copy of the layout as the members of the workspace in front: the
    /// tree the loop holds lists nobody.
    place_here: Rc<dyn Fn(&mut Layout)>,
    /// The live session: who is here, whom this person follows, and what the bar must tell them.
    sync: Option<Rc<crate::sync::Client>>,
    /// What a person's card asked of the session, carried out on the next pass: the queue the
    /// roster and the bar share.
    asks: Rc<RefCell<Vec<crate::people::PeopleAsk>>>,
    /// The surfaces the shell shows over its windows, and whose card the roster shows.
    surfaces: crate::device_lists::Surfaces,
    /// Whether this person may invite someone into the workspace in front, as the loop last
    /// answered it: the roster offers its invite only then.
    invitable: Rc<Cell<bool>>,
    /// The person at this device, by the id their own presence goes by.
    person: String,
    /// Whom the wave this person sent just now went to, and when: their row says it went for a
    /// moment.
    waved_to: RefCell<Option<(String, ui::time::Instant)>>,
    /// Whether this device has the app that holds a person's conversations, which is what a card
    /// needs to offer one.
    chat: Rc<dyn Fn() -> bool>,
    /// What the person typed into their own card and the card has not kept yet: their name and
    /// what they say they are doing, each with when it was last typed. The card asks for these on
    /// every change, and they are kept once typing pauses, on Enter, and when anything closes the
    /// card, so nothing typed is lost for want of a Save.
    drafts: RefCell<Drafts>,
    /// The name and the words the card last asked the session to keep: what its fields are
    /// compared with until the reading carries them, so Enter and the field losing focus in the
    /// same frame is one change, never two.
    kept: RefCell<(String, String)>,
    /// What the reading said last, so a pass that changed nothing is no revision.
    published: RefCell<Option<People>>,
    /// What moved the reading last: presence, the layout, the card, the person and the notices.
    /// A pass whose key is unchanged reads nothing.
    seen: RefCell<Option<Key>>,
    /// Whom this device followed when the reading last looked: a following that ends with them
    /// still here ended by the person's own input, and the bar says so a moment.
    followed: RefCell<String>,
    /// Whose view the person's own input stopped, and when, while the bar still says so.
    stopped: RefCell<Option<(String, ui::time::Instant)>>,
    /// When the panel last closed: the press that closed it does not open it again.
    closed: RefCell<Option<ui::time::Instant>>,
}

/// What the person has typed into their own card and nothing has kept yet.
#[derive(Default)]
struct Drafts {
    name: Option<(String, ui::time::Instant)>,
    doing: Option<(String, ui::time::Instant)>,
}

/// What the person's own card offers and wears, as one value the pass compares: the host's
/// actions, the settings the card reads, and whether a conversation can be opened.
#[derive(Clone, PartialEq, Eq)]
struct Own {
    actions: Vec<(String, String, String, String)>,
    photo_change: String,
    photo_remove: String,
    control_hints: Option<bool>,
    colour_id: String,
    colour_slot: Option<u32>,
    given: String,
    chat: bool,
}

/// Everything that changes the reading, cheaply: who is here, what the shell shows, this person
/// and what the bar still has to say.
type Key = (
    u64,
    u64,
    u64,
    String,
    String,
    String,
    String,
    String,
    Vec<String>,
    (String, String),
    // A tuple holds twelve values; what the person may invite, whom they waved at, a follow they
    // just ended and what their own card offers close the key as one.
    (bool, String, String, Own),
);

impl PeopleDevice {
    pub(crate) fn new(
        handle: tilo_session_core::desktop::state::DesktopHandle,
        place_here: Rc<dyn Fn(&mut Layout)>,
        sync: Option<Rc<crate::sync::Client>>,
        asks: Rc<RefCell<Vec<crate::people::PeopleAsk>>>,
        surfaces: crate::device_lists::Surfaces,
        invitable: Rc<Cell<bool>>,
        person: String,
        chat: Rc<dyn Fn() -> bool>,
    ) -> Self {
        Self {
            handle,
            place_here,
            sync,
            asks,
            surfaces,
            invitable,
            person,
            waved_to: RefCell::new(None),
            chat,
            drafts: RefCell::default(),
            kept: RefCell::default(),
            published: RefCell::new(None),
            seen: RefCell::new(None),
            followed: RefCell::new(String::new()),
            stopped: RefCell::new(None),
            closed: RefCell::new(None),
        }
    }

    /// Whose view the bar says the person stopped following, while that moment lasts; empty when it
    /// has nothing to say.
    fn stopped_now(&self) -> String {
        self.stopped
            .borrow()
            .as_ref()
            .map_or_else(String::new, |(who, at)| {
                (ui::time::now().saturating_duration_since(*at) < STOPPED_SHOWN)
                    .then(|| who.clone())
                    .unwrap_or_default()
            })
    }

    /// What moves the reading, without building it: a pass whose key is unchanged has nothing new
    /// to say.
    fn key(&self) -> Key {
        let own = self.sync.as_ref().map(|sync| sync.own());
        let (waved, arrived, arrived_seen) = self.notices();
        let mine = self.mine();
        (
            self.handle.revision(),
            self.sync.as_ref().map_or(0, |sync| sync.people_revision()),
            // What the live session says of the people themselves: the accounts they keep
            // elsewhere and whom they follow, which the workspace's own presence board does not
            // carry.
            self.sync
                .as_ref()
                .map_or(0, |sync| sync.here().borrow().revision()),
            self.surfaces.person.borrow().clone(),
            own.as_ref()
                .map_or_else(String::new, |own| own.label.clone()),
            own.as_ref()
                .map_or_else(String::new, |own| own.availability.id().to_owned()),
            own.as_ref()
                .and_then(|own| own.status.as_ref())
                .map(|status| status.text.clone())
                .unwrap_or_default(),
            own.and_then(|own| own.following).unwrap_or_default(),
            arrived,
            (waved, arrived_seen),
            (
                self.invitable.get(),
                self.waved_to(),
                self.stopped_now(),
                mine,
            ),
        )
    }

    /// What this person's own card offers and wears, as one value: what the pass compares to know
    /// whether the card changed without building it.
    fn mine(&self) -> Own {
        let (_, identity) = self.face();
        let given = self.given();
        Own {
            actions: self
                .own_actions()
                .into_iter()
                .map(|action| (action.id, action.title, action.icon, action.group))
                .collect(),
            photo_change: self.photo_action(SET_PHOTO),
            photo_remove: self.photo_action(RESET_PHOTO),
            control_hints: self.control_hints(),
            colour_id: settings_store::identity::COLOUR.get().unwrap_or_default(),
            colour_slot: Some(self.worn_colour(&identity) as u32),
            given,
            chat: (self.chat)(),
        }
    }

    /// The person's name and the identity their face is drawn from: what their own card opens on.
    fn face(&self) -> (String, String) {
        let own = self.sync.as_ref().map(|sync| sync.own());
        crate::people::own_face(own.as_ref(), &self.person)
    }

    /// The name the person gave themselves; empty while they gave none, so their card asks for one
    /// rather than holding the words said in place of it.
    fn given(&self) -> String {
        crate::platform::person::given_name(
            ui::local_identity::load().map(|identity| identity.display_name),
        )
        .unwrap_or_default()
    }

    /// The colour the person's face is drawn in now: the one they chose, else the one their
    /// identity derives.
    fn worn_colour(&self, identity: &str) -> usize {
        settings_store::identity::COLOUR
            .get()
            .as_deref()
            .and_then(ui::tokens::PresenceHue::from_id)
            .unwrap_or_else(|| ui::tokens::PresenceHue::of_identity(identity))
            .slot()
    }

    /// Whether the desktop's control hints show: the person's own record, which every device of
    /// theirs reads.
    fn control_hints(&self) -> Option<bool> {
        Some(settings_store::control_hints::LEARNED.get().enabled)
    }

    /// What the host's personal action with `id` is called and drawn as, where it offers it.
    fn photo_action(&self, id: &str) -> String {
        self.handle
            .host_actions()
            .into_iter()
            .find(|action| action.id == id)
            .map(|_| id.to_owned())
            .unwrap_or_default()
    }

    /// The host's actions about this person, as their own card shows them: where Settings is,
    /// what the host can do about them, and their other devices. The picture's are offered beside
    /// the avatar instead, so they are named apart ([`People::photo_change`]).
    fn own_actions(&self) -> Vec<PersonAction> {
        let mut actions: Vec<PersonAction> = self
            .handle
            .host_actions()
            .into_iter()
            .filter(tilo_session_core::desktop::state::HostAction::is_personal)
            .filter(|action| action.id != SET_PHOTO && action.id != RESET_PHOTO)
            .map(|action| PersonAction {
                id: action.id,
                title: action.title,
                icon: action
                    .icon
                    .unwrap_or_else(|| ui::icons::Icon::Fingerprint.name().to_owned()),
                group: "identity".to_owned(),
            })
            .collect();
        // Settings itself, at the top of the card: where a person looks for it.
        actions.insert(
            0,
            PersonAction {
                id: OPEN_SETTINGS.to_owned(),
                title: crate::i18n::t("profile-action-settings"),
                icon: ui::icons::Icon::Settings.name().to_owned(),
                group: "settings".to_owned(),
            },
        );
        actions.push(PersonAction {
            id: PAIR_DEVICE.to_owned(),
            title: crate::i18n::t("profile-action-pair-device"),
            icon: ui::icons::Icon::MonitorSmartphone.name().to_owned(),
            group: "devices".to_owned(),
        });
        // Everything that acts as the person, and taking any of it back.
        actions.push(PersonAction {
            id: DEVICES_ACCESS.to_owned(),
            title: crate::i18n::t("profile-action-devices-access"),
            icon: ui::icons::Icon::Shield.name().to_owned(),
            group: "devices".to_owned(),
        });
        actions
    }

    /// The presence colours a person may wear, in the order their card offers them, painted in the
    /// dark presence palette identity colours are defined at.
    fn colours(&self) -> Vec<PresenceColour> {
        let palette = ui::theme::Theme::dark().tokens.colors.presence;
        ui::tokens::PresenceHue::ALL
            .into_iter()
            .map(|hue| {
                let colour = palette.hue(hue).to_rgba8();
                PresenceColour {
                    id: hue.id().to_owned(),
                    colour: format!("{:02x}{:02x}{:02x}", colour.r, colour.g, colour.b),
                }
            })
            .collect()
    }

    /// Everyone here in the workspace in front, as the roster lists them: each member presence
    /// says is here, with what their row says of them.
    fn rows(&self) -> Vec<Person> {
        let mut tree = self.handle.read(Clone::clone);
        (self.place_here)(&mut tree);
        let Some(workspace) = tree.active_workspace() else {
            return Vec::new();
        };
        // Who of the workspace's members is here, by the member presence names: one pass, so a
        // workspace of thousands reads in the time a workspace of three does.
        let present: std::collections::HashMap<&collab_types::MemberId, _> = workspace
            .present
            .iter()
            .map(|here| (&here.member, here))
            .collect();
        // The live session's own reading of the people, where their other devices, their accounts
        // elsewhere and the key a conversation with them is kept on are.
        let live = self.sync.as_ref().map(|sync| sync.here());
        let live = live.as_ref().map(|live| live.borrow());
        let mut raw: Vec<(Person, Option<u64>)> = workspace
            .members
            .iter()
            .filter_map(|member| {
                let present = *present.get(&member.id)?;
                let mut person = person_of(workspace, member, present);
                let agent = person.agent;
                person.agent_client = if agent {
                    ui::presence::agent_client(&member.name).to_owned()
                } else {
                    String::new()
                };
                if let Some(live) = live.as_deref() {
                    let user = live.user_of(&person.key).map(str::to_owned);
                    person.user_key = user.clone().unwrap_or_default();
                    // Chat holds the conversation each pair of people shares, on the record that
                    // person's root key names: someone whose key is not known here (a guest,
                    // before the host's own line arrives) has none to open.
                    person.can_message = !agent && user.is_some() && (self.chat)();
                    if !agent {
                        person.devices = live
                            .devices_of(&person.key)
                            .into_iter()
                            .map(device_of)
                            .collect();
                    }
                    person.linked = user
                        .as_deref()
                        .map(|user| {
                            live.linked_of(user)
                                .iter()
                                .map(|link| PersonLinked {
                                    service: link.service.clone(),
                                    handle: link.handle.clone(),
                                    icon: tilo_app::linked_accounts::service_name_icon(
                                        &link.service,
                                    )
                                    .name()
                                    .to_owned(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                }
                Some((
                    person,
                    member
                        .id
                        .person_id()
                        .and_then(|person| workspace.policy.joined_at(person)),
                ))
            })
            .collect();
        // Someone nobody named is told apart by when they joined: their name and face are every
        // unnamed person's.
        name_the_unnamed(&mut raw);
        let mut rows: Vec<Person> = raw.into_iter().map(|(person, _)| person).collect();
        // The person at this device stands in the roster's own "You" row, never among the others.
        if let Some(own) = self.sync.as_ref().map(|sync| sync.publisher().to_string()) {
            rows.retain(|person| person.key != own);
        }
        // Whom each person here follows, as their presence says: "Following Ada" on the row of
        // someone following Ada, "Following you" on the row of someone following this person.
        if let Some(sync) = &self.sync {
            let workspace_id = workspace.id().to_string();
            let mut pairs: Vec<(String, String)> = sync
                .members(&workspace_id)
                .into_iter()
                .filter_map(|member| Some((member.publisher, member.following?)))
                .collect();
            if let Some(following) = sync.own().following {
                pairs.push((sync.publisher().to_string(), following));
            }
            note_following(&mut rows, &pairs, Some(&sync.publisher().to_string()));
        }
        // People first, software after, each in the order the workspace lists them.
        rows.sort_by_key(|person| person.agent);
        rows
    }

    /// The person at this device, as their own card shows them.
    fn me(&self) -> Person {
        // The card's field holds the name they gave, never the words that stand in for one they
        // have not: an empty field says what to type and leaves nothing to type over.
        let given = self.given();
        let (name, identity) = self.face();
        let colour_id = settings_store::identity::COLOUR.get().unwrap_or_default();
        let Some(sync) = &self.sync else {
            let (red, green, blue) = crate::people::own_rgb(&identity);
            return Person {
                key: SELF.to_owned(),
                name,
                colour: colour_of((red, green, blue)),
                me: true,
                availability: "available".to_owned(),
                given,
                colour_slot: Some(self.worn_colour(&identity) as u32),
                colour_id,
                ..Person::default()
            };
        };
        let own = sync.own();
        let (red, green, blue) = crate::people::own_live_rgb(Some(sync), &identity);
        Person {
            key: SELF.to_owned(),
            name,
            colour: colour_of((red, green, blue)),
            me: true,
            availability: own.availability.id().to_owned(),
            doing: own
                .status
                .as_ref()
                .map(|status| status.text.trim().to_owned())
                .filter(|line| !line.is_empty())
                .unwrap_or_default(),
            automatic: own.status.as_ref().is_some_and(|status| status.automatic),
            call: own
                .call
                .as_ref()
                .map_or_else(String::new, |call| call.call.name.clone()),
            given,
            colour_slot: Some(self.worn_colour(&identity) as u32),
            colour_id,
            ..Person::default()
        }
    }

    /// What the bar still says of a wave or an arrival, as the notices read now.
    fn notices(&self) -> (String, Vec<String>, String) {
        let Some(sync) = &self.sync else {
            return (String::new(), Vec::new(), String::new());
        };
        // The waves heard since the last look, which this shell says itself: the same queue the
        // desktop's own bar reads.
        sync.show_waves();
        let now = ui::time::now();
        let waved = sync
            .wave_notice()
            .filter(|notice| now.saturating_duration_since(notice.at) < WAVE_SHOWN);
        let arrived = sync
            .arrival_notice()
            .filter(|notice| now.saturating_duration_since(notice.at) < ARRIVAL_SHOWN);
        (
            waved.map_or_else(String::new, |notice| notice.from),
            arrived
                .as_ref()
                .map(|notice| notice.names.clone())
                .unwrap_or_default(),
            arrived.and_then(|notice| notice.seen).unwrap_or_default(),
        )
    }

    /// Whom the wave this person sent just now went to, while their row still says it went.
    fn waved_to(&self) -> String {
        self.waved_to
            .borrow()
            .as_ref()
            .filter(|(_, at)| ui::time::now().saturating_duration_since(*at) < WAVE_SENT_SHOWN)
            .map(|(key, _)| key.clone())
            .unwrap_or_default()
    }

    /// How long until a wave or an arrival stops showing, so the pass that clears it wakes then:
    /// whichever still has time to run, ending soonest.
    fn noticed_in(&self) -> Option<std::time::Duration> {
        let now = ui::time::now();
        let remaining = |shown: ui::time::Duration, at: ui::time::Instant| {
            (now.saturating_duration_since(at) < shown)
                .then(|| shown.saturating_sub(now.saturating_duration_since(at)))
        };
        let wave = self
            .sync
            .as_ref()
            .and_then(|sync| sync.wave_notice())
            .and_then(|notice| remaining(WAVE_SHOWN, notice.at));
        let arrived = self
            .sync
            .as_ref()
            .and_then(|sync| sync.arrival_notice())
            .and_then(|notice| remaining(ARRIVAL_SHOWN, notice.at));
        let sent = self
            .waved_to
            .borrow()
            .as_ref()
            .and_then(|(_, at)| remaining(WAVE_SENT_SHOWN, *at));
        let stopped = self
            .stopped
            .borrow()
            .as_ref()
            .and_then(|(_, at)| remaining(STOPPED_SHOWN, *at));
        [wave, arrived, sent, stopped].into_iter().flatten().min()
    }

    /// The reading as it stands.
    fn read(&self) -> People {
        let (waved, arrived, arrived_seen) = self.notices();
        let rows = self.rows();
        let following = self
            .sync
            .as_ref()
            .and_then(|sync| sync.own().following)
            .unwrap_or_default();
        // Following that ends with the followed person still here ended by the person's own input
        // — a press on Stop, or their card's Stop following — and the bar says so a moment. One
        // that ends because they left says nothing: they are gone from the roster.
        let was = self.followed.replace(following.clone());
        let now = ui::time::now();
        let mut stopped = self.stopped.borrow_mut();
        if !following.is_empty() {
            *stopped = None;
        } else if !was.is_empty() && rows.iter().any(|row| row.key == was) {
            *stopped = Some((was, now));
        }
        if stopped
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= STOPPED_SHOWN)
        {
            *stopped = None;
        }
        let stopped = stopped
            .as_ref()
            .map(|(who, _)| who.clone())
            .unwrap_or_default();
        People {
            rows,
            me: self.me(),
            colours: self.colours(),
            actions: self.own_actions(),
            photo_change: self.photo_action(SET_PHOTO),
            photo_remove: self.photo_action(RESET_PHOTO),
            control_hints: self.control_hints(),
            showing: self.surfaces.person.borrow().clone(),
            following,
            stopped,
            waved,
            waved_to: self.waved_to(),
            arrived,
            arrived_seen,
            may_invite: self.invitable.get(),
        }
    }

    /// Carry out what a card or the bar asked of the people.
    fn act(&self, effect: &PeopleEffect) -> Result<Started, ApiError> {
        match effect {
            PeopleEffect::Show => {
                self.keep_typed(true);
                if self.surfaces.overlays.here()
                    == tilo_session_core::desktop::overlays::Overlay::People
                {
                    self.surfaces.close("people");
                } else {
                    // Their press closed it a moment ago: this is that press, not another.
                    let dismissed = self
                        .closed
                        .borrow_mut()
                        .take()
                        .is_some_and(|at| at.elapsed() < DISMISS_GRACE);
                    if !dismissed {
                        self.surfaces.person.borrow_mut().clear();
                        self.surfaces.open("people");
                    }
                }
            }
            PeopleEffect::Close => {
                // Closing the card keeps what was typed and not yet kept, as a field that loses
                // focus keeps it: there is no Save button, so nothing typed is lost.
                self.keep_typed(true);
                *self.closed.borrow_mut() = Some(ui::time::now());
                self.surfaces.person.borrow_mut().clear();
                self.surfaces.close("people");
            }
            PeopleEffect::ShowPerson { person } => {
                // Leaving the card keeps what was typed.
                self.keep_typed(true);
                let person = person.trim().to_owned();
                // Opening the card of the person whose view this is is a gesture about the
                // following itself, not the person working: it leaves the view they follow as it
                // is, as the bar's own chip does in the shell.
                if let Some(sync) = &self.sync
                    && !person.is_empty()
                    && sync.own().following.as_deref() == Some(person.as_str())
                {
                    sync.keep_following_through_gesture();
                }
                // An empty key comes back from a card to the roster; anyone else's opens on them.
                *self.surfaces.person.borrow_mut() = person;
                self.surfaces.open("people");
            }
            PeopleEffect::Follow { person } => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Follow {
                        who: person.trim().to_owned(),
                        following: true,
                    });
                self.taken_elsewhere();
            }
            PeopleEffect::Unfollow => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Follow {
                        who: String::new(),
                        following: false,
                    });
                self.taken_elsewhere();
            }
            PeopleEffect::Wave { person } => {
                let who = person.trim().to_owned();
                self.waved_to.replace(Some((who.clone(), ui::time::now())));
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Wave { who });
            }
            PeopleEffect::Message { person } => {
                // Chat holds the conversation each pair of people shares on the record their root
                // keys name: someone whose key is not known here has none to open.
                let key = self.sync.as_ref().and_then(|sync| {
                    let here = sync.here();
                    let here = here.borrow();
                    here.user_of(person.trim()).map(str::to_owned)
                });
                if let Some(key) = key {
                    self.asks
                        .borrow_mut()
                        .push(crate::people::PeopleAsk::Message { key });
                }
                self.taken_elsewhere();
            }
            PeopleEffect::GoTo { person } => {
                self.asks.borrow_mut().push(crate::people::PeopleAsk::GoTo {
                    who: person.trim().to_owned(),
                });
                self.taken_elsewhere();
            }
            PeopleEffect::SetName { name } => {
                // Enter keeps what is in the field: the pause that was coming is answered here
                // rather than kept a second time from the same words.
                self.drafts.borrow_mut().name = None;
                if self.name_says_more(name) {
                    self.kept.borrow_mut().0 = name.trim().to_owned();
                    self.asks
                        .borrow_mut()
                        .push(crate::people::PeopleAsk::Rename(name.clone()));
                }
            }
            PeopleEffect::SetDoing { text } => {
                self.drafts.borrow_mut().doing = None;
                if self.doing_says_more(text) {
                    self.kept.borrow_mut().1 = text.trim().to_owned();
                    self.asks
                        .borrow_mut()
                        .push(crate::people::PeopleAsk::Status(text.clone()));
                }
            }
            PeopleEffect::SetAvailability { availability } => {
                let chosen = collab_types::presence::Availability::from_id(availability.trim())
                    .ok_or_else(|| {
                        ApiError::InvalidArguments(format!(
                            "`{availability}` is not one of available, focusing, away, idle"
                        ))
                    })?;
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Availability(chosen));
            }
            PeopleEffect::PairDevice => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::PairDevice);
                self.taken_elsewhere();
            }
            PeopleEffect::OpenSettings { link } => {
                let link = link
                    .as_deref()
                    .map(str::trim)
                    .filter(|link| !link.is_empty())
                    .unwrap_or("settings://");
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Settings(link.to_owned()));
                self.taken_elsewhere();
            }
            PeopleEffect::Agent {
                person,
                client,
                ask,
            } => {
                use tilo_session_core::desktop::launchpad::AgentAsk;
                let ask = match ask.trim() {
                    "stop" => AgentAsk::Stop,
                    "undo-run" => AgentAsk::UndoSession,
                    "take-over" => AgentAsk::TakeOver,
                    "revoke" => AgentAsk::Revoke,
                    "card" => AgentAsk::Card,
                    other => {
                        return Err(ApiError::InvalidArguments(format!(
                            "`{other}` is not one of stop, undo-run, take-over, revoke, card"
                        )));
                    }
                };
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Agent {
                        who: person.trim().to_owned(),
                        client: client.trim().to_owned(),
                        ask,
                    });
                self.taken_elsewhere();
            }
            PeopleEffect::Colour { slot } => {
                let hue = usize::try_from(*slot)
                    .ok()
                    .filter(|slot| *slot < ui::tokens::PresenceHue::ALL.len())
                    .map(ui::tokens::PresenceHue::from_slot)
                    .ok_or_else(|| {
                        ApiError::InvalidArguments(format!(
                            "`{slot}` is not one of the presence colours the card offers"
                        ))
                    })?;
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Colour(hue.id().to_owned()));
            }
            PeopleEffect::HostAction { id } => {
                let id = id.trim();
                if let Some(link) = own_settings_link(id) {
                    self.asks
                        .borrow_mut()
                        .push(crate::people::PeopleAsk::Settings(link.to_owned()));
                } else if self
                    .handle
                    .host_actions()
                    .iter()
                    .any(|action| action.id == id)
                {
                    // What the host offered the card: the choice it asks for, answered here rather
                    // than on the avatar the ask waits on.
                    self.asks
                        .borrow_mut()
                        .push(crate::people::PeopleAsk::HostAction(id.to_owned()));
                    self.taken_elsewhere();
                }
            }
            PeopleEffect::ControlHints { on } => {
                // The hints' switch is the person's record, which every device reads: the shell
                // folds it in as their settings move.
                let mut learned = settings_store::control_hints::LEARNED.get();
                learned.enabled = *on;
                if let Err(error) = settings_store::control_hints::LEARNED.set(learned) {
                    tracing::warn!(%error, "could not keep the control hints' switch");
                }
            }
            PeopleEffect::Draft { field, text } => {
                let field = field.trim();
                if field != "name" && field != "doing" {
                    return Err(ApiError::InvalidArguments(format!(
                        "`{field}` is not a field of the person's card"
                    )));
                }
                self.note_draft(field, text);
            }
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    /// Hold what the person just typed into their card: the pause it starts is what keeps it, as
    /// Enter and closing the card are.
    fn note_draft(&self, field: &str, text: &str) {
        let typed = Some((text.to_owned(), ui::time::now()));
        match field {
            "name" => self.drafts.borrow_mut().name = typed,
            _ => self.drafts.borrow_mut().doing = typed,
        }
    }

    /// What the person says they are doing in their own words: what this device read off their
    /// steps is not theirs to edit, so it is not what their card's field holds.
    fn doing(&self) -> String {
        self.sync.as_ref().map_or_else(String::new, |sync| {
            let own = sync.own();
            own.status
                .as_ref()
                .filter(|status| !status.automatic)
                .map(|status| status.text.trim().to_owned())
                .unwrap_or_default()
        })
    }

    /// A choice took the person away from who is here — to a conversation, a window, a page, an
    /// agent's card — so the card and the panel leave with them, as the person was taken to what
    /// they chose. What they typed there is kept first.
    fn taken_elsewhere(&self) {
        self.keep_typed(true);
        self.surfaces.person.borrow_mut().clear();
        self.surfaces.close("people");
    }

    /// Keep what the person's card holds, where it says something the card has not already kept:
    /// on Enter and a card closing (`all`), and once typing pauses. A card has no Save button, so
    /// what is typed in it is kept when it is submitted, when the card closes and when typing
    /// pauses. An empty field is no answer, never a way to lose the name they have, and the words
    /// that stand in for a name nobody gave are what the field says, never a name to keep.
    fn keep_typed(&self, all: bool) {
        let now = ui::time::now();
        let ripe = |slot: &Option<(String, ui::time::Instant)>| {
            all || slot
                .as_ref()
                .is_some_and(|(_, typed)| now.saturating_duration_since(*typed) >= TYPING_SETTLE)
        };
        let mut drafts = self.drafts.borrow_mut();
        let name = ripe(&drafts.name).then(|| drafts.name.take()).flatten();
        let doing = ripe(&drafts.doing).then(|| drafts.doing.take()).flatten();
        drop(drafts);
        if let Some((name, _)) = name.filter(|(name, _)| self.name_says_more(name)) {
            let name = name.trim().to_owned();
            self.kept.borrow_mut().0.clone_from(&name);
            self.asks
                .borrow_mut()
                .push(crate::people::PeopleAsk::Rename(name));
        }
        if let Some((doing, _)) = doing.filter(|(doing, _)| self.doing_says_more(doing)) {
            let doing = doing.trim().to_owned();
            self.kept.borrow_mut().1.clone_from(&doing);
            self.asks
                .borrow_mut()
                .push(crate::people::PeopleAsk::Status(doing));
        }
    }

    /// Whether `name` is one to keep: a name given, never the words said in place of one, and
    /// never the one the card is already on — neither the one the reading carries nor the one the
    /// field has just asked to keep.
    fn name_says_more(&self, name: &str) -> bool {
        let name = name.trim();
        !ui::presence::is_unnamed_person(name)
            && name != self.given()
            && name != self.kept.borrow().0
    }

    /// Whether `doing` is what the card is not already saying: neither the words the reading
    /// carries nor the ones the field has just asked to keep.
    fn doing_says_more(&self, doing: &str) -> bool {
        let doing = doing.trim();
        doing != self.doing() && doing != self.kept.borrow().1
    }

    /// How long until a pause in typing keeps what was typed, so the pass that keeps it wakes then:
    /// whichever field's pause ends soonest.
    fn typed_due(&self) -> Option<std::time::Duration> {
        let now = ui::time::now();
        let drafts = self.drafts.borrow();
        let left = |slot: &Option<(String, ui::time::Instant)>| {
            slot.as_ref().map(|(_, typed)| {
                TYPING_SETTLE.saturating_sub(now.saturating_duration_since(*typed))
            })
        };
        [left(&drafts.name), left(&drafts.doing)]
            .into_iter()
            .flatten()
            .min()
    }
}

/// What the row of `member`, here through `here` in `workspace`, says of them.
fn person_of(
    workspace: &tilo_desktop_model::Workspace,
    member: &Member,
    here: &tilo_desktop_model::Present,
) -> Person {
    let agent = !member.id.is_person();
    let bot = member.id.is_bot();
    let (red, green, blue) = collab_types::presence::identity_rgb(member.colour);
    let titled = here
        .tab
        .and_then(|tab| workspace.tabs().find(|candidate| candidate.id == tab))
        .map(crate::app_names::tab_title)
        .filter(|title| !title.trim().is_empty());
    let app = here
        .app
        .as_deref()
        .map(crate::app_names::app_name)
        .filter(|name| !name.trim().is_empty());
    let doing = here
        .status
        .as_ref()
        .map(|status| status.text.trim().to_owned())
        .filter(|line| !line.is_empty());
    // What their device wrote for them is marked as software's, never shown as their own words.
    let automatic = here.status.as_ref().is_some_and(|status| status.automatic) && doing.is_some();
    Person {
        key: here.device.clone(),
        name: member.name.clone(),
        colour: colour_of((red, green, blue)),
        agent,
        bot,
        me: false,
        availability: here.availability.id().to_owned(),
        doing: doing.unwrap_or_default(),
        automatic,
        activity: titled.unwrap_or_default(),
        in_app: app.unwrap_or_default(),
        call: here
            .call
            .as_ref()
            .map_or_else(String::new, |call| call.name.clone()),
        // Filled in once every row is read: following is a relation between rows.
        follows: String::new(),
        joined: String::new(),
        following: here.followed,
        follower: here.follows_you,
        ..Person::default()
    }
}

/// One of a person's devices as their card lists it: what it calls itself (or nothing), whether it
/// is the one the roster is drawn on, and how available its person is there.
fn device_of(device: tilo_session_core::desktop::here::PersonDevice) -> PersonDevice {
    PersonDevice {
        id: device.id,
        label: device.label,
        this: device.this,
        availability: device.availability.id().to_owned(),
    }
}

/// Give each row nobody named the clock they joined by, from the member row the workspace keeps:
/// their name and face are every unnamed person's, so when they joined is the one fact that tells
/// them apart. Two people admitted within one second would read alike otherwise, so those two say
/// the millisecond as well. A named person's row says nothing of it.
fn name_the_unnamed(rows: &mut [(Person, Option<u64>)]) {
    let joined: Vec<(usize, u64)> = rows
        .iter()
        .enumerate()
        .filter(|(_, (row, _))| ui::presence::is_unnamed_person(&row.name))
        .filter_map(|(index, (_, at))| Some((index, (*at)?)))
        .collect();
    let seconds: Vec<String> = joined
        .iter()
        .map(|(_, at)| clock(*at, ui::format::time_of_with_seconds))
        .collect();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for text in &seconds {
        let count = counts.entry(text.as_str()).or_default();
        *count = count.saturating_add(1);
    }
    for ((index, at), text) in joined.iter().zip(&seconds) {
        let shown = if counts.get(text.as_str()).copied().unwrap_or_default() > 1 {
            clock_millis(*at)
        } else {
            text.clone()
        };
        if let Some((row, _)) = rows.get_mut(*index)
            && !shown.trim().is_empty()
        {
            row.joined = shown;
        }
    }
}

/// The local clock `at` (Unix milliseconds) reads by, to the second, through the person's own
/// time format.
fn clock(at: u64, read: fn(&ui::format::UnixSecs) -> String) -> String {
    i64::try_from(at / 1000)
        .map(|seconds| read(&ui::format::UnixSecs(seconds)))
        .unwrap_or_default()
}

/// The local clock `at` reads by, to the millisecond: what tells two people who joined within one
/// second apart.
fn clock_millis(at: u64) -> String {
    i64::try_from(at)
        .map(|millis| ui::format::time_of_with_millis(&ui::format::UnixMillis(millis)))
        .unwrap_or_default()
}

/// Say on each follower's row whom they follow, from `pairs` of `(follower, followed)` presence
/// keys: the followed row's key, or `self` for this person. A pair naming someone the roster does
/// not list says nothing, and a row already annotated keeps the first pair that names it.
fn note_following(rows: &mut [Person], pairs: &[(String, String)], own: Option<&str>) {
    for (follower, followed) in pairs {
        let target = if own == Some(followed.as_str()) {
            SELF.to_owned()
        } else if rows.iter().any(|row| row.key == *followed) {
            followed.clone()
        } else {
            continue;
        };
        let Some(row) = rows.iter_mut().find(|row| row.key == *follower) else {
            continue;
        };
        if row.follows.is_empty() {
            row.follows = target;
        }
    }
}

/// A colour as the reading writes it, `rrggbb`.
fn colour_of((red, green, blue): (u8, u8, u8)) -> String {
    format!("{red:02x}{green:02x}{blue:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::MemberId;
    use collab_types::presence::Availability;
    use tilo_desktop_model::{Present, Status, Workspace, WorkspaceId};

    /// A row says what presence and the member row say of someone: where they are, what they are
    /// doing, how available they are, and who follows whom — never the app's id for its name.
    #[test]
    fn a_row_says_where_someone_is_and_who_follows_whom() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Studio");
        let ada = MemberId::person("ada");
        workspace.member_here(tilo_desktop_model::Member::named(
            ada.clone(),
            "Ada Lovelace",
        ));
        let mut here = Present::of(ada);
        here.device = "device-key".to_owned();
        here.app = Some("so.tilo.notes".to_owned());
        here.status = Some(Status {
            text: "Rewriting the launch note".to_owned(),
            automatic: false,
        });
        here.availability = Availability::Away;
        here.followed = true;
        here.follows_you = true;
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert_eq!(row.key, "device-key");
        assert_eq!(row.name, "Ada Lovelace");
        assert_eq!(row.availability, "away");
        assert_eq!(row.doing, "Rewriting the launch note");
        assert_eq!(row.colour.len(), 6, "a colour the shell can paint");
        assert!(row.following, "this person follows them");
        assert!(row.follower, "they follow this person");
        assert!(!row.agent && !row.me);
        assert_ne!(row.in_app, "so.tilo.notes", "the app's name, never its id");
        let (red, green, blue) = collab_types::presence::identity_rgb(workspace.members[0].colour);
        assert_eq!(
            row.colour,
            format!("{red:02x}{green:02x}{blue:02x}"),
            "the colour the member row wears, as every surface draws them"
        );
    }

    /// An agent's row says the window it works in, not what a person's does.
    #[test]
    fn an_agents_row_says_the_window_it_works_in() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(2), "Studio");
        let fieldwork = MemberId::agent("fieldwork", "ada");
        workspace.member_here(tilo_desktop_model::Member::named(
            fieldwork.clone(),
            "Fieldwork",
        ));
        let mut here = Present::of(fieldwork);
        here.device = "agent-key".to_owned();
        here.app = Some("so.tilo.tasks".to_owned());
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(row.agent);
        assert!(
            !row.bot,
            "an agent is a run, never one of the workspace's bots"
        );
        assert_ne!(row.in_app, "so.tilo.tasks", "the app's name, never its id");
    }

    /// A bot is software with an installation of its own: its row says so, where an agent's does
    /// not, and neither is ever read as a person.
    #[test]
    fn a_bot_s_row_is_tagged_as_a_bot_and_an_agents_is_not() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(3), "Studio");
        let bot = MemberId::bot("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6");
        workspace.member_here(tilo_desktop_model::Member::of_software(
            bot.clone(),
            "Standup",
        ));
        let mut here = Present::of(bot);
        here.device = "bot-key".to_owned();
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(row.bot && row.agent, "a bot is software, tagged Bot");
        let (red, green, blue) = collab_types::presence::identity_rgb(workspace.members[0].colour);
        assert_eq!(
            row.colour,
            format!("{red:02x}{green:02x}{blue:02x}"),
            "software wears the colour its member row stores, which the bar and History share"
        );

        let mut workspace = Workspace::new(WorkspaceId::from_u128(4), "Studio");
        let ada = MemberId::person("ada");
        workspace.member_here(tilo_desktop_model::Member::named(ada.clone(), "Ada"));
        workspace.place_presence(Present::of(ada));
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(!row.bot && !row.agent);
    }

    /// An agent's row names the window it works in, by the title the shell shows, and the app it
    /// runs; a person's row names the window they look at the same way.
    #[test]
    fn an_agents_row_names_the_window_it_works_in() {
        use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
        let notes = Tab {
            id: 7,
            title: "Launch notes".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut main = Strip::new(StripId(1), "Main");
        main.window_groups.push(WindowGroup {
            id: 8,
            active_tab: notes.id,
            tabs: vec![notes],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut workspace = Workspace::new(WorkspaceId::from_u128(5), "Design");
        workspace.strips = vec![main];
        let claude = MemberId::agent("claude", "ada");
        workspace.member_here(tilo_desktop_model::Member::named(
            claude.clone(),
            "Claude Code",
        ));
        workspace.place_presence(Present {
            tab: Some(7),
            app: Some("so.tilo.notes".to_owned()),
            ..Present::of(claude)
        });
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert_eq!(
            row.activity, "Launch notes",
            "the window, named as every surface names it"
        );
        assert_eq!(row.in_app, "Notes", "and the app it runs, by its name");
    }

    /// A line their device read off their steps is marked as software's; one they wrote is not.
    #[test]
    fn a_line_software_wrote_says_so() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(6), "Studio");
        let ada = MemberId::person("ada");
        workspace.member_here(tilo_desktop_model::Member::named(ada.clone(), "Ada"));
        let mut here = Present::of(ada);
        here.device = "device-key".to_owned();
        here.status = Some(Status {
            text: "Editing the release plan".to_owned(),
            automatic: true,
        });
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(row.automatic, "their device wrote the line for them");
        assert_eq!(row.doing, "Editing the release plan");
        workspace.present[0].status = Some(Status {
            text: "Editing the release plan".to_owned(),
            automatic: false,
        });
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(!row.automatic, "what they wrote is their own");
    }

    /// A follower's row names whom they follow, for any pair: someone following this person says
    /// `self`, the followed row is not annotated, and a pair naming someone the roster does not
    /// list says nothing.
    #[test]
    fn a_followers_row_says_whom_they_follow() {
        let mut rows = vec![
            person("ada", "Ada", false),
            person("bob", "Bob", false),
            person("cy", "Cy", false),
        ];
        let pairs = [
            ("ada".to_owned(), "bob".to_owned()),
            ("cy".to_owned(), "me".to_owned()),
            ("bob".to_owned(), "gone".to_owned()),
        ];
        note_following(&mut rows, &pairs, Some("me"));
        assert_eq!(rows[0].follows, "bob", "ada follows bob");
        assert_eq!(rows[1].follows, "", "bob is followed, not following");
        assert_eq!(rows[2].follows, SELF, "cy follows this person");
    }

    /// Someone nobody named is told apart by when they joined, never a bare "Someone": two admitted
    /// within one second say the millisecond as well, and a named person's row says none of it.
    #[test]
    fn a_row_nobody_named_says_when_they_joined() {
        ui::i18n::I18nManager::init("en-US");
        let unnamed = ui::presence::unnamed_person_label();
        let mut rows = vec![
            (
                person("early", &unnamed, false),
                Some(1_765_000_000_000_u64),
            ),
            (person("late", &unnamed, false), Some(1_765_000_000_402)),
        ];
        name_the_unnamed(&mut rows);
        let told: Vec<&String> = rows.iter().map(|(row, _)| &row.joined).collect();
        assert_ne!(told[0], told[1], "the two rows read apart: {told:?}");
        assert!(
            told[0].contains(':') && told[1].contains(':'),
            "the clock carries seconds: {told:?}"
        );
        assert!(
            told[0].contains('.') && told[1].contains('.'),
            "and the millisecond where two came within one second: {told:?}"
        );

        let mut rows = vec![
            (
                person("first", &unnamed, false),
                Some(1_765_000_000_000_u64),
            ),
            (person("later", &unnamed, false), Some(1_765_000_007_000)),
        ];
        name_the_unnamed(&mut rows);
        assert!(
            !rows[0].0.joined.contains('.') && !rows[1].0.joined.contains('.'),
            "seconds tell them apart, so no millisecond is needed"
        );
        assert_ne!(rows[0].0.joined, rows[1].0.joined);

        let mut rows = vec![(person("ada", "Ada", false), Some(1_765_000_000_000_u64))];
        name_the_unnamed(&mut rows);
        assert_eq!(
            rows[0].0.joined, "",
            "a named person says when they joined by name"
        );
    }

    /// A row the reading builds for a test: a person or software, named or not.
    fn person(key: &str, name: &str, agent: bool) -> Person {
        Person {
            key: key.to_owned(),
            name: name.to_owned(),
            agent,
            ..Person::default()
        }
    }
}

impl Service for PeopleDevice {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![offer::<PeopleEffect>(tilo_resource::typed_resource::<
            PeopleRoot,
        >(
            RESOURCE,
            crate::i18n::t("session-people"),
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.read()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        }
        let effect = decode::<PeopleEffect>(&self.resources(), resource, action, arguments)?;
        self.act(&effect)
    }

    /// Whether the roster, the person here or what the bar says about a wave or an arrival moved.
    fn poll(&mut self) -> bool {
        // Whatever closed the card — its own button, the bar's people button, Escape, another
        // surface coming up — keeps what was typed, as a field that loses focus does. While it
        // stands, only the pause keeps it. Done before the key is read, so the pass that keeps it
        // carries it out and draws what came of it.
        let card_open = !self.surfaces.person.borrow().is_empty()
            && self.surfaces.overlays.here()
                == tilo_session_core::desktop::overlays::Overlay::People;
        self.keep_typed(!card_open);
        let key = self.key();
        if self.seen.borrow().as_ref() == Some(&key) {
            return false;
        }
        let now = self.read();
        *self.seen.borrow_mut() = Some(key);
        if self.published.borrow().as_ref() == Some(&now) {
            return false;
        }
        *self.published.borrow_mut() = Some(now);
        true
    }

    /// When a wave or an arrival stops showing, or a pause in typing keeps what was typed: the
    /// pass that does either has to wake then.
    fn wakes_in(&self) -> Option<std::time::Duration> {
        match (self.noticed_in(), self.typed_due()) {
            (Some(notice), Some(typed)) => Some(notice.min(typed)),
            (notice, typed) => notice.or(typed),
        }
        .map(|left| left.max(std::time::Duration::from_millis(16)))
    }
}
