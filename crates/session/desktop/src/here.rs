//! Who is here in a hosted room and what they are doing.
//!
//! Two sources meet here. A room's live layer names the members present
//! now (people and agents), their colour, the record they focus and an
//! agent's own activity line. The host answers `/here` with each person's
//! one-line status and a summary of their last minutes, built from the
//! titles of the actions they took (`tilo_relay_protocol::web::HERE_PATH`).
//! Neither is written to the room's desktop document: the bar, the People
//! popover and the windows read them as ambient presence.
//!
//! A person may be here from several devices at once (each browser tab is a
//! device of its own). They are one person here: the host's lines pair each
//! device with its person, devices of one person show once, and this page's
//! person's other devices are this page's person, never someone else.
use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use tilo_desktop_model::{Collaborator, DesktopTree, TabId, TileContent};
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};

mod following;
mod screen;
mod waves;
pub use screen::{MemberLayout, MemberWindow, Screen, ScreenWindow};
pub use waves::{WAVE_AGAIN_MS, Wave, WaveEar, WaveFrom, WaveOut};

/// The most bytes of a person's own words, as their live presence carries
/// them (a participant's activity line).
const MAX_OWN_WORDS_BYTES: usize = 96;

/// How long a host line keeps a person listed without the live layer to
/// say they are present: a page posts every 20 seconds while open.
const HOST_ONLY_FRESH_MS: u64 = 90_000;

/// How long a device that dropped out of the live layer still counts as
/// here: back within it, it never left. A page renews its live grant every
/// few minutes by reconnecting, which drops every member and adds them
/// again, and a carrier re-announces a member as withdrawn and back; none
/// of that is someone arriving.
const REJOIN_QUIET_MS: u64 = 90_000;

/// A record a member focuses, as their live presence names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Focus {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub record: String,
}

/// One member of the room's live layer, as the page reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct LiveMember {
    pub publisher: String,
    /// `"person"` or `"agent"`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focus: Option<Focus>,
    /// An agent's own activity line.
    #[serde(default)]
    pub activity: Option<String>,
    /// Where within the room's desktop a person is: the id of the strip
    /// they are on, as their presence names it.
    #[serde(default)]
    pub place: Option<String>,
    /// The person (user root, hex) this page is certified as: given for the
    /// page's own entry only, which knows its certificate. Other members'
    /// persons come from the host's lines, which the host authenticated.
    #[serde(default)]
    pub user: Option<String>,
    /// The member's latest wave at someone here.
    #[serde(default)]
    pub wave: Option<Wave>,
    /// Where a person's windows are on their screen.
    #[serde(default)]
    pub layout: Option<MemberLayout>,
    /// Whose view the member follows: that member's publisher.
    #[serde(default)]
    pub following: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct HostLine {
    pub device: String,
    /// The person's user root (hex), as the host authenticated them.
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub recent: String,
    /// Model prose that passed the host's grounding check.
    #[serde(default)]
    pub generated: bool,
    /// The window (tab id, hex) they focus in the room's shared desktop.
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub updated_ms: u64,
    /// When the person last changed their kept profile (name, picture).
    #[serde(default)]
    pub profile_ms: u64,
}

/// A line of text and who wrote it: the person, a model, or the host
/// reading it plainly off what they did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub source: crate::collaborators::LineSource,
}

impl Line {
    /// Whether software wrote it rather than the person.
    #[must_use]
    pub fn automatic(&self) -> bool {
        self.source != crate::collaborators::LineSource::Written
    }
}

/// Someone present, as the desktop shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    /// Their live publisher (the device key for a person), hex.
    pub id: String,
    /// The person (user root, hex) as the host authenticated them, once
    /// its lines say: the id that stays theirs on every device.
    pub user: Option<String>,
    pub label: String,
    pub rgb: (u8, u8, u8),
    pub agent: bool,
    /// The window of the record they focus, when this desktop shows it.
    pub tab: Option<TabId>,
    /// Where they are, saying only what their status line does not:
    /// "Strip · App", the strip only where the workspace has more than one,
    /// the app only where the status does not already name it
    /// (`worded_place`). Where no window here shows their record, as in
    /// a room whose layout is personal: "In App", from the app their
    /// presence focuses (`focused_app`).
    pub place: Option<String>,
    pub status: Option<Line>,
    pub recent: Option<Line>,
    /// Where their windows are on their screen, as their live presence
    /// says: a person's only, from the first of their devices that says.
    pub screen: Option<Screen>,
}

/// What the page told the desktop about the room.
#[derive(Debug, Default)]
pub struct HereState {
    /// The live layer's members, or `None` while it is not live.
    members: Option<Vec<LiveMember>>,
    /// This page itself: its publisher, name and colour.
    own: Option<LiveMember>,
    lines: Vec<HostLine>,
    /// When `lines` arrived, Unix milliseconds.
    lines_at_ms: u64,
    watching: u32,
    /// Bumped on every change, for the bar's snapshot revision.
    revision: u64,
    /// The ids last registered on window presence, to forget who left.
    registered: Vec<String>,
    /// What this page's person lets the host say about them: only
    /// [`ui::StatusSharing::Automatic`] sends their actions.
    sharing: ui::StatusSharing,
    /// What this page's person says they are doing, in their own words:
    /// it travels on their live presence and leads what everyone here
    /// reads of them, before anything the host words automatically.
    own_words: Option<String>,
    /// The other people's devices already announced as here; `None` until
    /// the first roster arrived ([`HereState::take_arrivals`]).
    announced: Option<std::collections::BTreeSet<String>>,
    /// When each announced device last dropped out of the live layer, Unix
    /// milliseconds, so one back within [`REJOIN_QUIET_MS`] is not news.
    dropped: HashMap<String, u64>,
    /// The waves this page sent and heard.
    waves: waves::Waves,
    /// The last wave at this person, as the bar says it.
    notice: Option<crate::chrome::WaveNotice>,
    /// Whose view this page follows: their publisher.
    following: Option<String>,
    /// Each person's accounts elsewhere as their profile names them, by
    /// their root key (lower-case hex).
    linked: HashMap<String, Vec<tilo_app::linked_accounts::ProfileLink>>,
}

pub type Here = std::rc::Rc<std::cell::RefCell<HereState>>;

fn line(text: &str, source: crate::collaborators::LineSource) -> Option<Line> {
    let text = text.trim();
    (!text.is_empty()).then(|| Line {
        text: text.to_owned(),
        source,
    })
}

/// A line of the host's: model prose, or what it read off their actions.
fn host_line(text: &str, generated: bool) -> Option<Line> {
    line(
        text,
        crate::collaborators::LineSource::generated_if(generated),
    )
}

