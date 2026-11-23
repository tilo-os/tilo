//! Who is here in a workspace and what they are doing.
//!
//! Who is here is the workspace's live layer alone: the members present now (people and agents),
//! each device's certified person, the record they focus, and the one line each says of what they
//! are doing — a person's own words or what their device read off their steps, an agent's own
//! activity. Every device, a page or a native desktop, publishes its own line there and reads
//! everyone else's from there. None of it is written to the workspace's desktop document: the bar,
//! the People popover and the windows read it as ambient presence.
//!
//! A person may be here from several devices at once (each browser tab is a device of its own).
//! They are one person here: each device's certificate names its person, devices of one person
//! show once, and this page's person's other devices are this page's person, never someone else.
use std::collections::{HashMap, HashSet};
use tilo_sort::CompactSort as _;

use collab_types::presence::{PresenceHue, Shown};
use collab_types::{Member, MemberId, PersonId};
use tilo_desktop_model::{Layout, Present, Status, TabId, TileContent, Workspace};
use ui::presence::{PresenceCtx, PresenceKey};

use crate::presence::Place;

mod agent_work;
mod following;
mod screen;
mod waves;

pub use agent_work::{WorkPlace, apply_narrated, go_to, note_narrated, reveal_own_agent_work};
pub use collab_types::presence::{
    ShownFocus as Focus, ShownLayout as MemberLayout, ShownWindow as MemberWindow,
};
pub use screen::{Screen, ScreenWindow};
pub use waves::{WAVE_AGAIN_MS, Wave, WaveEar, WaveFrom, WaveOut};

/// The most bytes of a person's own words, as their live presence carries them (a member's
/// activity line).
const MAX_OWN_WORDS_BYTES: usize = 96;

/// One member of the workspace's live layer, as the page reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct LiveMember {
    pub publisher: String,
    /// `"person"` or `"agent"` .
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    /// What the device calls itself, from its certificate: what tells two devices of one person
    /// apart. Empty on a device that named none.
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub focus: Option<Focus>,
    /// What they are doing: an agent's own activity line, or a person's status.
    #[serde(default)]
    pub activity: Option<String>,
    /// Their device read `activity` off the steps they took, rather than anyone saying it.
    #[serde(default)]
    pub automatic: bool,
    /// Where within the workspace's desktop a person is: the id of the strip they are on, as their
    /// presence names it.
    #[serde(default)]
    pub place: Option<String>,
    /// The person (user root, hex) the device is certified as, once its certificate was heard.
    #[serde(default)]
    pub user: Option<String>,
    /// The workspace member this presence names, where it names one.
    #[serde(default)]
    pub member: Option<MemberId>,
    /// The member's latest wave at someone here.
    #[serde(default)]
    pub wave: Option<Wave>,
    /// Where a person's windows are on their screen.
    #[serde(default)]
    pub layout: Option<MemberLayout>,
    /// Whose view the member follows: that member's publisher.
    #[serde(default)]
    pub following: Option<String>,
    /// How available they said they were, in the presence wire's own word
    /// ([`ui::Availability::id`]). `None` when they said nothing of it,
    /// which pages reporting members leave out.
    #[serde(default)]
    pub availability: Option<String>,
    /// The voice channel they are in, as their presence carries it ([`ui::CallPresence::encode`]).
    /// `None` when they are in none.
    #[serde(default)]
    pub call: Option<String>,
    /// The colour they wear, `#rrggbb`, as their shell publishes it: the one
    /// they chose in their own card, or the one their identity derives.
    /// Empty for a member that said none, who wears the colour who they are
    /// gives them ([`HereState::colors`]).
    #[serde(default)]
    pub colour: String,
}

impl LiveMember {
    /// `shown` as the desktop's roster reads them: the same record a page reads from its own
    /// board.
    #[must_use]
    pub fn of(shown: &Shown) -> Self {
        Self {
            publisher: shown.publisher.clone(),
            kind: if shown.agent { "agent" } else { "person" }.into(),
            label: shown.label.clone(),
            device: shown.device.clone(),
            focus: shown.focus.clone(),
            activity: shown.activity.clone(),
            automatic: shown.automatic,
            place: shown.place.clone(),
            user: shown.user.clone(),
            member: shown.member.clone(),
            wave: shown.wave.clone(),
            layout: shown.layout.clone(),
            following: shown.following.clone(),
            // An agent has no availability of its own: it is always at work.
            availability: (!shown.agent).then(|| shown.availability.clone()),
            call: shown.call.clone(),
            colour: shown.colour.clone(),
        }
    }
}

/// A line of text and who wrote it: the person, a model, or the host reading it plainly off what
/// they did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub source: crate::desktop::people::LineSource,
}

impl Line {
    /// Whether software wrote it rather than the person.
    #[must_use]
    pub fn automatic(&self) -> bool {
        self.source != crate::desktop::people::LineSource::Written
    }
}

