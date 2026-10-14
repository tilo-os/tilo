//! Who is here in a hosted room and what they are doing.
//!
//! Two sources meet here. A room's live layer names the members present
//! now (people and agents), their colour, the record they focus and an
//! agent's own activity line. The host answers `/here` with each person's
//! one-line status and a summary of their last minutes, built from the
//! titles of the actions they took (`tilo_relay_protocol::web::HERE_PATH`).
//! Neither is written to the room's desktop document: the bar, the People
//! popover and the windows read them as ambient presence.
use std::collections::HashMap;

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
}

/// One person's line from the host.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct HostLine {
    pub device: String,
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
        let own = self.own_id();
        let mut seen: Vec<&str> = Vec::new();
        let mut places = Vec::new();
        for member in self.members.iter().flatten() {
            if Some(member.publisher.as_str()) == own
                || member.kind == "agent"
                || seen.contains(&member.publisher.as_str())
            {
                continue;
            }
            seen.push(&member.publisher);
            if let Some(strip) = member.place.as_deref().and_then(|p| p.parse().ok()) {
                places.push(tilo_desktop_model::StripId(strip));
            }
        }
        places
    }

    fn own_id(&self) -> Option<&str> {
        self.own.as_ref().map(|own| own.publisher.as_str())
    }

    /// This page's own name, as its presence publishes it.
    pub fn own_label(&self) -> Option<&str> {
        self.own
            .as_ref()
            .map(|own| own.label.trim())
            .filter(|label| !label.is_empty())
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
        let own = self.own_id();
        let host = |device: &str| self.lines.iter().find(|line| line.device == device);
        let mut people: Vec<Person> = Vec::new();
        for member in self.members.iter().flatten() {
            if Some(member.publisher.as_str()) == own
                || people.iter().any(|p| p.id == member.publisher)
            {
                continue;
            }
            let agent = member.kind == "agent";
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
                && Some(l.device.as_str()) != own
        };
        for l in self.lines.iter().filter(|l| fresh(l)) {
            if people.iter().any(|p| p.id == l.device) {
                continue;
            }
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
        people.sort_by_key(|person| person.agent);
        people
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

fn guest_label() -> String {
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
        }
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