/// What someone says they are doing in their own words, as their live
/// presence carries it.
fn own_words(member: &LiveMember) -> Option<Line> {
    member
        .activity
        .as_deref()
        .and_then(|text| line(text, crate::collaborators::LineSource::Written))
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
        let members = self.members.clone().unwrap_or_default();
        self.hear_waves(&members);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub fn set_lines(&mut self, lines: Vec<HostLine>, now_ms: u64) -> bool {
        self.lines_at_ms = now_ms;
        if self.lines == lines {
            return false;
        }
        self.lines = lines;
        // Each device wears its person's picture.
        for line in &self.lines {
            if !line.user.is_empty() {
                ui::people_photos::alias(&line.device, &line.user);
            }
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Someone's picture changed: what shows them is drawn again.
    pub fn photos_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The accounts elsewhere `user`'s profile names now. Whether that
    /// changed anything.
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

    /// The accounts elsewhere `user`'s profile names.
    #[must_use]
    pub fn linked_of(&self, user: &str) -> &[tilo_app::linked_accounts::ProfileLink] {
        self.linked
            .get(&user.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    /// Who joined since the last call, by the name everyone here sees: a
    /// person who was not in the room before. The first roster this page
    /// sees is who was already here, not arrivals; this page's person and
    /// agents are never announced, and leaving is not news. A device back
    /// within `REJOIN_QUIET_MS` of dropping out never left, and another
    /// device of someone here is not them arriving. One whose name is not
    /// known yet (neither their presence nor the host's lines carry it) is
    /// announced once it is, by that name, as the bar shows them.
    pub fn take_arrivals(&mut self, now_ms: u64) -> Vec<String> {
        let Some(members) = &self.members else {
            return Vec::new();
        };
        let here: Vec<&LiveMember> = members
            .iter()
            .filter(|member| member.kind != "agent" && !self.is_own_person(&member.publisher))
            .collect();
        let Some(before) = &self.announced else {
            self.announced = Some(here.iter().map(|member| member.publisher.clone()).collect());
            return Vec::new();
        };
        let labels = self.labels();
        let mut arrived = Vec::new();
        let mut announced = std::collections::BTreeSet::new();
        // Who counts as here already: the people of devices announced before.
        let mut persons: Vec<&str> = here
            .iter()
            .filter(|member| before.contains(&member.publisher))
            .map(|member| self.person_of(&member.publisher))
            .collect();
        for member in &here {
            let id = member.publisher.as_str();
            let back = self
                .dropped
                .get(id)
                .is_some_and(|at| now_ms.saturating_sub(*at) < REJOIN_QUIET_MS);
            let person = self.person_of(id);
            if !before.contains(id) && !back && !persons.contains(&person) {
                let Some(name) = self.member_name(member) else {
                    continue;
                };
                arrived.push(labels.label(id, name));
                persons.push(person);
            }
            announced.insert(id.to_owned());
        }
        for gone in before.difference(&announced) {
            self.dropped.insert(gone.clone(), now_ms);
        }
        self.dropped.retain(|id, at| {
            !announced.contains(id) && now_ms.saturating_sub(*at) < REJOIN_QUIET_MS
        });
        self.announced = Some(announced);
        arrived
    }

    /// The name a member goes by here: what their presence says, else the
    /// host's line for their device.
    fn member_name<'a>(&'a self, member: &'a LiveMember) -> Option<&'a str> {
        Some(member.label.trim())
            .filter(|label| !label.is_empty())
            .or_else(|| self.line_label(&member.publisher))
    }

    fn line_label(&self, device: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|line| line.device == device)
            .map(|line| line.label.trim())
            .filter(|label| !label.is_empty())
    }

    pub fn set_watching(&mut self, watching: u32) -> bool {
        if self.watching == watching {
            return false;
        }
        self.watching = watching;
        self.revision = self.revision.wrapping_add(1);
        true
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

    /// Say what this person is doing in their own words; `None` (or only
    /// spaces) clears it. On one line, at most [`ui::MAX_STATUS_CHARS`]
    /// characters and as many bytes as a live participant's activity
    /// carries.
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

    /// The words this page publishes as what its person is doing: their
    /// own, unless they share nothing of what they do.
    pub fn shared_words(&self) -> Option<&str> {
        self.own_words()
            .filter(|_| self.sharing != ui::StatusSharing::Off)
    }

    /// What everyone here reads of this person: their own words, else
    /// the host's automatic line while they share one.
    pub fn own_doing(&self) -> Option<Line> {
        self.shared_words()
            .and_then(|words| line(words, crate::collaborators::LineSource::Written))
            .or_else(|| {
                self.own_status()
                    .filter(|_| self.sharing == ui::StatusSharing::Automatic)
            })
    }

    pub fn watching(&self) -> u32 {
        self.watching
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

    /// The person the device `publisher` is a device of, as the host's lines
    /// say; `None` before its page posted to the host.
    pub fn user_of(&self, publisher: &str) -> Option<&str> {
        if Some(publisher) == self.own_id()
            && let Some(own) = self.own_user()
        {
            return Some(own);
        }
        self.lines
            .iter()
            .find(|line| line.device == publisher && !line.user.is_empty())
            .map(|line| line.user.as_str())
    }

    /// Who `publisher` counts as: its person where known, else the device
    /// itself, so devices of one person count once.
    fn person_of<'a>(&'a self, publisher: &'a str) -> &'a str {
        self.user_of(publisher).unwrap_or(publisher)
    }

    /// Where `person`'s windows are: the layout of the first of their
    /// devices in the live layer that says.
    fn screen_of(&self, person: &str) -> Option<Screen> {
        self.members
            .iter()
            .flatten()
            .filter(|member| member.kind != "agent" && self.person_of(&member.publisher) == person)
            .find_map(|member| member.layout.as_ref().and_then(Screen::from_layout))
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

    /// The name and colour presence shows for the person `user` (their user
    /// root, hex): their page's, found through the host's lines, which
    /// pair each device with its person. `None` for someone not here now.
    ///
    /// Of a person's devices, the one live here names them, as the bar of
    /// who is here does: a device they left (a page reloaded, a tab closed)
    /// keeps its line a while, with the name it went by before they chose
    /// theirs.
    pub fn person(&self, user: &str) -> Option<(String, (u8, u8, u8))> {
        let member_of = |device: &str| {
            self.members
                .iter()
                .flatten()
                .chain(self.own.as_ref())
                .find(|member| member.publisher == device)
        };
        let mut devices = self
            .lines
            .iter()
            .filter(|line| !line.user.is_empty() && line.user == user)
            .map(|line| (line, member_of(&line.device)));
        let first = devices.next()?;
        let (line, member) = std::iter::once(first)
            .chain(devices)
            .find(|(_, member)| member.is_some())
            .unwrap_or(first);
        let label = member
            .map(|member| member.label.trim())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| line.label.trim());
        let label = if label.is_empty() {
            guest_label()
        } else {
            label.to_owned()
        };
        let label = self.labels().label(&line.device, &label);
        // A device that left the live layer keeps its line a while: their
        // person's own colour then, as when alone here.
        let rgb = self
            .colors()
            .get(&line.device)
            .copied()
            .unwrap_or_else(|| ui::presence::user_color(user));
        Some((label, rgb))
    }

    /// This page's own colour, as everyone here sees it.
    pub fn own_rgb(&self) -> Option<(u8, u8, u8)> {
        let own = self.own_id()?;
        Some(color_in(&self.colors(), own))
    }

    /// What colour everyone here is drawn in, by live publisher, this page
    /// included. Settled over the whole room at once
    /// ([`ui::presence::assign_peer_colors`]): two people here together
    /// never share a colour while the ring has room for both, and every
    /// page, seeing the same room, settles it alike. A person is settled by
    /// who they are (their user root) so their other devices wear their
    /// colour; an agent by its own publisher. The colour a page publishes
    /// for itself is not consulted: a page picking alone cannot know who
    /// else is here, which is how two visitors came to wear the same one.
    ///
    /// A person's device is in the live layer before the host's lines pair
    /// it with its person. Until they do it has no entry here, and takes no
    /// part in the settlement, so they are drawn neutral
    /// ([`ui::presence::UNSETTLED_RGB`]) rather than in a colour keyed by
    /// their device that changes once their person is known.
    pub fn colors(&self) -> HashMap<String, (u8, u8, u8)> {
        let present: Vec<(&str, &str)> = match &self.members {
            Some(members) => members
                .iter()
                .chain(self.own.as_ref())
                .filter_map(|member| {
                    let id = member.publisher.as_str();
                    if member.kind == "agent" {
                        Some((id, id))
                    } else {
                        self.user_of(id).map(|user| (id, user))
                    }
                })
                .collect(),
            // Without the live layer the host's lines are who is here, each
            // already known to the host.
            None => self
                .lines
                .iter()
                .map(|line| line.device.as_str())
                .map(|id| (id, self.user_of(id).unwrap_or(id)))
                .chain(self.own_id().and_then(|id| Some((id, self.user_of(id)?))))
                .collect(),
        };
        let keys: Vec<&str> = present.iter().map(|(_, key)| *key).collect();
        let settled = ui::presence::assign_peer_colors(&keys, &[]);
        present
            .into_iter()
            .filter_map(|(id, key)| settled.get(key).map(|rgb| (id.to_owned(), *rgb)))
            .collect()
    }

    /// This page's own line from the host.
    pub fn own_status(&self) -> Option<Line> {
        let own = self.own_id()?;
        self.lines
            .iter()
            .find(|line| line.device == own)
            .and_then(|own| host_line(&own.status, own.generated))
    }

    /// Everyone present but this page, people first, then agents, each in
    /// the order they arrived. `tree` and `records` (each window's record)
    /// place them in this desktop.
    ///
    /// `app_name` names an app by its id, for where someone is.
    pub fn people(
        &self,
        tree: &DesktopTree,
        records: &[(TabId, String)],
        app_name: &dyn Fn(&str) -> Option<String>,
        now_ms: u64,
    ) -> Vec<Person> {
        let host = |device: &str| self.lines.iter().find(|line| line.device == device);
        let colors = self.colors();
        let mut people: Vec<Person> = Vec::new();
        // Who each listed person counts as (`person_of`), beside `people`.
        let mut persons: Vec<&str> = Vec::new();
        for member in self.members.iter().flatten() {
            let agent = member.kind == "agent";
            let person = if agent {
                member.publisher.as_str()
            } else {
                self.person_of(&member.publisher)
            };
            if (!agent && self.is_own_person(&member.publisher)) || persons.contains(&person) {
                continue;
            }
            persons.push(person);
            let line_of = host(&member.publisher);
            // An agent names the record it works in; a person's page says
            // which window they are in.
            let tab = member
                .focus
                .as_ref()
                .and_then(|focus| window_of(tree, records, focus))
                .or_else(|| line_of.and_then(|l| shown_window(tree, l)));
            // Their own words first (an agent's activity is its own), else
            // what the host read off what they did.
            let status = own_words(member)
                .or_else(|| line_of.and_then(|l| host_line(&l.status, l.generated)));
            let place = match tab {
                Some(tab) => worded_place(tree, tab, app_name, status.as_ref()),
                None => focused_app(member, app_name, status.as_ref()),
            };
            people.push(Person {
                id: member.publisher.clone(),
                user: self.user_of(&member.publisher).map(str::to_owned),
                label: [
                    member.label.as_str(),
                    line_of.map_or("", |l| l.label.as_str()),
                ]
                .into_iter()
                .map(str::trim)
                .find(|label| !label.is_empty())
                .map_or_else(guest_label, str::to_owned),
                rgb: color_in(&colors, &member.publisher),
                agent,
                tab,
                place,
                status,
                recent: line_of.and_then(|l| host_line(&l.recent, l.generated)),
                screen: if agent { None } else { self.screen_of(person) },
            });
        }
        // Without the live layer the host's lines are all there is; a line
        // the person stopped refreshing has left.
        let fresh = |l: &HostLine| {
            self.members.is_none()
                && now_ms.saturating_sub(self.lines_at_ms) < HOST_ONLY_FRESH_MS
                && !self.is_own_person(&l.device)
        };
        for l in self.lines.iter().filter(|l| fresh(l)) {
            let person = self.person_of(&l.device);
            if persons.contains(&person) {
                continue;
            }
            persons.push(person);
            let tab = shown_window(tree, l);
            let status = host_line(&l.status, l.generated);
            people.push(Person {
                id: l.device.clone(),
                user: Some(l.user.clone()).filter(|user| !user.is_empty()),
                label: Some(l.label.trim())
                    .filter(|label| !label.is_empty())
                    .map_or_else(guest_label, str::to_owned),
                rgb: color_in(&colors, &l.device),
                agent: false,
                tab,
                place: tab.and_then(|tab| worded_place(tree, tab, app_name, status.as_ref())),
                status,
                recent: host_line(&l.recent, l.generated),
                screen: None,
            });
        }
        people.compact_sort_by_key(|person| person.agent);
        let labels = self.labels();
        for person in people.iter_mut().filter(|person| !person.agent) {
            person.label = labels.label(&person.id, &person.label);
        }
        people
    }

    /// This page's own name as everyone here is shown it: numbered when
    /// someone else here goes by the same name.
    pub fn own_shown(&self) -> Option<String> {
        let own = self.own_label()?;
        let id = self.own_id()?;
        Some(self.labels().label(id, own))
    }

    /// The numbers the people here are told apart by, this page's own
    /// person included: the live layer's people, else the host's lines.
    /// Keyed by live publisher, which every page in the room names alike.
    fn labels(&self) -> ui::presence::ParticipantLabels {
        let mut named: Vec<(&str, String)> = Vec::new();
        if let Some(members) = &self.members {
            for member in members.iter().chain(self.own.as_ref()) {
                if member.kind == "agent" {
                    continue;
                }
                let label = self
                    .member_name(member)
                    .map_or_else(guest_label, str::to_owned);
                named.push((member.publisher.as_str(), label));
            }
        } else {
            for line in &self.lines {
                let label = self
                    .line_label(&line.device)
                    .map_or_else(guest_label, str::to_owned);
                named.push((line.device.as_str(), label));
            }
            if let Some(own) = &self.own {
                named.push((own.publisher.as_str(), own.label.trim().to_owned()));
            }
        }
        ui::presence::ParticipantLabels::new(named.iter().map(|(id, name)| (*id, name.as_str())))
    }

    /// Show `people` on the windows they are in: each one's colour, name and
    /// kind, registered on their window's tab. Whoever left, or focuses no
    /// window here, is taken off every tab.
    /// Show the waves at this person heard since the last call: each
    /// waver's face bounces, and the last of them is the bar's notice.
    pub fn show_waves(&mut self, presence: &mut PresenceCtx) {
        let now = ui::time::now();
        for wave in self.take_waves() {
            presence.bounce(
                UserId(wave.id.clone()),
                ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
                now,
            );
            self.notice = Some(crate::chrome::WaveNotice {
                from: wave.name,
                at: now,
            });
        }
    }

    /// The last wave at this person, for the bar to say.
    pub fn wave_notice(&self) -> Option<crate::chrome::WaveNotice> {
        self.notice.clone()
    }

    pub fn register_windows(&mut self, people: &[Person], presence: &mut PresenceCtx) {
        for gone in self
            .registered
            .iter()
            .filter(|id| !people.iter().any(|p| p.id == **id && p.tab.is_some()))
        {
            let user = UserId(gone.clone());
            for list in presence
                .here
                .iter_mut()
                .filter_map(|(key, list)| matches!(key, PresenceKey::Tab { .. }).then_some(list))
            {
                list.retain(|u| *u != user);
            }
        }
        self.registered.clear();
        // Every device here, not only the one each person is listed by:
        // the pointer of someone's second tab wears their colour too, and
        // a person id from elsewhere (a call's roster) finds it.
        let colors = self.colors();
        presence.set_persons(colors.keys().filter_map(|publisher| {
            let person = self.user_of(publisher)?;
            Some((UserId(publisher.clone()), UserId(person.to_owned())))
        }));
        for (publisher, (r, g, b)) in colors {
            presence.set_color_override(UserId(publisher), &format!("#{r:02x}{g:02x}{b:02x}"));
        }
        for person in people {
            let user = UserId(person.id.clone());
            let (r, g, b) = person.rgb;
            presence.set_color_override(user.clone(), &format!("#{r:02x}{g:02x}{b:02x}"));
            presence.set_display_name(user.clone(), &person.label);
            presence.set_participant_kind(&user, person.agent);
            if let Some(tab) = person.tab {
                let key = PresenceKey::Tab {
                    id: format!("{tab:x}"),
                };
                presence.set_tab_focus(&key, user);
                self.registered.push(person.id.clone());
            }
        }
    }
}

/// `publisher`'s colour in `colors` ([`HereState::colors`]), or the
/// neutral face while the room has not settled one for it.
fn color_in(colors: &HashMap<String, (u8, u8, u8)>, publisher: &str) -> (u8, u8, u8) {
    colors
        .get(publisher)
        .copied()
        .unwrap_or(ui::presence::UNSETTLED_RGB)
}

/// What a person who gave no name is called.
pub fn guest_label() -> String {
    "Guest".to_owned()
}

/// The window a host line says its person is in, when the active
/// workspace has it.
fn shown_window(tree: &DesktopTree, line: &HostLine) -> Option<TabId> {
    let tab = u64::from_str_radix(line.window.as_deref()?, 16).ok()?;
    tree.active_workspace()?
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .any(|t| t.id == tab)
        .then_some(tab)
}

/// The window showing `focus`: the record's window, else the first window
/// of its app.
fn window_of(tree: &DesktopTree, records: &[(TabId, String)], focus: &Focus) -> Option<TabId> {
    let tabs: HashMap<TabId, &str> = tree
        .active_workspace()
        .into_iter()
        .flat_map(|w| &w.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, .. } => Some((tab.id, app_id.as_str())),
            _ => None,
        })
        .collect();
    records
        .iter()
        .find(|(tab, record)| {
            *record == focus.record && tabs.get(tab).is_some_and(|app| *app == focus.app)
        })
        .map(|(tab, _)| *tab)
}

/// Where the window `tab` is, in words beside `status`: "Strip · App".
/// The strip is named only where the workspace has more than one (the one
/// there is is no news), and the app only where `status` does not already
/// say it ("Main · Chat" over "Working in Chat" says Chat twice). `None`
/// when that leaves nothing.
fn worded_place(
    tree: &DesktopTree,
    tab: TabId,
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
        let app = match &tab.tile {
            TileContent::LocalApp { app_id, .. } => app_name(app_id),
            _ => None,
        }
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

/// Where `member` is when no window here shows it: "In App", the app
/// their presence focuses, unless `status` already names it.
fn focused_app(
    member: &LiveMember,
    app_name: &dyn Fn(&str) -> Option<String>,
    status: Option<&Line>,
) -> Option<String> {
    let app = app_name(&member.focus.as_ref()?.app)?;
    let app = app.trim();
    (!app.is_empty() && !status.is_some_and(|status| names(&status.text, app)))
        .then(|| crate::collaborators::in_app(app))
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
    !name.is_empty() && text.windows(name.len()).any(|run| run == name.as_slice())
}

/// `people` as the active workspace's collaborators, for the bar's avatars:
/// read from a copy of the tree, never written to the room's document.
pub fn overlay_collaborators(tree: &mut DesktopTree, people: &[Person]) {
    let active = tree.active;
    let Some(workspace) = tree.workspaces.iter_mut().find(|w| w.id == active) else {
        return;
    };
    for person in people {
        if workspace.collaborators.iter().any(|c| c.id == person.id) {
            continue;
        }
        let (r, g, b) = person.rgb;
        workspace.collaborators.push(Collaborator {
            id: person.id.clone(),
            name: person.label.clone(),
            active_tab: person.tab,
            cursor_color: Some(format!("#{r:02x}{g:02x}{b:02x}")),
            role_id: person.agent.then(|| "agent".to_owned()),
            ..Collaborator::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(publisher: &str, kind: &str, label: &str) -> LiveMember {
        LiveMember {
            publisher: publisher.into(),
            kind: kind.into(),
            label: label.into(),
            focus: None,
            activity: (kind == "agent").then(|| "Reading the notes".into()),
            place: None,
            user: None,
            wave: None,
            layout: None,
            following: None,
        }
    }

    /// A person's screen is their device's layout; an agent has none, even
    /// when its frame says one.
    #[test]
    fn a_person_s_layout_is_their_screen_and_an_agent_shows_none() {
        let layout = MemberLayout {
            size: [800, 600],
            windows: vec![MemberWindow {
                app: "so.tilo.notes".into(),
                record: "0123456789abcdef".into(),
                rect: [0, 0, 10_000, 10_000],
                focused: true,
            }],
        };
        let mut here = HereState::default();
        here.set_members(
            None,
            Some(vec![
                LiveMember {
                    layout: Some(layout.clone()),
                    ..member("ada", "person", "Ada")
                },
                LiveMember {
                    layout: Some(layout.clone()),
                    ..member("bot", "agent", "Scribe")
                },
            ]),
        );
        let people = here.people(&DesktopTree::default(), &[], &|_| None, 0);
        let ada = people.iter().find(|p| p.id == "ada").expect("Ada");
        assert_eq!(ada.screen, Screen::from_layout(&layout));
        assert!(ada.screen.is_some());
        let bot = people.iter().find(|p| p.id == "bot").expect("the agent");
        assert_eq!(bot.screen, None);
    }

    /// A person is named as their page's presence names them, found through
    /// the host's line pairing their device with their user root.
    #[test]
    fn a_person_is_named_by_their_pages_presence() {
        let mut here = HereState::default();
        here.set_members(None, Some(vec![member("ada-device", "person", "Ada")]));
        let line = |device: &str, user: &str, label: &str| HostLine {
            device: device.into(),
            user: user.into(),
            label: label.into(),
            ..HostLine::default()
        };
        here.set_lines(
            vec![
                line("ada-device", "ada-root", "Ada (host)"),
                line("ben-device", "ben-root", "Ben"),
                line("cy-device", "cy-root", ""),
            ],
            1,
        );
        assert_eq!(
            here.person("ada-root"),
            Some(("Ada".to_owned(), ui::presence::user_color("ada-root"))),
            "alone here, a person wears their own identity's colour"
        );
        assert_eq!(
            here.person("ben-root"),
            Some(("Ben".to_owned(), ui::presence::user_color("ben-root")))
        );
        assert_eq!(
            here.person("cy-root").map(|(name, _)| name),
            Some(guest_label())
        );
        assert_eq!(here.person("someone-who-left"), None);
    }

    /// Someone joining is worth a notice; who was already here when this
    /// page arrived, this page itself, agents and people leaving are not.
    #[test]
    fn only_people_who_join_later_are_announced() {
        let mut here = HereState::default();
        assert!(here.take_arrivals(1_000).is_empty(), "no roster yet");
        let me = member("me", "person", "Quinn");
        here.set_members(
            Some(me.clone()),
            Some(vec![me.clone(), member("ada", "person", "Ada")]),
        );
        assert!(here.take_arrivals(1_000).is_empty(), "Ada was already here");
        here.set_members(
            Some(me.clone()),
            Some(vec![
                me.clone(),
                member("ada", "person", "Ada"),
                member("ben", "person", "Ben"),
                member("bot", "agent", "Scribe"),
            ]),
        );
        assert_eq!(here.take_arrivals(1_000), ["Ben"]);
        assert!(here.take_arrivals(1_000).is_empty(), "announced once");
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(1_000).is_empty(), "leaving is not news");
        here.set_members(Some(me.clone()), None);
        assert!(here.take_arrivals(2_000).is_empty(), "the layer dropped");
        here.set_members(
            Some(me.clone()),
            Some(vec![me, member("ada", "person", "Ada")]),
        );
        assert_eq!(
            here.take_arrivals(1_000 + REJOIN_QUIET_MS),
            ["Ada"],
            "she came back"
        );
    }

    /// Someone already here who drops out of the live layer for a moment
    /// and is back, or whose presence arrives again, has not joined again:
    /// a page renews its live grant by reconnecting, which drops and re-adds
    /// every member, and a carrier re-announces a member as withdrawn and
    /// back. Neither is news; one arrival is one notice, when it happens.
    #[test]
    fn a_reconnect_or_repeat_from_someone_here_is_not_a_second_arrival() {
        let me = member("me", "person", "Quinn");
        let ben = member("ben", "person", "Ben");
        let mut here = HereState::default();
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(0).is_empty());
        here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
        assert_eq!(here.take_arrivals(1_000), ["Ben"], "announced on arrival");
        // The same frame again, and a presence update, are not arrivals.
        here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
        assert!(here.take_arrivals(2_000).is_empty());
        let mut moved = ben.clone();
        moved.place = Some("2".into());
        here.set_members(Some(me.clone()), Some(vec![me.clone(), moved]));
        assert!(here.take_arrivals(3_000).is_empty());
        // This page renews its grant ten minutes on: everyone drops out and
        // is back; then Ben's page does the same.
        for at in [600_000, 660_000] {
            here.set_members(Some(me.clone()), Some(vec![me.clone()]));
            assert!(here.take_arrivals(at).is_empty());
            here.set_members(Some(me.clone()), None);
            assert!(here.take_arrivals(at + 50).is_empty());
            here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
            assert!(here.take_arrivals(at + 300).is_empty(), "Ben never left");
        }
        let line = |device: &str| HostLine {
            device: device.into(),
            user: "ben-root".into(),
            label: "Ben".into(),
            ..HostLine::default()
        };
        here.set_lines(vec![line("ben"), line("ben-tab")], 700_000);
        here.set_members(
            Some(me.clone()),
            Some(vec![me, ben, member("ben-tab", "person", "Ben")]),
        );
        assert!(here.take_arrivals(700_000).is_empty(), "his other tab");
    }

    /// A visitor's other page (another tab, or one they left, whose host
    /// line outlives it a while) has a line from before they named
    /// themselves. The host lists lines by device key, not by which page is
    /// live, so either may come first. Whichever does, the name a window row
    /// gives its holder is the one the bar of who is here shows.
    /// A visitor's presence can arrive before it carries their name: the
    /// notice names them as the bar does, from the host's line, and waits
    /// for a name rather than calling them a guest.
    #[test]
    fn a_visitor_is_announced_by_the_name_the_bar_shows() {
        let line = |device: &str, label: &str| HostLine {
            device: device.into(),
            user: format!("{device}-root"),
            label: label.into(),
            ..HostLine::default()
        };
        let me = member("me", "person", "Quinn");
        let mut here = HereState::default();
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(1).is_empty());
        here.set_members(
            Some(me.clone()),
            Some(vec![me.clone(), member("dolphin", "person", "")]),
        );
        here.set_lines(vec![line("dolphin", "Quiet Dolphin")], 1);
        assert_eq!(here.take_arrivals(1), ["Quiet Dolphin"]);
        // Nobody names the next one yet: they wait, then arrive by name.
        here.set_members(
            Some(me.clone()),
            Some(vec![
                me,
                member("dolphin", "person", ""),
                member("otter", "person", ""),
            ]),
        );
        assert!(here.take_arrivals(1).is_empty(), "no name yet");
        here.set_lines(
            vec![
                line("dolphin", "Quiet Dolphin"),
                line("otter", "Brave Otter"),
            ],
            2,
        );
        assert_eq!(here.take_arrivals(1), ["Brave Otter"]);
        assert!(here.take_arrivals(1).is_empty(), "announced once");
    }

    #[test]
    fn a_holders_name_is_the_bars_whatever_order_the_host_lists_their_devices() {
        let line = |device: &str, label: &str| HostLine {
            device: device.into(),
            user: "visitor-root".into(),
            label: label.into(),
            ..HostLine::default()
        };
        let tree = DesktopTree::default();
        // The first page arrives before its visitor chose a name.
        let mut here = HereState::default();
        here.set_members(None, Some(vec![member("tab-before", "person", "")]));
        here.set_lines(vec![line("tab-before", "")], 1);
        assert_eq!(
            here.person("visitor-root").map(|(name, _)| name),
            Some(guest_label())
        );
        // Their live page goes by the name they chose; the earlier one is
        // gone from the live layer, but its line is still listed.
        here.set_members(
            None,
            Some(vec![member("tab-now", "person", "Curious Beaver")]),
        );
        for lines in [
            vec![line("tab-before", ""), line("tab-now", "Curious Beaver")],
            vec![line("tab-now", "Curious Beaver"), line("tab-before", "")],
        ] {
            here.set_lines(lines, 2);
            let bar: Vec<String> = here
                .people(&tree, &[], &|_| None, 2)
                .into_iter()
                .map(|person| person.label)
                .collect();
            assert_eq!(bar, ["Curious Beaver"]);
            assert_eq!(
                here.person("visitor-root").map(|(name, _)| name),
                bar.first().cloned()
            );
        }
    }

    /// Where someone is says only what their status line does not: no
    /// strip in a workspace of one, and no app their status already names.
    #[test]
    fn where_someone_is_never_repeats_their_status() {
        use tilo_desktop_model::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceId};
        let chat = Tab {
            id: 7,
            title: "chat".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.chat".into(),
                label: "Chat".into(),
                note: String::new(),
            },
            app_title: None,
        };
        let mut main = Strip::new(StripId(1), "Main");
        main.window_groups.push(WindowGroup {
            id: 8,
            active_tab: chat.id,
            tabs: vec![chat],
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.strips = vec![main];
        let mut tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let app = |id: &str| (id == "so.tilo.chat").then(|| "Chat".to_owned());
        let status = |text: &str| Line {
            text: text.into(),
            source: crate::collaborators::LineSource::Observed,
        };
        let place = |tree: &DesktopTree, line: Option<&Line>| worded_place(tree, 7, &app, line);
        assert_eq!(
            place(&tree, None),
            Some("Chat".into()),
            "one strip is no news"
        );
        assert_eq!(
            place(&tree, Some(&status("Working in Chat"))),
            None,
            "the status already says Chat"
        );
        assert_eq!(
            place(&tree, Some(&status("Working on Welcome in Notes"))),
            Some("Chat".into())
        );
        tree.workspaces[0]
            .strips
            .push(Strip::new(StripId(2), "Second"));
        assert_eq!(place(&tree, None), Some("Main · Chat".into()));
        assert_eq!(
            place(&tree, Some(&status("Working in chat"))),
            Some("Main".into()),
            "whatever the case"
        );
        assert!(!names("Chatting", "Chat"), "a word, not part of one");
    }

    /// Two visitors whose identities hash to the same colour still wear two
    /// once they are here together, and each one's page settles the room
    /// alike, so a person is the same colour on every screen, their own
    /// included.
    #[test]
    fn visitors_here_together_wear_distinct_colours_on_every_page() {
        assert_eq!(
            ui::presence::user_color("visitor-0"),
            ui::presence::user_color("visitor-13")
        );
        let line = |device: &str| HostLine {
            device: device.into(),
            user: device.into(),
            ..HostLine::default()
        };
        let page = |own: &str, other: &str| {
            let mut here = HereState::default();
            here.set_members(
                Some(member(own, "person", own)),
                Some(vec![member(other, "person", other)]),
            );
            here.set_lines(vec![line(own), line(other)], 1);
            here
        };
        let first = page("visitor-0", "visitor-13");
        let second = page("visitor-13", "visitor-0");
        assert_ne!(
            first.colors()["visitor-0"],
            first.colors()["visitor-13"],
            "two people here together never share a colour"
        );
        assert_eq!(first.colors(), second.colors(), "every page settles alike");
        assert_eq!(first.own_rgb(), Some(second.colors()["visitor-0"]));
        let tree = DesktopTree::default();
        let shown = first.people(&tree, &[], &|_| None, 0);
        assert_eq!(shown[0].rgb, second.own_rgb().expect("own colour"));
    }

    /// Someone whose device is live here before the host's lines pair it
    /// with their person is drawn neutral, never in a colour their device
    /// would key that changes once their person is known: the face on the
    /// bar, the People list's and the windows' all wear their room colour
    /// from the first frame it is known, and none before.
    #[test]
    fn a_face_is_neutral_until_the_room_knows_whose_it_is() {
        use ui::ColorCompat as _;
        let me = LiveMember {
            user: Some("me-root".into()),
            ..member("me", "person", "Quinn")
        };
        let mut here = HereState::default();
        here.set_members(
            Some(me.clone()),
            Some(vec![me, member("ben-tab", "person", "Ben")]),
        );
        let tree = DesktopTree::default();
        let shown = |here: &mut HereState| {
            let people = here.people(&tree, &[], &|_| None, 0);
            let mut presence = PresenceCtx::empty();
            here.register_windows(&people, &mut presence);
            (people[0].rgb, presence.color_for(&UserId("ben-tab".into())))
        };
        assert_eq!(
            shown(&mut here),
            (ui::presence::UNSETTLED_RGB, ui::presence::UNSETTLED_RGB),
            "not yet known whose device it is"
        );
        assert!(!here.colors().contains_key("ben-tab"));
        assert_eq!(
            here.own_rgb(),
            Some(ui::presence::assign_peer_colors(&["me-root"], &[])["me-root"]),
            "this page knows its own person"
        );
        here.set_lines(
            vec![HostLine {
                device: "ben-tab".into(),
                user: "ben-root".into(),
                label: "Ben".into(),
                ..HostLine::default()
            }],
            1,
        );
        let settled = ui::presence::assign_peer_colors(&["me-root", "ben-root"], &[]);
        assert_eq!(shown(&mut here), (settled["ben-root"], settled["ben-root"]));
        // A neutral face paints grey, not a ring colour.
        let palette = ui::tokens::colors::Colors::dark().presence;
        let painted = palette.of_rgb(ui::presence::UNSETTLED_RGB);
        assert!(painted.r().abs_diff(painted.g()) <= 1, "{painted:?}");
        assert!(painted.g().abs_diff(painted.b()) <= 1, "{painted:?}");
    }

    /// A person's devices wear one colour (the person's, once the host pairs
    /// each device with them), and the presence every window draws from
    /// learns it for each device, so a second tab's pointer matches too.
    #[test]
    fn a_persons_devices_share_their_colour_in_presence() {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(vec![
                member("ada-laptop", "person", "Ada"),
                member("ada-phone", "person", "Ada"),
            ]),
        );
        let line = |device: &str| HostLine {
            device: device.into(),
            user: "ada-root".into(),
            ..HostLine::default()
        };
        here.set_lines(vec![line("ada-laptop"), line("ada-phone")], 1);
        let colors = here.colors();
        assert_eq!(colors["ada-laptop"], colors["ada-phone"]);
        let tree = DesktopTree::default();
        let people = here.people(&tree, &[], &|_| None, 0);
        let mut presence = PresenceCtx::empty();
        here.register_windows(&people, &mut presence);
        for device in ["ada-laptop", "ada-phone"] {
            assert_eq!(
                presence.color_for(&UserId(device.into())),
                colors["ada-laptop"]
            );
            assert_eq!(
                presence.person_of(&UserId(device.into())),
                Some(&UserId("ada-root".into())),
                "each device is known as its person's"
            );
        }
    }

    /// What someone is doing reads their own words first, then a model's
    /// line, then the host's plain reading of what they did: each marked
    /// by who wrote it, so nothing automatic passes for their words.
    #[test]
    fn their_own_words_lead_then_a_models_then_the_hosts_reading() {
        use crate::collaborators::LineSource;
        let mut here = HereState::default();
        let mut ada = member("ada", "person", "Ada");
        let bo = member("bo", "person", "Bo");
        let cy = member("cy", "person", "Cy");
        ada.activity = Some("  Reviewing the launch plan ".into());
        here.set_members(Some(member("me", "person", "Me")), Some(vec![ada, bo, cy]));
        here.set_lines(
            vec![
                host("ada", "Working in Chat", false),
                host("bo", "Drafting the welcome note", true),
                host("cy", "Working in Chat", false),
            ],
            1,
        );
        let people = here.people(&DesktopTree::default(), &[], &|_| None, 0);
        let said: Vec<(&str, LineSource)> = people
            .iter()
            .filter_map(|person| person.status.as_ref())
            .map(|line| (line.text.as_str(), line.source))
            .collect();
        assert_eq!(
            said,
            [
                ("Reviewing the launch plan", LineSource::Written),
                ("Drafting the welcome note", LineSource::Generated),
                ("Working in Chat", LineSource::Observed),
            ]
        );
        assert!(!people[0].status.as_ref().is_some_and(Line::automatic));
        assert!(people[2].status.as_ref().is_some_and(Line::automatic));
    }

    /// This person's own words are what their page publishes and what
    /// everyone reads of them, unless they share nothing; without words,
    /// the host's automatic line, only while they share one.
    #[test]
    fn this_persons_own_words_lead_unless_they_share_nothing() {
        use crate::collaborators::LineSource;
        let mut here = HereState::default();
        here.set_members(Some(member("me", "person", "Me")), Some(Vec::new()));
        here.set_lines(vec![host("me", "Working in Notes", false)], 1);
        assert_eq!(
            here.own_doing().map(|line| (line.text, line.source)),
            Some(("Working in Notes".into(), LineSource::Observed))
        );
        here.set_own_words(Some("  Fixing\nthe onboarding   copy ".into()));
        assert_eq!(here.shared_words(), Some("Fixing the onboarding copy"));
        assert_eq!(
            here.own_doing().map(|line| (line.text, line.source)),
            Some(("Fixing the onboarding copy".into(), LineSource::Written))
        );
        here.set_sharing(ui::StatusSharing::ManualOnly);
        assert_eq!(here.shared_words(), Some("Fixing the onboarding copy"));
        here.set_sharing(ui::StatusSharing::Off);
        assert_eq!(here.shared_words(), None, "sharing nothing hides them");
        assert_eq!(here.own_doing(), None);
        assert_eq!(here.own_words(), Some("Fixing the onboarding copy"));
        here.set_sharing(ui::StatusSharing::Automatic);
        here.set_own_words(Some("   ".into()));
        assert_eq!(here.own_words(), None, "only spaces clear them");
        here.set_own_words(Some("x".repeat(ui::MAX_STATUS_CHARS + 20)));
        assert_eq!(
            here.own_words().map(|words| words.chars().count()),
            Some(ui::MAX_STATUS_CHARS)
        );
        here.set_own_words(Some("é".repeat(80)));
        assert_eq!(
            here.own_words().map(str::len),
            Some(96),
            "as many bytes as their presence carries"
        );
    }

    fn host(device: &str, status: &str, generated: bool) -> HostLine {
        HostLine {
            device: device.into(),
            user: format!("{device}-root"),
            label: "From host".into(),
            status: status.into(),
            recent: "Edited the text of Welcome note.".into(),
            generated,
            updated_ms: 1,
            profile_ms: 0,
            window: None,
        }
    }

    #[test]
    fn people_join_live_members_with_host_lines_and_leave_out_this_page() {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(vec![
                member("bot", "agent", "Scribe"),
                member("me", "person", "Me"),
                member("ann", "person", ""),
            ]),
        );
        here.set_lines(
            vec![
                host("ann", "Drafting Welcome note in Notes", true),
                host("me", "Mine", false),
            ],
            0,
        );
        let tree = DesktopTree::default();
        let people = here.people(&tree, &[], &|_| None, 0);
        let ids: Vec<_> = people.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ann", "bot"],
            "people first, agents after, never this page"
        );
        assert_eq!(
            people[0].label, "From host",
            "the host's label names a silent member"
        );
        assert_eq!(people[0].rgb, here.colors()["ann"]);
        assert_eq!(
            people[0].status,
            Some(Line {
                text: "Drafting Welcome note in Notes".into(),
                source: crate::collaborators::LineSource::Generated,
            })
        );
        assert!(people[1].agent);
        assert_eq!(
            people[1].status.as_ref().map(|l| l.text.as_str()),
            Some("Reading the notes"),
            "an agent's line is its own activity"
        );
        assert_eq!(here.own_status().map(|l| l.text), Some("Mine".into()));
    }

    /// In a room whose layout is personal nobody's window is on this
    /// desktop: a person's row still says where they are, from the app
    /// their presence focuses, unless their status already names it.
    #[test]
    fn someone_whose_window_is_not_here_is_in_their_app() {
        let mut here = HereState::default();
        let working = |publisher: &str, app: &str| LiveMember {
            focus: Some(Focus {
                app: app.into(),
                record: "welcome".into(),
            }),
            activity: None,
            ..member(publisher, "person", publisher)
        };
        here.set_members(
            None,
            Some(vec![
                working("ann", "so.tilo.notes"),
                working("ben", "so.tilo.notes"),
                working("cy", "so.tilo.unknown"),
            ]),
        );
        here.set_lines(vec![host("ben", "Drafting Welcome in Notes", false)], 0);
        let app = |id: &str| (id == "so.tilo.notes").then(|| "Notes".to_owned());
        let people = here.people(&DesktopTree::default(), &[], &app, 0);
        let place = |id: &str| {
            people
                .iter()
                .find(|person| person.id == id)
                .and_then(|person| person.place.clone())
        };
        assert_eq!(place("ann"), Some("In Notes".into()));
        assert_eq!(place("ben"), None, "the status already says Notes");
        assert_eq!(place("cy"), None, "an app this page cannot name");
    }

    /// Two pages going by one name are numbered, this page included, by
    /// their publishers — the same numbers on every page in the room.
    #[test]
    fn people_sharing_a_name_are_numbered_alike_on_every_page() {
        let tree = DesktopTree::default();
        let everyone = vec![
            member("pub-b", "person", "Ada"),
            member("pub-a", "person", "ada "),
            member("pub-c", "person", "Ben"),
            member("bot", "agent", "Ada"),
        ];
        let mut mine = HereState::default();
        mine.set_members(
            Some(member("pub-b", "person", "Ada")),
            Some(everyone.clone()),
        );
        let labels: Vec<String> = mine
            .people(&tree, &[], &|_| None, 0)
            .into_iter()
            .map(|person| person.label)
            .collect();
        assert_eq!(labels, ["ada (1)", "Ben", "Ada"]);
        assert_eq!(mine.own_shown().as_deref(), Some("Ada (2)"));

        let mut theirs = HereState::default();
        let mut reordered = everyone;
        reordered.reverse();
        theirs.set_members(Some(member("pub-a", "person", "ada ")), Some(reordered));
        assert_eq!(theirs.own_shown().as_deref(), Some("ada (1)"));
        assert!(
            theirs
                .people(&tree, &[], &|_| None, 0)
                .iter()
                .any(|person| person.label == "Ada (2)")
        );

        mine.set_members(
            Some(member("pub-b", "person", "Ada")),
            Some(vec![member("pub-c", "person", "Ben")]),
        );
        assert_eq!(mine.own_shown().as_deref(), Some("Ada"), "alone, no number");
    }

    #[test]
    fn host_lines_alone_list_people_only_while_fresh_and_without_the_live_layer() {
        let mut here = HereState::default();
        here.set_lines(vec![host("ann", "Editing", false)], 1_000);
        let tree = DesktopTree::default();
        assert_eq!(here.people(&tree, &[], &|_| None, 2_000).len(), 1);
        assert!(
            here.people(&tree, &[], &|_| None, 1_000 + HOST_ONLY_FRESH_MS)
                .is_empty()
        );
        here.set_members(Some(member("me", "person", "Me")), Some(Vec::new()));
        assert!(
            here.people(&tree, &[], &|_| None, 2_000).is_empty(),
            "the live layer says who is present"
        );
    }

    /// Two tabs of one person are two devices of theirs: listed once to
    /// everyone else, and to either tab never as someone else.
    #[test]
    fn a_persons_devices_are_one_person() {
        let line = |device: &str, user: &str| HostLine {
            device: device.into(),
            user: user.into(),
            label: device.into(),
            ..HostLine::default()
        };
        let on = |publisher: &str, place: &str| LiveMember {
            place: Some(place.into()),
            ..member(publisher, "person", "Ada")
        };
        let lines = vec![
            line("ada-tab-1", "ada-root"),
            line("ada-tab-2", "ada-root"),
            line("ben-tab", "ben-root"),
        ];
        let members = vec![
            on("ada-tab-1", "10"),
            on("ada-tab-2", "11"),
            on("ben-tab", "12"),
            member("bot", "agent", "Scribe"),
        ];
        let tree = DesktopTree::default();

        // Ben sees Ada once, in the order she arrived.
        let mut ben = HereState::default();
        ben.set_members(
            Some(LiveMember {
                user: Some("ben-root".into()),
                ..on("ben-tab", "12")
            }),
            Some(members.clone()),
        );
        ben.set_lines(lines.clone(), 0);
        let ids: Vec<_> = ben
            .people(&tree, &[], &|_| None, 0)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["ada-tab-1", "bot"]);

        // Ada's second tab: her first tab is her, not someone else.
        let mut ada = HereState::default();
        ada.set_members(
            Some(LiveMember {
                user: Some("ada-root".into()),
                ..on("ada-tab-2", "11")
            }),
            Some(members),
        );
        ada.set_lines(lines.clone(), 0);
        let ids: Vec<_> = ada
            .people(&tree, &[], &|_| None, 0)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["ben-tab", "bot"]);

        let mut host_only = HereState::default();
        host_only.set_members(
            Some(LiveMember {
                user: Some("ben-root".into()),
                ..on("ben-tab", "12")
            }),
            None,
        );
        host_only.set_lines(lines, 0);
        let ids: Vec<_> = host_only
            .people(&tree, &[], &|_| None, 1)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["ada-tab-1"]);
    }

    #[test]
    fn collaborators_overlay_marks_agents_and_keeps_existing_rows() {
        let mut tree = DesktopTree::default();
        let workspace = tilo_desktop_model::WorkspaceId::default();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace, "Room"));
        tree.active = workspace;
        let people = vec![Person {
            id: "bot".into(),
            user: None,
            label: "Scribe".into(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            place: None,
            status: None,
            recent: None,
            screen: None,
        }];
        overlay_collaborators(&mut tree, &people);
        let workspace = tree.active_workspace().expect("a workspace");
        let bot = workspace
            .collaborators
            .iter()
            .find(|c| c.id == "bot")
            .expect("overlaid");
        assert_eq!(bot.role_id.as_deref(), Some("agent"));
        assert_eq!(bot.cursor_color.as_deref(), Some("#010203"));
        overlay_collaborators(&mut tree, &people);
        assert_eq!(
            tree.active_workspace().map(|w| w
                .collaborators
                .iter()
                .filter(|c| c.id == "bot")
                .count()),
            Some(1)
        );
    }
}