/// One device a person is here on, as their face lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonDevice {
    /// The device's live publisher, hex: what a copy affordance keeps, never the device's name.
    pub id: String,
    /// What the device calls itself, from its certificate; empty on a device that named itself
    /// none, which a card names by which one it is.
    pub label: String,
    /// This is the device the shell is drawn on.
    pub this: bool,
    /// How available that device said it was.
    pub availability: ui::Availability,
}

/// Someone present, as the desktop shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    /// Their live publisher (the device key for a person), hex.
    pub id: String,
    /// The person (user root, hex) their device's certificate names: the id that stays theirs on
    /// every device.
    pub user: Option<String>,
    /// The agent's workspace key, as its presence names it: what tells two sessions of one agent, or a
    /// client of two people, apart in the member list.
    pub member: Option<MemberId>,
    /// What everyone here calls them: the person's name, whichever of their devices is the one
    /// listed.
    pub label: String,
    /// The devices they are here on, each as it calls itself.
    pub devices: Vec<PersonDevice>,
    pub rgb: (u8, u8, u8),
    pub agent: bool,
    /// The window of the record they focus, when this desktop shows it.
    pub tab: Option<TabId>,
    /// The app of the record they focus, as their presence names it: where following them goes
    /// while no window here shows that record.
    pub focus_app: Option<String>,
    /// Where they are, saying only what their status line does not:
    /// "Strip · App", the strip only where the workspace has more than one,
    /// the app only where the status does not already name it
    /// (`worded_place`). Where no window here shows their record, as in
    /// a workspace whose layout is personal: "In App", from the app their
    /// presence focuses (`focused_app`).
    pub place: Option<String>,
    pub status: Option<Line>,
    /// How available they said they were, from their presence: the face dims while they are away or
    /// idle.
    pub availability: ui::Availability,
    /// The voice channel they are in, as their presence says.
    pub call: Option<ui::CallPresence>,
}

impl Person {
    /// The member this is: an agent by its workspace key, a person by the root their device's
    /// certificate names. `None` for a device whose person is not known yet.
    #[must_use]
    pub fn member_id(&self) -> Option<MemberId> {
        if self.agent {
            return Some(
                self.member
                    .clone()
                    .filter(|member| !member.is_person())
                    .unwrap_or_else(|| MemberId::agent(&self.id)),
            );
        }
        self.user
            .as_deref()
            .filter(|user| !user.is_empty())
            .map(MemberId::person)
    }

    /// The app following them brings forward where no window here shows their record: the app their
    /// presence focuses, as their screen's front window says it.
    #[must_use]
    pub fn followed_app(&self) -> Option<&str> {
        self.focus_app.as_deref()
    }

    /// Where this person is, as following reads them.
    #[must_use]
    pub fn place(&self) -> Place<'_> {
        Place {
            id: &self.id,
            tab: self.tab,
            app: self.followed_app(),
        }
    }
}

/// The member following `id` follows, as among `people`: `id` is the person's own live publisher or
/// one of their devices', and following any of them follows the person.
#[must_use]
pub fn followed_member(people: &[Person], id: &str) -> Option<MemberId> {
    people
        .iter()
        .find(|person| person.id == id || person.devices.iter().any(|device| device.id == id))
        .and_then(Person::member_id)
}

/// What the page told the desktop about the workspace.
#[derive(Debug, Default)]
pub struct HereState {
    /// The live layer's members, or `None` while it is not live.
    members: Option<Vec<LiveMember>>,
    /// Why this page cannot join the live layer at all, in the words it shows in the members'
    /// place: a live session that could not be reached.
    unavailable: Option<String>,
    /// This page itself: its publisher, name and colour.
    own: Option<LiveMember>,
    /// What this device read off its person's own recent steps: the line it publishes for them
    /// while they share an automatic status and said nothing themselves.
    own_reading: Option<String>,
    /// Bumped on every change, for the bar's snapshot revision.
    revision: u64,
    /// The members last registered on window presence, to forget who left.
    registered: Vec<MemberId>,
    /// What this page's person lets their device say about them: only
    /// [`ui::StatusSharing::Automatic`] publishes what it read off their steps.
    sharing: ui::StatusSharing,
    /// What this page's person says they are doing, in their own words: it travels on their live
    /// presence and leads what everyone here reads of them, before anything their device reads off
    /// their steps.
    own_words: Option<String>,
    /// The waves this page sent and heard.
    waves: waves::Waves,
    /// The last wave at this person, as the bar says it.
    notice: Option<crate::desktop::host::chrome::WaveNotice>,
    /// Whose view this page follows: their publisher.
    following: Option<String>,
    /// Each person's accounts elsewhere as their profile names them, by their root key (lower-case
    /// hex).
    linked: HashMap<String, Vec<tilo_app::linked_accounts::ProfileLink>>,
    /// The name each person's profile keeps, by their root key (lower-case hex), for someone the
    /// page asked about who is not here to say it ([`HereState::set_profile_name`]); an empty one
    /// says their profile names none.
    profile_names: HashMap<String, String>,
    /// This person's own agents already met on this page, by publisher
    /// ([`HereState::take_own_agents_arrived`]).
    met_agents: std::collections::BTreeSet<String>,
    /// Where this person's own agents were last shown working ([`HereState::take_own_agent_work`]).
    agent_work: agent_work::AgentWork,
}

