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

/// How long a host line keeps a person listed without the live layer to
/// say they are present: a page posts every 20 seconds while open.
const HOST_ONLY_FRESH_MS: u64 = 90_000;

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
    /// `#rrggbb`.
    #[serde(default)]
    pub color: Option<String>,
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
}

/// One person's line from the host.
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
}

/// A line of text and whether software wrote it rather than the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub automatic: bool,
}

/// Someone present, as the desktop shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// Their live publisher (the device key for a person), hex.
    pub id: String,
    pub label: String,
    pub rgb: (u8, u8, u8),
    pub agent: bool,
    /// The window of the record they focus, when this desktop shows it.
    pub tab: Option<TabId>,
    /// Where they are: "Strip · App".
    pub place: Option<String>,
    pub status: Option<Line>,
    pub recent: Option<Line>,
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
}

pub type Here = std::rc::Rc<std::cell::RefCell<HereState>>;

fn parse_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#')?;
    let byte = |at: std::ops::Range<usize>| u8::from_str_radix(hex.get(at)?, 16).ok();
    Some((byte(0..2)?, byte(2..4)?, byte(4..6)?))
}

fn line(text: &str, automatic: bool) -> Option<Line> {
    let text = text.trim();
    (!text.is_empty()).then(|| Line {
        text: text.to_owned(),
        automatic,
    })
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
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub fn set_lines(&mut self, lines: Vec<HostLine>, now_ms: u64) -> bool {
        self.lines_at_ms = now_ms;
        if self.lines == lines {
            return false;
        }
        self.lines = lines;
        self.revision = self.revision.wrapping_add(1);
        true
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

    pub fn watching(&self) -> u32 {
        self.watching
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The strips the room's other people are on, one per person whose
    /// presence names one. Agents work in records, not strips.
    pub fn strip_places(&self) -> Vec<tilo_desktop_model::StripId> {
        let mut seen: Vec<&str> = Vec::new();
        let mut places = Vec::new();
        for member in self.members.iter().flatten() {
            if member.kind == "agent" || self.is_own_person(&member.publisher) {
                continue;
            }
            let person = self.person_of(&member.publisher);
            if seen.contains(&person) {
                continue;
            }
            seen.push(person);
            if let Some(strip) = member.place.as_deref().and_then(|p| p.parse().ok()) {
                places.push(tilo_desktop_model::StripId(strip));
            }
        }
        places
    }

    fn own_id(&self) -> Option<&str> {
        self.own.as_ref().map(|own| own.publisher.as_str())
    }

    /// This page's person (user root, hex), when the page named it.
    fn own_user(&self) -> Option<&str> {
        self.own
            .as_ref()
            .and_then(|own| own.user.as_deref())
            .filter(|user| !user.is_empty())
    }

    /// The person the device `publisher` is a device of, as the host's lines
    /// say; `None` before its page posted to the host.
    fn user_of(&self, publisher: &str) -> Option<&str> {
        if Some(publisher) == self.own_id() {
            return self.own_user();
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
    pub fn person(&self, user: &str) -> Option<(String, (u8, u8, u8))> {
        let line = self
            .lines
            .iter()
            .find(|line| !line.user.is_empty() && line.user == user)?;
        let member = self
            .members
            .iter()
            .flatten()
            .chain(self.own.as_ref())
            .find(|member| member.publisher == line.device);
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
        let rgb = member
            .and_then(|member| member.color.as_deref())
            .and_then(parse_rgb)
            .unwrap_or_else(|| ui::presence::user_color(&line.device));
        Some((label, rgb))
    }

    /// This page's own colour, as its presence publishes it.
    pub fn own_rgb(&self) -> Option<(u8, u8, u8)> {
        self.own.as_ref()?.color.as_deref().and_then(parse_rgb)
    }

    /// This page's own line from the host.
    pub fn own_status(&self) -> Option<Line> {
        let own = self.own_id()?;
        self.lines
            .iter()
            .find(|line| line.device == own)
            .and_then(|own| line(&own.status, own.generated))
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
            let status = if agent {
                member
                    .activity
                    .as_deref()
                    .and_then(|text| line(text, false))
            } else {
                line_of.and_then(|l| line(&l.status, l.generated))
            };
            people.push(Person {
                id: member.publisher.clone(),
                label: [
                    member.label.as_str(),
                    line_of.map_or("", |l| l.label.as_str()),
                ]
                .into_iter()
                .map(str::trim)
                .find(|label| !label.is_empty())
                .map_or_else(guest_label, str::to_owned),
                rgb: member
                    .color
                    .as_deref()
                    .and_then(parse_rgb)
                    .unwrap_or_else(|| ui::presence::user_color(&member.publisher)),
                agent,
                tab,
                place: tab.and_then(|tab| place_of(tree, tab, app_name)),
                status,
                recent: line_of.and_then(|l| line(&l.recent, l.generated)),
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
            people.push(Person {
                id: l.device.clone(),
                label: Some(l.label.trim())
                    .filter(|label| !label.is_empty())
                    .map_or_else(guest_label, str::to_owned),
                rgb: ui::presence::user_color(&l.device),
                agent: false,
                tab,
                place: tab.and_then(|tab| place_of(tree, tab, app_name)),
                status: line(&l.status, l.generated),
                recent: line(&l.recent, l.generated),
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
        let line_label = |device: &str| {
            self.lines
                .iter()
                .find(|line| line.device == device)
                .map(|line| line.label.trim())
                .filter(|label| !label.is_empty())
        };
        let mut named: Vec<(&str, String)> = Vec::new();
        if let Some(members) = &self.members {
            for member in members.iter().chain(self.own.as_ref()) {
                if member.kind == "agent" {
                    continue;
                }
                let label = Some(member.label.trim())
                    .filter(|label| !label.is_empty())
                    .or_else(|| line_label(&member.publisher))
                    .map_or_else(guest_label, str::to_owned);
                named.push((member.publisher.as_str(), label));
            }
        } else {
            for line in &self.lines {
                let label = line_label(&line.device).map_or_else(guest_label, str::to_owned);
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

/// "Strip · App" for the window `tab`.
fn place_of(
    tree: &DesktopTree,
    tab: TabId,
    app_name: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let workspace = tree.active_workspace()?;
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
        let strip = strip.name.trim();
        Some(if strip.is_empty() {
            app
        } else {
            format!("{strip} · {app}")
        })
    })
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
            color: Some("#112233".into()),
            focus: None,
            activity: Some("Reading the notes".into()),
            place: None,
            user: None,
        }
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
            Some(("Ada".to_owned(), (0x11, 0x22, 0x33)))
        );
        // Without presence, the host's label and the device's colour.
        assert_eq!(
            here.person("ben-root"),
            Some(("Ben".to_owned(), ui::presence::user_color("ben-device")))
        );
        assert_eq!(
            here.person("cy-root").map(|(name, _)| name),
            Some(guest_label())
        );
        assert_eq!(here.person("someone-who-left"), None);
    }

    /// The strips other people are on come from their presence: this page
    /// and agents are left out, and a person without a strip counts nowhere.
    #[test]
    fn strip_places_are_the_other_people_on_strips() {
        let mut here = HereState::default();
        let on = |publisher: &str, kind: &str, place: Option<&str>| LiveMember {
            place: place.map(str::to_owned),
            ..member(publisher, kind, publisher)
        };
        here.set_members(
            Some(on("me", "person", Some("10"))),
            Some(vec![
                on("me", "person", Some("10")),
                on("ada", "person", Some("11")),
                on("lin", "person", Some("11")),
                on("sam", "person", None),
                on("bot", "agent", Some("10")),
            ]),
        );
        assert_eq!(
            here.strip_places(),
            [
                tilo_desktop_model::StripId(11),
                tilo_desktop_model::StripId(11)
            ]
        );
    }

    fn host(device: &str, status: &str, generated: bool) -> HostLine {
        HostLine {
            device: device.into(),
            user: String::new(),
            label: "From host".into(),
            status: status.into(),
            recent: "Edited the text of Welcome note.".into(),
            generated,
            updated_ms: 1,
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
        assert_eq!(people[0].rgb, (0x11, 0x22, 0x33));
        assert_eq!(
            people[0].status,
            Some(Line {
                text: "Drafting Welcome note in Notes".into(),
                automatic: true
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

        // Ben sees Ada once, in the order she arrived, and her strip once.
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
        assert_eq!(ben.strip_places(), [tilo_desktop_model::StripId(10)]);

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
        assert_eq!(ada.strip_places(), [tilo_desktop_model::StripId(12)]);

        // Without the live layer the host's lines merge the same way.
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
            label: "Scribe".into(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            place: None,
            status: None,
            recent: None,
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