pub type Here = std::rc::Rc<std::cell::RefCell<HereState>>;

fn line(text: &str, source: crate::desktop::people::LineSource) -> Option<Line> {
    let text = text.trim();
    (!text.is_empty()).then(|| Line {
        text: text.to_owned(),
        source,
    })
}

/// How available a member said they were: their presence's word, and the default — available — when
/// they said nothing of it.
fn availability_of(member: &LiveMember) -> ui::Availability {
    member
        .availability
        .as_deref()
        .and_then(ui::Availability::from_id)
        .unwrap_or(ui::Availability::Available)
}

/// The name a member goes by here, as their presence says it.
fn member_name(member: &LiveMember) -> Option<&str> {
    ui::presence::shown_name(&member.label)
}

/// The row already listed for the agent at `device`: the same publisher, or the same person's
/// agent going by the same name (one agent heard through two publishers). One member is one row,
/// however many sessions of it work here.
fn same_agent<'a>(
    people: &'a mut [Person],
    device: &str,
    user: Option<&str>,
    label: &str,
) -> Option<&'a mut Person> {
    let label = label.trim();
    people.iter_mut().find(|row| {
        row.agent
            && (row.id == device
                || (!label.is_empty()
                    && row.label == label
                    && user.is_some()
                    && row.user.as_deref() == user))
    })
}

/// What `member` says they are doing, as their live presence carries it: their own words (an
/// agent's activity is its own), or what their device read off their steps.
fn said_line(member: &LiveMember) -> Option<Line> {
    let source = if member.automatic {
        crate::desktop::people::LineSource::Observed
    } else {
        crate::desktop::people::LineSource::Written
    };
    member
        .activity
        .as_deref()
        .and_then(|text| line(text, source))
}

impl HereState {
    pub fn set_members(
        &mut self,
        own: Option<LiveMember>,
        members: Option<Vec<LiveMember>>,
    ) -> bool {
        if self.own == own && self.members == members {
            return false;
        }
        self.own = own;
        self.members = members;
        if let Some(own) = &self.own
            && let Some(user) = own.user.as_deref()
        {
            ui::people_photos::alias(&own.publisher, user);
        }
        // Each device wears its person's picture.
        for member in self.members.iter().flatten() {
            if let Some(user) = member.user.as_deref().filter(|user| !user.is_empty()) {
                ui::people_photos::alias(&member.publisher, user);
            }
        }
        let members = self.members.clone().unwrap_or_default();
        self.hear_waves(&members);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// What the page says it cannot do where it would show who is here, or `None` while it can (or
    /// did). Returns whether it changed.
    pub fn set_unavailable(&mut self, unavailable: Option<String>) -> bool {
        if self.unavailable == unavailable {
            return false;
        }
        self.unavailable = unavailable;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Why the page joins no live layer at all, shown in the members' place;
    /// `None` while it does.
    #[must_use]
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Someone's picture changed: what shows them is drawn again.
    pub fn photos_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The accounts elsewhere `user` 's profile names now. Whether that changed anything.
    pub fn set_linked(
        &mut self,
        user: &str,
        linked: Vec<tilo_app::linked_accounts::ProfileLink>,
    ) -> bool {
        let user = user.to_ascii_lowercase();
        if self.linked.get(&user).map_or(&[][..], Vec::as_slice) == linked.as_slice() {
            return false;
        }
        if linked.is_empty() {
            self.linked.remove(&user);
        } else {
            self.linked.insert(user, linked);
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// The name `user`'s profile keeps, as the page fetched it: empty when it keeps none. Whether
    /// that changed anything.
    pub fn set_profile_name(&mut self, user: &str, name: &str) -> bool {
        let user = user.to_ascii_lowercase();
        let name = name.trim();
        if self.profile_names.get(&user).map(String::as_str) == Some(name) {
            return false;
        }
        self.profile_names.insert(user, name.to_owned());
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// The name `user`'s profile keeps, once the page asked: `Some("")` when it keeps none,
    /// `None` when nobody asked yet.
    #[must_use]
    pub fn profile_name(&self, user: &str) -> Option<&str> {
        self.profile_names
            .get(&user.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The accounts elsewhere `user` 's profile names.
    #[must_use]
    pub fn linked_of(&self, user: &str) -> &[tilo_app::linked_accounts::ProfileLink] {
        self.linked
            .get(&user.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    /// This person's own agents (devices of theirs that are agents, such as the one their setup
    /// prompt connected) present now and not met on this page before, each once, as `(publisher,
    /// label)`: the page follows one and opens its card, so the person watches it work from the
    /// moment it arrives. Someone else's agent is not theirs to be shown into.
    pub fn take_own_agents_arrived(&mut self) -> Vec<(String, String)> {
        let Some(own) = self.own_user().map(str::to_owned) else {
            return Vec::new();
        };
        let arrived: Vec<(String, String)> = self
            .members
            .iter()
            .flatten()
            .filter(|member| member.kind == "agent")
            .filter(|member| !self.met_agents.contains(&member.publisher))
            .filter(|member| {
                self.user_of(&member.publisher)
                    .is_some_and(|user| user.eq_ignore_ascii_case(&own))
            })
            .map(|member| (member.publisher.clone(), member.label.clone()))
            .collect();
        self.met_agents
            .extend(arrived.iter().map(|(publisher, _)| publisher.clone()));
        arrived
    }

    pub fn sharing(&self) -> ui::StatusSharing {
        self.sharing
    }

    pub fn set_sharing(&mut self, sharing: ui::StatusSharing) {
        if self.sharing != sharing {
            self.sharing = sharing;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Say what this person is doing in their own words; `None` (or only spaces) clears it. On one
    /// line, at most [`ui::MAX_STATUS_CHARS`] characters and as many bytes as a live member's
    /// activity carries.
    pub fn set_own_words(&mut self, words: Option<String>) {
        let words = words
            .map(|words| {
                let mut kept = String::new();
                for (index, c) in words
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .enumerate()
                {
                    if index == ui::MAX_STATUS_CHARS
                        || kept.len().saturating_add(c.len_utf8()) > MAX_OWN_WORDS_BYTES
                    {
                        break;
                    }
                    kept.push(c);
                }
                kept.trim_end().to_owned()
            })
            .filter(|words| !words.is_empty());
        if self.own_words != words {
            self.own_words = words;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// What this person said they are doing, whether or not they share it.
    pub fn own_words(&self) -> Option<&str> {
        self.own_words.as_deref()
    }

    /// The words this page publishes as what its person is doing: their own, unless they share
    /// nothing of what they do.
    pub fn shared_words(&self) -> Option<&str> {
        self.own_words()
            .filter(|_| self.sharing != ui::StatusSharing::Off)
    }

    /// What this device read off its person's own recent steps, `None` while they took none lately.
    /// Whether that changed anything.
    pub fn set_own_reading(&mut self, reading: Option<String>) -> bool {
        let reading = reading
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        if self.own_reading == reading {
            return false;
        }
        self.own_reading = reading;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// What everyone here reads of this person, as this device publishes it: their own words, else
    /// what it read off their steps while they share an automatic status.
    pub fn own_doing(&self) -> Option<Line> {
        self.shared_words()
            .and_then(|words| line(words, crate::desktop::people::LineSource::Written))
            .or_else(|| {
                self.own_reading
                    .as_deref()
                    .filter(|_| self.sharing == ui::StatusSharing::Automatic)
                    .and_then(|text| line(text, crate::desktop::people::LineSource::Observed))
            })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// This page's own publisher, which names this person on its roster.
    #[must_use]
    pub fn own_id(&self) -> Option<&str> {
        self.own.as_ref().map(|own| own.publisher.as_str())
    }

    /// This page's person (user root, hex), when the page named it.
    pub fn own_user(&self) -> Option<&str> {
        self.own
            .as_ref()
            .and_then(|own| own.user.as_deref())
            .filter(|user| !user.is_empty())
    }

    /// The person the device `publisher` is a device of, as its certificate says; `None` before it
    /// was heard, and for a device whose presence names no person's root key.
    pub fn user_of(&self, publisher: &str) -> Option<&str> {
        if Some(publisher) == self.own_id()
            && let Some(own) = self.own_user()
        {
            return Some(own);
        }
        self.members
            .iter()
            .flatten()
            .find(|member| member.publisher == publisher)
            .and_then(|member| member.user.as_deref())
            .filter(|user| !user.is_empty() && PersonId::parse(user).is_ok())
    }

    /// The member the device `publisher` is here as: the agent or bot its presence names, else its
    /// person, once heard.
    #[must_use]
    pub fn member_of(&self, publisher: &str) -> Option<MemberId> {
        let named = self
            .members
            .iter()
            .flatten()
            .chain(self.own.as_ref())
            .find(|member| member.publisher == publisher)
            .and_then(|member| member.member.clone())
            .filter(|member| !member.is_person());
        named.or_else(|| self.user_of(publisher).map(MemberId::person))
    }

    /// Who `publisher` counts as: its person where known, else the device itself, so devices of one
    /// person count once.
    fn person_of<'a>(&'a self, publisher: &'a str) -> &'a str {
        self.user_of(publisher).unwrap_or(publisher)
    }

    /// The devices the person behind `publisher` is here on: which of their machines are here, each
    /// as it calls itself and as it said it was. A device that named none is known by its
    /// publisher, never by its person's name.
    #[must_use]
    pub fn devices_of(&self, publisher: &str) -> Vec<PersonDevice> {
        let person = self.person_of(publisher);
        let here = self.own_id();
        self.members
            .iter()
            .flatten()
            .chain(self.own.as_ref())
            .filter(|member| member.kind != "agent" && self.person_of(&member.publisher) == person)
            .map(|member| PersonDevice {
                id: member.publisher.clone(),
                // A device that named itself none keeps it empty: its key is an id, and never
                // stands in for the name it did not give.
                label: member.device.trim().to_owned(),
                this: here == Some(member.publisher.as_str()),
                availability: availability_of(member),
            })
            .collect()
    }

    /// Everyone here, by the device their presence comes from: the name and colour of the person
    /// they are. What names a device in a place only its publisher reaches, such as a pointer over
    /// a tile.
    #[must_use]
    pub fn person_names(&self) -> HashMap<String, (String, (u8, u8, u8))> {
        let colors = self.colors();
        let mut names = HashMap::new();
        for member in self.members.iter().flatten().chain(self.own.as_ref()) {
            let label = ui::presence::person_label(member_name(member).unwrap_or_default());
            names.insert(
                member.publisher.clone(),
                (label, color_in(&colors, &member.publisher)),
            );
        }
        names
    }

    /// Whether `publisher` is this page or another device of its person.
    fn is_own_person(&self, publisher: &str) -> bool {
        Some(publisher) == self.own_id()
            || self
                .own_user()
                .is_some_and(|own| self.user_of(publisher) == Some(own))
    }

    /// This page's own name, as its presence publishes it.
    pub fn own_label(&self) -> Option<&str> {
        self.own
            .as_ref()
            .map(|own| own.label.trim())
            .filter(|label| !label.is_empty())
    }

    /// The name and colour presence shows for the person `user` (their user root, hex): the name
    /// their devices here go by. `None` for someone not here now.
    pub fn person(&self, user: &str) -> Option<(String, (u8, u8, u8))> {
        let mut devices = self
            .members
            .iter()
            .flatten()
            .chain(self.own.as_ref())
            .filter(|member| {
                member.kind != "agent" && self.user_of(&member.publisher) == Some(user)
            });
        let first = devices.next()?;
        let named = std::iter::once(first)
            .chain(devices)
            .find(|member| member_name(member).is_some())
            .unwrap_or(first);
        let label = ui::presence::person_label(member_name(named).unwrap_or_default());
        let rgb = self
            .colors()
            .get(&named.publisher)
            .copied()
            .unwrap_or_else(|| ui::presence::user_color(user));
        Some((label, rgb))
    }

    /// What each other person's device here is drawn as, by live publisher: their person's name
    /// and the colour everyone here draws them in ([`Self::colors`]).
    /// Their pointer wears it as their face on the bar and their row in People do.
    #[must_use]
    pub fn faces(&self) -> HashMap<String, (String, (u8, u8, u8))> {
        let colors = self.colors();
        self.members
            .iter()
            .flatten()
            .filter(|member| member.kind != "agent")
            .map(|member| {
                let face = self
                    .user_of(&member.publisher)
                    .and_then(|user| self.person(user))
                    .unwrap_or_else(|| {
                        (
                            ui::presence::person_label(member_name(member).unwrap_or_default()),
                            color_in(&colors, &member.publisher),
                        )
                    });
                (member.publisher.clone(), face)
            })
            .collect()
    }

    /// Each person here by the person they are (their user root, hex), this page's own among them,
    /// with the name and colour every surface here shows them by: what the apps here are told of
    /// them, so their caret and marks in a window wear their face on the bar, and a window names
    /// its own person as everyone else's does.
    #[must_use]
    pub fn persons(&self) -> Vec<(String, String, (u8, u8, u8))> {
        let mut persons: Vec<(String, String, (u8, u8, u8))> = Vec::new();
        for member in self.own.iter().chain(self.members.iter().flatten()) {
            let user = self
                .user_of(&member.publisher)
                .filter(|_| member.kind != "agent");
            if let Some(user) = user.filter(|user| persons.iter().all(|(held, _, _)| held != user))
                && let Some((name, rgb)) = self.person(user)
            {
                persons.push((user.to_owned(), name, rgb));
            }
        }
        persons
    }

    /// This page's own colour, as everyone here sees it.
    pub fn own_rgb(&self) -> Option<(u8, u8, u8)> {
        let own = self.own_id()?;
        Some(color_in(&self.colors(), own))
    }

    /// What colour everyone here is drawn in, by live publisher, this page included: the colour
    /// the member claims (a person's choice in their own card, as their shell publishes it), else
    /// the one who they are gives them ([`ui::presence::member_color`]), keyed by the member — a
    /// person by their user root, so their other devices wear their colour, an agent by the
    /// member it is, never by the key its session speaks from. The rule every app paints the
    /// member by, so a face on the bar is the colour of their card, marks and caret in a window.
    ///
    /// A person's device is in the live layer before its certificate pairs it with its person.
    /// Until it does it has no entry here, so it is drawn neutral
    /// ([`ui::presence::UNSETTLED_RGB`]) rather than in a colour keyed by its device that changes
    /// once its person is known.
    pub fn colors(&self) -> HashMap<String, (u8, u8, u8)> {
        self.members
            .iter()
            .flatten()
            .chain(self.own.as_ref())
            .filter_map(|member| {
                let id = if member.kind == "agent" {
                    member
                        .member
                        .clone()
                        .unwrap_or_else(|| MemberId::agent(&member.publisher))
                } else {
                    MemberId::person(self.user_of(&member.publisher)?)
                };
                let rgb = ui::presence::parse_hex_color(&member.colour)
                    .unwrap_or_else(|| ui::presence::member_color(&id));
                Some((member.publisher.clone(), rgb))
            })
            .collect()
    }

    /// Everyone present but this page, people first, then agents, each in the order they arrived.
    /// `tree` and `records` (each window's record) place them in this desktop. `app_name` names an
    /// app by its id, for where someone is.
    pub fn people(
        &self,
        tree: &Layout,
        records: &[(TabId, String, String)],
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        let colors = self.colors();
        let mut people: Vec<Person> = Vec::new();
        // Who each listed person counts as (`person_of`), beside `people` .
        let mut persons: Vec<&str> = Vec::new();
        for member in self.members.iter().flatten() {
            // An agent is known by the session it declared, never by what it is called.
            let agent = member.kind == "agent";
            let person = if agent {
                member.publisher.as_str()
            } else {
                self.person_of(&member.publisher)
            };
            if (!agent && self.is_own_person(&member.publisher)) || persons.contains(&person) {
                continue;
            }
            if agent
                && let Some(row) = same_agent(
                    &mut people,
                    &member.publisher,
                    self.user_of(&member.publisher),
                    &member.label,
                )
            {
                // The same agent heard through a second publisher is one row: the first line it
                // said is kept, the other fills in.
                if row.status.is_none() {
                    row.status = said_line(member);
                }
                continue;
            }
            persons.push(person);
            // An agent names the record it works in; a person's page says which window they are in.
            let tab = member
                .focus
                .as_ref()
                .and_then(|focus| window_of(tree, records, focus));
            let status = said_line(member);
            let place = match tab {
                Some(tab) => worded_place(
                    tree,
                    tab,
                    member.focus.as_ref().map(|focus| focus.app.as_str()),
                    app_name,
                    status.as_ref(),
                ),
                None => focused_app(member, app_name, status.as_ref()),
            };
            people.push(Person {
                id: member.publisher.clone(),
                user: self.user_of(&member.publisher).map(str::to_owned),
                member: member.member.clone(),
                // A person goes by the name they gave, never their device's label; an agent
                // that says no name goes by the program it works in.
                label: ui::presence::person_label(
                    std::iter::once(member.label.as_str())
                        .chain(agent.then_some(member.device.as_str()))
                        .find_map(ui::presence::shown_name)
                        .unwrap_or_default(),
                ),
                devices: self.devices_of(&member.publisher),
                rgb: color_in(&colors, &member.publisher),
                agent,
                tab,
                focus_app: member.focus.as_ref().map(|focus| focus.app.clone()),
                place,
                status,
                availability: availability_of(member),
                call: member.call.as_deref().and_then(ui::CallPresence::decode),
            });
        }
        people.compact_sort_by_key(|person| person.agent);
        people
    }

    /// This page's own name as everyone here is shown it.
    pub fn own_shown(&self) -> Option<String> {
        self.own_label().map(str::to_owned)
    }

    /// Show `people` on the windows they are in: each one's colour, name and kind, registered on
    /// their window's tab. Whoever left, or focuses no window here, is taken off every tab. Show
    /// the waves at this person heard since the last call: each waver's face bounces, and the last
    /// of them is the bar's notice.
    pub fn show_waves(&mut self, presence: &mut PresenceCtx) {
        let now = ui::time::now();
        for wave in self.take_waves() {
            if let Some(person) = self.user_of(&wave.id) {
                presence.bounce(
                    MemberId::person(person),
                    ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
                    now,
                );
            }
            self.notice = Some(crate::desktop::host::chrome::WaveNotice {
                from: wave.name,
                at: now,
            });
        }
    }

    /// The last wave at this person, for the bar to say.
    pub fn wave_notice(&self) -> Option<crate::desktop::host::chrome::WaveNotice> {
        self.notice.clone()
    }

    pub fn register_windows(&mut self, people: &[Person], presence: &mut PresenceCtx) {
        let here: Vec<(MemberId, &Person)> = people
            .iter()
            .filter_map(|person| Some((person.member_id()?, person)))
            .collect();
        for gone in self.registered.iter().filter(|id| {
            !here
                .iter()
                .any(|(member, p)| member == *id && p.tab.is_some())
        }) {
            for list in presence
                .here
                .iter_mut()
                .filter_map(|(key, list)| matches!(key, PresenceKey::Tab { .. }).then_some(list))
            {
                list.retain(|u| u != gone);
            }
        }
        self.registered.clear();
        for (user, person) in here {
            let (r, g, b) = person.rgb;
            presence.set_color_override(user.clone(), &format!("#{r:02x}{g:02x}{b:02x}"));
            presence.set_display_name(user.clone(), &person.label);
            if let Some(tab) = person.tab {
                let key = PresenceKey::Tab {
                    id: format!("{tab:x}"),
                };
                presence.set_tab_focus(&key, user.clone());
                self.registered.push(user);
            }
        }
    }
}

/// `publisher` 's colour in `colors` ([`HereState::colors`]), or the neutral face while the
/// workspace does not know who they are yet.
fn color_in(colors: &HashMap<String, (u8, u8, u8)>, publisher: &str) -> (u8, u8, u8) {
    colors
        .get(publisher)
        .copied()
        .unwrap_or(ui::presence::UNSETTLED_RGB)
}

pub use ui::presence::unnamed_person_label;

/// Name every person here and every device of theirs in `authors`, as
/// presence shows them: what a folder file's version resolves through — the
/// person whose device signed it, and the computer whose session vouched for
/// the writer.
pub fn register_presence(authors: &mut ui::AuthorDirectory, here: &HereState) {
    let mut named = here.faces();
    if let Some(own) = here.own_id().map(str::to_owned) {
        let label = here.own_label().unwrap_or_default().to_owned();
        named.entry(own).or_insert((label, (0, 0, 0)));
    }
    for (publisher, (label, _)) in named {
        if !label.trim().is_empty() {
            authors.register_device(&publisher, label);
        }
        let Some(user) = here.user_of(&publisher).map(str::to_owned) else {
            continue;
        };
        let Some((name, _)) = here.person(&user) else {
            continue;
        };
        authors.register_member(
            &MemberId::person(user.clone()),
            ui::history_source::author_for_identity(name, &user),
        );
    }
}

/// The window showing `focus` : the record's window, else the first window of its app.
fn window_of(tree: &Layout, records: &[(TabId, String, String)], focus: &Focus) -> Option<TabId> {
    let shown: HashSet<TabId> = tree
        .active_workspace()
        .into_iter()
        .flat_map(Workspace::tabs)
        .map(|tab| tab.id)
        .collect();
    records
        .iter()
        .find(|(tab, app, record)| {
            *record == focus.document && *app == focus.app && shown.contains(tab)
        })
        .map(|(tab, _, _)| *tab)
}

/// Where the window `tab` is, in words beside `status` : "Strip · App". The strip is named only
/// where the workspace has more than one (the one there is is no news), and the app only where
/// `status` does not already say it ("Main · Chat" over "Working in Chat" says Chat twice). `None`
/// when that leaves nothing.
fn worded_place(
    tree: &Layout,
    tab: TabId,
    focus_app: Option<&str>,
    app_name: &dyn Fn(&str) -> Option<String>,
    status: Option<&Line>,
) -> Option<String> {
    let workspace = tree.active_workspace()?;
    let several = workspace.strips.len() > 1;
    workspace.strips.iter().find_map(|strip| {
        let tab = strip
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .find(|t| t.id == tab)?;
        // The app the window sessions: from its tile where the shell's own model names it, else from
        // the app its presence focuses (a window drawn by its app's own surface names none here).
        let app = match &tab.tile {
            TileContent::LocalApp { app_id, .. } => app_name(app_id),
            _ => None,
        }
        .or_else(|| focus_app.and_then(app_name))
        .unwrap_or_else(|| tab.title.clone());
        let app = Some(app.trim())
            .filter(|app| !app.is_empty())
            .filter(|app| !status.is_some_and(|status| names(&status.text, app)));
        let strip = Some(strip.name.trim()).filter(|name| several && !name.is_empty());
        match (strip, app) {
            (Some(strip), Some(app)) => Some(format!("{strip} · {app}")),
            (Some(only), None) | (None, Some(only)) => Some(only.to_owned()),
            (None, None) => None,
        }
    })
}

/// Where `member` is when no window here shows it: "In App", the app their presence focuses, unless
/// `status` already names it.
fn focused_app(
    member: &LiveMember,
    app_name: &dyn Fn(&str) -> Option<String>,
    status: Option<&Line>,
) -> Option<String> {
    let app = app_name(&member.focus.as_ref()?.app)?;
    let app = app.trim();
    (!app.is_empty() && !status.is_some_and(|status| names(&status.text, app)))
        .then(|| crate::desktop::people::in_app(app))
}

/// What `person` says they are doing, as the workspace's own model keeps it: their words, trimmed,
/// and whether software said them for them rather than they typing them.
fn doing(person: &Person) -> Option<Status> {
    let line = person.status.as_ref()?;
    let text = line.text.trim();
    (!text.is_empty()).then(|| Status {
        text: text.to_owned(),
        automatic: line.automatic(),
    })
}

/// Whether `text` says `name`: its words, in order, among `text`'s,
/// whatever their case.
fn names(text: &str, name: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (text, name) = (words(text), words(name));
    !name.is_empty()
        && text
            .windows(name.len())
            .any(|session| session == name.as_slice())
}

/// `people` written into `tree` as the bar reads who is here: the active workspace's member rows,
/// each in the colour it wears everywhere, and everyone's presence. `tree` is the copy the bar
/// paints, never the workspace's document.
pub fn place_here(tree: &mut Layout, people: &[Person], following: Option<&str>) {
    let active = tree.active;
    let now = tilo_clock::unix_now();
    let Some(workspace) = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == active)
    else {
        return;
    };
    workspace.members = member_rows(workspace, people, now);
    workspace.present.clear();
    for person in people {
        workspace.place_presence(present_of(person, following));
    }
}

/// Mark the faces in `tree`'s active workspace of everyone following this person (`followers`,
/// by roster id), so the bar shows who watches their view where their faces already stand.
pub fn mark_followers(tree: &mut Layout, followers: &[String]) {
    let active = tree.active;
    let Some(workspace) = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == active)
    else {
        return;
    };
    for present in &mut workspace.present {
        present.follows_you = followers.contains(&present.device);
    }
}

/// The workspace's members, each in the colour it wears everywhere: its people with their roles,
/// the agents admitted in it, its bots and anyone here its policy does not list yet. A person here
/// now wears the colour they show by here; everyone else the one who they are gives them.
fn member_rows(workspace: &Workspace, people: &[Person], now: u64) -> Vec<Member> {
    let here = |id: &MemberId| people.iter().find(|person| member_id_of(person) == *id);
    let mut rows: Vec<Member> = workspace
        .roster()
        .into_iter()
        .filter(|(person, _)| !workspace.is_blocked(person))
        .filter_map(|(person, role)| {
            let person_id = PersonId::parse(&person).ok()?;
            let id = MemberId::person(person_id.clone());
            // Someone not here goes by the name the workspace's governance gives them: the one
            // they gave themselves, wherever they did.
            let (name, colour) = here(&id).map_or_else(
                || {
                    (
                        workspace
                            .people_names
                            .get(&person)
                            .cloned()
                            .unwrap_or_default(),
                        PresenceHue::of_member(&id),
                    )
                },
                |person| (person.label.clone(), hue_of(person.rgb)),
            );
            let mut row = Member::of_person(
                &collab_types::Person::new(person_id, name).wearing(colour),
                Some(role),
            );
            row.joined_at = workspace.policy.joined_at(&person);
            Some(row)
        })
        .collect();
    for (agent, _) in workspace.agent_members_at(now) {
        let id = MemberId::agent(agent.clone());
        let mut row = Member::of_software(id.clone(), agent);
        if let Some(person) = here(&id) {
            row.name.clone_from(&person.label);
            row.colour = hue_of(person.rgb);
        }
        rows.push(row);
    }
    for bot in &workspace.bots {
        let id = MemberId::bot(
            bot.installation
                .clone()
                .unwrap_or_else(|| bot.name.to_lowercase()),
        );
        let mut row = Member::of_software(id, bot.name.clone());
        row.name.clone_from(&bot.name);
        rows.push(row);
    }
    // Anyone here whom the workspace's policy does not list yet is still someone here.
    for person in people {
        let id = member_id_of(person);
        if rows.iter().any(|row| row.id == id) {
            continue;
        }
        rows.push(member_of_presence(person, &id));
    }
    rows
}

/// `person` as a member row, for someone here the workspace's policy does not list.
fn member_of_presence(person: &Person, id: &MemberId) -> Member {
    if let MemberId::Person(person_id) = id {
        Member::of_person(
            &collab_types::Person::new(person_id.clone(), person.label.clone())
                .wearing(hue_of(person.rgb)),
            None,
        )
    } else {
        let mut row = Member::of_software(id.clone(), person.label.clone());
        row.colour = hue_of(person.rgb);
        row
    }
}

/// The member `person` is here as: the person behind their device, or the agent its key names.
fn member_id_of(person: &Person) -> MemberId {
    if person.agent
        && let Some(member) = &person.member
    {
        return member.clone();
    }
    MemberId::person(person.user.clone().unwrap_or_else(|| person.id.clone()))
}

/// `person`'s presence, as this session reads it.
fn present_of(person: &Person, following: Option<&str>) -> Present {
    Present {
        member: member_id_of(person),
        device: person.id.clone(),
        tab: person.tab,
        app: person.focus_app.clone(),
        status: doing(person),
        availability: person.availability,
        call: person.call.clone(),
        session: None,
        followed: following.is_some_and(|followed| {
            followed == person.id || person.devices.iter().any(|device| device.id == followed)
        }),
        follows_you: false,
    }
}

/// `rgb` as the hue the model stores a member's colour in.
fn hue_of((red, green, blue): (u8, u8, u8)) -> PresenceHue {
    PresenceHue::nearest_to_srgb(red, green, blue).unwrap_or_else(|| PresenceHue::from_slot(0))
}

#[cfg(test)]
mod tests;
