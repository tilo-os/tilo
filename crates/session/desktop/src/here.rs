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

use collab_types::presence::Shown;
use tilo_desktop_model::{Collaborator, DesktopTree, Status, TabId, TileContent, Workspace};
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};

mod following;
mod screen;
mod waves;
pub use collab_types::presence::{
    ShownFocus as Focus, ShownLayout as MemberLayout, ShownWindow as MemberWindow,
};
pub use screen::{Screen, ScreenWindow};
pub use waves::{WAVE_AGAIN_MS, Wave, WaveEar, WaveFrom, WaveOut};

/// The most bytes of a person's own words, as their live presence carries them (a participant's
/// activity line).
const MAX_OWN_WORDS_BYTES: usize = 96;

/// How long a device that dropped out of the live layer still counts as here: back within it, it
/// never left. A page renews its live grant every few minutes by reconnecting, which drops every
/// member and adds them again, and a carrier re-announces a member as withdrawn and back; none of
/// that is someone arriving.
const REJOIN_QUIET_MS: u64 = 90_000;

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
    /// Empty for a member that said none, whose colour is settled from their
    /// identity here ([`HereState::colors`]).
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
    pub source: crate::collaborators::LineSource,
}

impl Line {
    /// Whether software wrote it rather than the person.
    #[must_use]
    pub fn automatic(&self) -> bool {
        self.source != crate::collaborators::LineSource::Written
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
    /// This is the device the desk is drawn on.
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
    /// The app following them brings forward where no window here shows their record: the app their
    /// presence focuses, as their screen's front window says it.
    #[must_use]
    pub fn followed_app(&self) -> Option<&str> {
        self.focus_app.as_deref()
    }
}

/// What the page told the desktop about the workspace.
#[derive(Debug, Default)]
pub struct HereState {
    /// The live layer's members, or `None` while it is not live.
    members: Option<Vec<LiveMember>>,
    /// Why this page cannot join the live layer at all, in the words it shows in the members'
    /// place: a live plane that could not be reached.
    unavailable: Option<String>,
    /// This page itself: its publisher, name and colour.
    own: Option<LiveMember>,
    /// What this device read off its person's own recent steps: the line it publishes for them
    /// while they share an automatic status and said nothing themselves.
    own_reading: Option<String>,
    watching: u32,
    /// Bumped on every change, for the bar's snapshot revision.
    revision: u64,
    /// The ids last registered on window presence, to forget who left.
    registered: Vec<String>,
    /// What this page's person lets their device say about them: only
    /// [`ui::StatusSharing::Automatic`] publishes what it read off their steps.
    sharing: ui::StatusSharing,
    /// What this page's person says they are doing, in their own words: it travels on their live
    /// presence and leads what everyone here reads of them, before anything their device reads off
    /// their steps.
    own_words: Option<String>,
    /// The other people's devices already announced as here; `None` until the first roster arrived
    /// ([`HereState::take_arrivals`]).
    announced: Option<std::collections::BTreeSet<String>>,
    /// When each announced device last dropped out of the live layer, Unix milliseconds, so one
    /// back within [`REJOIN_QUIET_MS`] is not news.
    dropped: HashMap<String, u64>,
    /// The waves this page sent and heard.
    waves: waves::Waves,
    /// The last wave at this person, as the bar says it.
    notice: Option<crate::chrome::WaveNotice>,
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
    /// Every agent present, by publisher: its name and when it was last here (Unix milliseconds),
    /// so one gone for good is said to have finished ([`HereState::take_agents_finished`]).
    working_agents: HashMap<String, (String, u64)>,
}

/// An agent that left the workspace: its work is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFinished {
    pub publisher: String,
    /// Its name ("Claude Code").
    pub label: String,
}

/// What an agent's newest run did, as its finished notice says it: the run, as the history keeps
/// it, so one run is told once however many sessions of the agent worked it; how many of its
/// actions changed a record (a command it ran or a page it read changed none); what it worked, in
/// the words its card lists the run by ("Edited Launch notes", or the instruction as the person
/// phrased it); and the workspace it worked in, when the history names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub run: String,
    pub changes: usize,
    pub worked: String,
    pub workspace: Option<String>,
}

impl AgentFinished {
    /// What this agent's newest run in `runs` did: the history names a connection's run as the
    /// connection named its agent ("Claude Code", or "Claude Code #2" for a numbered run of it).
    /// `None` when the history lists no run of it.
    #[must_use]
    pub fn summary(
        &self,
        runs: &tilo_actions::remote::workspace_activity::Runs,
    ) -> Option<RunSummary> {
        let client = ui::presence::agent_client(&self.label);
        let run = runs
            .runs
            .iter()
            .filter(|run| ui::presence::agent_client(&run.agent.name) == client)
            .max_by_key(|run| run.last_ms)?;
        Some(RunSummary {
            run: run.id.clone(),
            changes: run.changes(),
            worked: run.instruction.trim().to_owned(),
            workspace: run.workspace.clone().filter(|name| !name.trim().is_empty()),
        })
    }
}

/// Console, where an agent's card is.
const AGENTS_APP: &str = "so.tilo.console";
/// The action key of a finished notice's choice that opens the card of the agent of the client
/// after it.
pub const AGENT_CARD_ACTION: &str = "agent-card:";

/// The notice that `agent` finished: what its run changed and where, when `summary` says
/// ("Claude Code finished: 4 changes in Storefront"), else that it finished; under it what the run
/// worked, in the words its card lists it by ("Edited Launch notes"), never what the agent was
/// doing (it is done); with its own face and its card one choice away.
///
/// `None` when there is nothing to tell: a run that changed nothing, or one `told` already says
/// a notice told of, as another session of the same agent that worked it did.
pub fn finished_notice(
    agent: &AgentFinished,
    summary: Option<&RunSummary>,
    told: &mut std::collections::BTreeSet<String>,
) -> Option<tilo_desktop_surface::notifications::Notification> {
    use tilo_desktop_surface::i18n::{t, t_args};
    if let Some(summary) = summary
        && (summary.changes == 0 || !told.insert(summary.run.clone()))
    {
        return None;
    }
    let name = || agent.label.clone().into();
    let title = match summary {
        Some(RunSummary {
            changes,
            workspace: Some(workspace),
            ..
        }) => t_args(
            "workspace-notice-agent-finished-in",
            &[
                ("agent", name()),
                ("count", (*changes).into()),
                ("workspace", workspace.clone().into()),
            ],
        ),
        Some(RunSummary {
            changes,
            workspace: None,
            ..
        }) => t_args(
            "workspace-notice-agent-finished-changes",
            &[("agent", name()), ("count", (*changes).into())],
        ),
        None => t_args("workspace-notice-agent-finished", &[("agent", name())]),
    };
    let body = summary.map_or_else(String::new, |summary| summary.worked.clone());
    let client = ui::presence::agent_client(&agent.label);
    let mut notice = tilo_desktop_surface::notifications::Notification::new(title, body)
        .with_agent_from(agent.label.clone(), agent.publisher.clone())
        .with_app_id(AGENTS_APP)
        // It is the agent's news, said in its name wherever notices are grouped by who sent them;
        // the banner, which leads with the agent's face, shows no source line repeating it.
        .with_app_name(agent.label.clone());
    notice.actions = vec![(
        format!("{AGENT_CARD_ACTION}{client}"),
        t("workspace-notice-agent-card"),
    )];
    Some(notice)
}

pub type Here = std::rc::Rc<std::cell::RefCell<HereState>>;

fn line(text: &str, source: crate::collaborators::LineSource) -> Option<Line> {
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
    Some(member.label.trim()).filter(|label| !label.is_empty())
}

/// The row already listed for the agent at `device`: the same publisher, or the same person's
/// agent going by the same name (one agent heard through two publishers). Two sessions of one
/// client are told apart by their names ("Claude Code #2"), so they stay two rows.
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
        crate::collaborators::LineSource::Observed
    } else {
        crate::collaborators::LineSource::Written
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

    /// The clients of the agents present now ([`ui::presence::agent_client`]): "Claude Code" for a
    /// "Claude Code #2".
    fn agent_clients_here(&self) -> std::collections::BTreeSet<&str> {
        self.members
            .iter()
            .flatten()
            .filter(|member| member.kind == "agent")
            .map(|member| ui::presence::agent_client(&member.label))
            .collect()
    }

    /// The agents that left since the last call, each once: gone from the live layer for longer than
    /// a reconnect takes (`REJOIN_QUIET_MS`), so a page renewing its grant is no agent finishing.
    /// An agent's finishing is news the person did not cause, which a notice says.
    ///
    /// An agent's client restarts its session often (a new `tilo mcp` per restart), each a
    /// publisher of its own working the same run: an agent is finished once no session of its
    /// client is here, and its sessions that left are said finished together, as one.
    pub fn take_agents_finished(&mut self, now_ms: u64) -> Vec<AgentFinished> {
        let Some(members) = &self.members else {
            return Vec::new();
        };
        for member in members.iter().filter(|member| member.kind == "agent") {
            let held = self
                .working_agents
                .entry(member.publisher.clone())
                .or_insert_with(|| (member.label.clone(), now_ms));
            held.0.clone_from(&member.label);
            held.1 = now_ms;
        }
        let due = self.agents_due();
        let gone: Vec<(String, (String, u64))> = self
            .working_agents
            .extract_if(|_, (label, _)| {
                due.get(ui::presence::agent_client(label))
                    .is_some_and(|at| *at <= now_ms)
            })
            .collect();
        // One per client: the session that was here last speaks for it.
        let mut by_client: std::collections::BTreeMap<String, (String, String, u64)> =
            std::collections::BTreeMap::new();
        for (publisher, (label, seen)) in gone {
            let client = ui::presence::agent_client(&label).to_owned();
            let entry = by_client
                .entry(client)
                .or_insert_with(|| (publisher.clone(), label.clone(), seen));
            if (seen, &publisher) > (entry.2, &entry.0) {
                *entry = (publisher, label, seen);
            }
        }
        let mut finished: Vec<AgentFinished> = by_client
            .into_values()
            .map(|(publisher, label, _)| AgentFinished { publisher, label })
            .collect();
        finished.sort_by(|a, b| a.publisher.cmp(&b.publisher));
        finished
    }

    /// When an agent that left is next due to be said finished, on the clock
    /// [`Self::take_agents_finished`] is handed: the reconnect's grace after it was last here.
    /// `None` while every agent seen is still here, or another session of its client is. A page
    /// draws only when something changes, and nothing changes when an agent stays gone: it wakes
    /// for this, or the notice waits for whatever draws next.
    #[must_use]
    pub fn next_agent_finished_ms(&self) -> Option<u64> {
        self.agents_due().into_values().min()
    }

    /// When each client none of whose sessions is here is due to be said finished: the reconnect's
    /// grace after the last of its sessions was here, so a session back within it was never gone.
    fn agents_due(&self) -> HashMap<String, u64> {
        let here = self.agent_clients_here();
        let mut due: HashMap<String, u64> = HashMap::new();
        for (label, seen) in self.working_agents.values() {
            let client = ui::presence::agent_client(label);
            if here.contains(client) {
                continue;
            }
            let at = seen.saturating_add(REJOIN_QUIET_MS);
            let entry = due.entry(client.to_owned()).or_insert(at);
            *entry = (*entry).max(at);
        }
        due
    }

    /// Who joined since the last call, by the name everyone here sees: a person who was not in the
    /// workspace before. The first roster this page sees is who was already here, not arrivals;
    /// this page's person and agents are never announced, and leaving is not news. A device back
    /// within `REJOIN_QUIET_MS` of dropping out never left, and another device of someone here is
    /// not them arriving. One whose name is not known yet (their presence has not said it) is
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
                let Some(name) = member_name(member) else {
                    continue;
                };
                arrived.push(labels.label(person, name));
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

    /// Say what this person is doing in their own words; `None` (or only spaces) clears it. On one
    /// line, at most [`ui::MAX_STATUS_CHARS`] characters and as many bytes as a live participant's
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
            .and_then(|words| line(words, crate::collaborators::LineSource::Written))
            .or_else(|| {
                self.own_reading
                    .as_deref()
                    .filter(|_| self.sharing == ui::StatusSharing::Automatic)
                    .and_then(|text| line(text, crate::collaborators::LineSource::Observed))
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

    /// The person the device `publisher` is a device of, as its certificate says; `None` before it
    /// was heard.
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
            .filter(|user| !user.is_empty())
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
        let labels = self.labels();
        let colors = self.colors();
        let mut names = HashMap::new();
        for member in self.members.iter().flatten().chain(self.own.as_ref()) {
            let agent = member.kind == "agent";
            let label = member_name(member).map_or_else(guest_label, str::to_owned);
            let key = if agent {
                member.publisher.as_str()
            } else {
                self.person_of(&member.publisher)
            };
            names.insert(
                member.publisher.clone(),
                (
                    labels.label(key, &label),
                    color_in(&colors, &member.publisher),
                ),
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
    /// their devices here go by, told apart as everywhere here names them. `None` for someone not
    /// here now.
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
        let label = member_name(named).map_or_else(guest_label, str::to_owned);
        let label = self.labels().label(user, &label);
        let rgb = self
            .colors()
            .get(&named.publisher)
            .copied()
            .unwrap_or_else(|| ui::presence::user_color(user));
        Some((label, rgb))
    }

    /// This page's own colour, as everyone here sees it.
    pub fn own_rgb(&self) -> Option<(u8, u8, u8)> {
        let own = self.own_id()?;
        Some(color_in(&self.colors(), own))
    }

    /// What colour everyone here is drawn in, by live publisher, this page included. Settled over
    /// the whole workspace at once ([`ui::presence::settle_colors`]): two people here together
    /// never share a colour while the ring has room for both, and every page, seeing the same
    /// workspace, settles it alike. A person is settled by who they are (their user root) so their
    /// other devices wear their colour; an agent by its own publisher, in the colour what it is
    /// wears ([`ui::presence::participant_color`]) — the one its own shell publishes and its face,
    /// its card and its marks wear in every app.
    ///
    /// The colour someone claims for themselves is the one they wear: it is the colour they chose
    /// in their own card, or the one their identity derives, as their shell publishes it — the same
    /// colour their apps paint them in. Whoever else's identity names it steps to a free entry, so
    /// two people who chose alike still read apart; this page's own person is honoured first, so
    /// nobody here is ever drawn in their colour.
    ///
    /// A person's device is in the live layer before its certificate pairs it with its person. Until they do it has no entry here, and takes no part in the
    /// settlement, so they are drawn neutral ([`ui::presence::UNSETTLED_RGB`]) rather than in a
    /// colour keyed by their device that changes once their person is known.
    pub fn colors(&self) -> HashMap<String, (u8, u8, u8)> {
        // Each device here, by the person it is (an agent by its own
        // publisher), with the colour its shell claimed where it claimed one and the
        // one its kind wears where it claimed none: [`ui::presence::participant_color`],
        // the rule every surface asks.
        let present: Vec<Present<'_>> = self
            .members
            .iter()
            .flatten()
            .chain(self.own.as_ref())
            .filter_map(|member| {
                let id = member.publisher.as_str();
                let agent = member.kind == "agent";
                let key = if agent { id } else { self.user_of(id)? };
                let claimed = ui::presence::parse_hex_color(&member.colour);
                Some((
                    id,
                    key,
                    claimed,
                    ui::presence::participant_color(key, agent),
                ))
            })
            .collect();
        let chosen: HashMap<String, (u8, u8, u8)> = present
            .iter()
            .filter_map(|(_, key, claimed, _)| claimed.map(|rgb| ((*key).to_owned(), rgb)))
            .collect();
        let naturals: Vec<(&str, (u8, u8, u8))> = present
            .iter()
            .map(|(_, key, _, natural)| (*key, *natural))
            .collect();
        // This page's own person is settled first: their colour is theirs, and nobody here is
        // drawn in it.
        let own = self
            .own_id()
            .and_then(|own| present.iter().find(|(id, _, _, _)| *id == own))
            .map(|(_, key, _, _)| *key);
        let settled = ui::presence::settle_colors_of(&naturals, &chosen, own);
        present
            .into_iter()
            .filter_map(|(id, key, _, _)| settled.get(key).map(|rgb| (id.to_owned(), *rgb)))
            .collect()
    }

    /// Everyone present but this page, people first, then agents, each in the order they arrived.
    /// `tree` and `records` (each window's record) place them in this desktop. `app_name` names an
    /// app by its id, for where someone is.
    pub fn people(
        &self,
        tree: &DesktopTree,
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
                label: [member.label.as_str(), member.device.as_str()]
                    .into_iter()
                    .map(str::trim)
                    .find(|label| !label.is_empty())
                    .map_or_else(guest_label, str::to_owned),
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
        let labels = self.labels();
        for person in people.iter_mut().filter(|person| !person.agent) {
            // A person is told apart by their own name: every device of theirs is the same person,
            // never a second one.
            let person_of = person.user.clone().unwrap_or_else(|| person.id.clone());
            person.label = labels.label(&person_of, &person.label);
        }
        people
    }

    /// This page's own name as everyone here is shown it: numbered when someone else here goes by
    /// the same name.
    pub fn own_shown(&self) -> Option<String> {
        let own = self.own_label()?;
        let id = self.person_of(self.own_id()?).to_owned();
        Some(self.labels().label(&id, own))
    }

    /// The numbers the people here are told apart by, this page's own person included. Keyed by
    /// person, so two devices of one person are one name to number.
    fn labels<'a>(&'a self) -> ui::presence::ParticipantLabels {
        let mut named: Vec<(&'a str, String)> = Vec::new();
        // One name per person, whoever of theirs spoke first: a device that has not said whose it
        // is yet does not keep their name out.
        let guest = guest_label();
        let add = |named: &mut Vec<(&'a str, String)>, id: &'a str, name: String| match named
            .iter_mut()
            .find(|(known, _)| *known == id)
        {
            Some((_, held)) if *held == guest => held.clone_from(&name),
            Some(_) => {}
            None => named.push((id, name)),
        };
        for member in self.members.iter().flatten().chain(self.own.as_ref()) {
            if member.kind == "agent" {
                continue;
            }
            let label = member_name(member).map_or_else(guest_label, str::to_owned);
            add(&mut named, self.person_of(&member.publisher), label);
        }
        ui::presence::ParticipantLabels::new(named.iter().map(|(id, name)| (*id, name.as_str())))
    }

    /// Show `people` on the windows they are in: each one's colour, name and kind, registered on
    /// their window's tab. Whoever left, or focuses no window here, is taken off every tab. Show
    /// the waves at this person heard since the last call: each waver's face bounces, and the last
    /// of them is the bar's notice.
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
        // Every device here, not only the one each person is listed by: the pointer of someone's
        // second tab wears their colour too, and a person id from elsewhere (a call's roster) finds
        // it.
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

/// One device here as the settlement reads it: its publisher, the person it
/// is (an agent, itself), the colour its shell claimed for that person where it
/// claimed one, and the colour its kind wears where it claimed none
/// ([`ui::presence::participant_color`]).
type Present<'a> = (&'a str, &'a str, Option<(u8, u8, u8)>, (u8, u8, u8));

/// `publisher` 's colour in `colors` ([`HereState::colors`]), or the neutral face while the
/// workspace has not settled one for it.
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

/// The window showing `focus` : the record's window, else the first window of its app.
fn window_of(
    tree: &DesktopTree,
    records: &[(TabId, String, String)],
    focus: &Focus,
) -> Option<TabId> {
    let shown: HashSet<TabId> = tree
        .active_workspace()
        .into_iter()
        .flat_map(Workspace::tabs)
        .map(|tab| tab.id)
        .collect();
    records
        .iter()
        .find(|(tab, app, record)| {
            *record == focus.record && *app == focus.app && shown.contains(tab)
        })
        .map(|(tab, _, _)| *tab)
}

/// Where the window `tab` is, in words beside `status` : "Strip · App". The strip is named only
/// where the workspace has more than one (the one there is is no news), and the app only where
/// `status` does not already say it ("Main · Chat" over "Working in Chat" says Chat twice). `None`
/// when that leaves nothing.
fn worded_place(
    tree: &DesktopTree,
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
        // The app the window runs: from its tile where the desk's own model names it, else from
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
        .then(|| crate::collaborators::in_app(app))
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
    !name.is_empty() && text.windows(name.len()).any(|run| run == name.as_slice())
}

/// `rgb` as the `#rrggbb` a roster row's colour is written in.
fn hex_of((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `people` as the active workspace's collaborators, for the bar's avatars: read from a copy of the
/// tree, never written to the workspace's document.
pub fn overlay_collaborators(tree: &mut DesktopTree, people: &[Person], following: Option<&str>) {
    let active = tree.active;
    let Some(workspace) = tree.workspaces.iter_mut().find(|w| w.slot == active) else {
        return;
    };
    // Following is said again each frame, like where everyone is: someone no longer followed, or
    // no longer here, carries none.
    for known in &mut workspace.collaborators {
        known.following = false;
    }
    for person in people {
        // Who a known member is belongs to the workspace; where they are and how available they are
        // is presence's, and is said again every frame.
        if let Some(known) = workspace
            .collaborators
            .iter_mut()
            .find(|c| c.id == person.id)
        {
            // Whose view this desk follows: the bar keeps that face when it is too narrow to stand
            // everyone's. Following names a publisher, which is a person's own device as often as
            // the person.
            known.following = following.is_some_and(|followed| {
                followed == person.id || person.devices.iter().any(|device| device.id == followed)
            });
            known.active_tab = person.tab;
            // The colour everyone here draws them in, as their pointer wears it: one colour per
            // person on every face of them, whatever an older row said.
            known.cursor_color = Some(hex_of(person.rgb));
            known.in_app.clone_from(&person.focus_app);
            known.availability = Some(person.availability.id().to_owned());
            known.call = person.call.as_ref().map(ui::CallPresence::encode);
            // What they are doing now, as their device publishes it: an agent's line is what it
            // works on, which is the only "where" a roster row has for software.
            known.status = doing(person);
            // Software is said on every frame too. A row this page already held (a roster it synced,
            // or one an older build wrote) carries no marker, and every surface reads the marker to
            // know an agent is an agent: it would draw as a person — a circle where software wears a
            // rounded square — for as long as the row exists.
            if person.agent {
                known.role_id = Some(crate::collaborators::AGENT_ROLE_MARKER.to_owned());
            } else if known.role_id.as_deref() == Some(crate::collaborators::AGENT_ROLE_MARKER) {
                // Software that stopped saying it runs here is not drawn as one: the row outlives
                // the line that named it.
                known.role_id = None;
            }
            continue;
        }
        let follows = following.is_some_and(|followed| {
            followed == person.id || person.devices.iter().any(|device| device.id == followed)
        });
        workspace.collaborators.push(Collaborator {
            following: follows,
            id: person.id.clone(),
            name: person.label.clone(),
            active_tab: person.tab,
            in_app: person.focus_app.clone(),
            // The colour their pointer is drawn in on this desk (`HereState::colors`, settled over
            // everyone here), so a face, a pill and a caret for one person all wear the one colour,
            // and their face on the bar matches the one their own bar shows them.
            cursor_color: Some(hex_of(person.rgb)),
            role_id: person.agent.then(|| "agent".to_owned()),
            availability: Some(person.availability.id().to_owned()),
            call: person.call.as_ref().map(ui::CallPresence::encode),
            status: doing(person),
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
            device: String::new(),
            focus: None,
            activity: (kind == "agent").then(|| "Reading the notes".into()),
            automatic: false,
            place: None,
            user: None,
            wave: None,
            layout: None,
            following: None,
            availability: None,
            call: None,
            colour: String::new(),
        }
    }

    /// An MCP agent connected for this page's person is in the workspace's presence as an agent:
    /// listed after the people, in its client's hue, saying what it does now, and naming the app
    /// of the record it acts in, which following it opens. It is met once: the page follows it and
    /// opens its card the first time only, and someone else's agent is never theirs to be shown
    /// into.
    #[test]
    fn an_mcp_agent_session_is_present_as_an_agent_and_met_once() {
        let mut here = HereState::default();
        let own = LiveMember {
            user: Some("aa".into()),
            ..member("me", "person", "Pat")
        };
        let claude = LiveMember {
            focus: Some(Focus {
                app: "so.tilo.notes".into(),
                record: "0123456789abcdef".into(),
            }),
            activity: Some("Editing notes/Welcome".into()),
            user: Some("aa".into()),
            ..member("claude", "agent", "Claude Code")
        };
        here.set_members(
            Some(own.clone()),
            Some(vec![
                own,
                member("ada", "person", "Ada"),
                claude,
                LiveMember {
                    user: Some("bb".into()),
                    ..member("theirs", "agent", "Codex")
                },
            ]),
        );
        let people = here.people(&DesktopTree::default(), &[], &|_| None);
        let ids: Vec<&str> = people.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ada", "claude", "theirs"],
            "people first, then agents"
        );
        let agent = &people[1];
        assert!(
            agent.agent,
            "an MCP session is an agent, not this person's own device"
        );
        assert_eq!(
            agent.rgb,
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange),
            "Claude Code wears its client's hue"
        );
        assert_eq!(
            agent.status.as_ref().map(|line| line.text.as_str()),
            Some("Editing notes/Welcome")
        );
        assert_eq!(agent.focus_app.as_deref(), Some("so.tilo.notes"));
        assert_eq!(
            here.take_own_agents_arrived(),
            [("claude".to_owned(), "Claude Code".to_owned())],
            "only this person's own agent is met"
        );
        assert!(here.take_own_agents_arrived().is_empty(), "met once");
    }

    /// A finished agent's notice counts what its newest run changed: each change of its record and
    /// file edits, not the commands it ran or pages it read, and names the workspace the history gives it.
    #[test]
    fn a_finished_agent_says_how_many_changes_its_run_made_and_where() {
        use tilo_actions::remote::workspace_activity as wire;
        let target = wire::Target {
            app_id: Some("so.tilo.notes".into()),
            app_label: Some("Notes".into()),
            id: "note".into(),
            kind: String::new(),
            label: "Welcome".into(),
        };
        let action = |view: wire::ActionView| wire::RunAction {
            range: wire::Range {
                from: "aa".into(),
                to: "ab".into(),
            },
            changes: 2,
            at_ms: 1,
            summary: String::new(),
            view,
            app_id: "so.tilo.notes".into(),
            record: "note".into(),
            undo: None,
        };
        let record = || {
            action(wire::ActionView::Record {
                target: target.clone(),
            })
        };
        let run = |name: &str, last_ms: i64, actions: Vec<wire::RunAction>| wire::AgentRun {
            id: format!("mcp:{name}{last_ms}"),
            agent: wire::Author {
                name: name.into(),
                ..wire::Author::default()
            },
            workspace: Some("Storefront".into()),
            instruction: String::new(),
            decision: None,
            started_ms: 0,
            last_ms,
            range: wire::Range {
                from: "aa".into(),
                to: "ab".into(),
            },
            actions,
            undo_blocked: None,
            now: None,
        };
        let runs = wire::Runs {
            runs: vec![
                run("Claude Code", 5, vec![record()]),
                run(
                    "Claude Code #2",
                    9,
                    vec![
                        record(),
                        record(),
                        action(wire::ActionView::Command {
                            target: target.clone(),
                            command: "cargo test".into(),
                            cwd: String::new(),
                            output: Vec::new(),
                            more: false,
                            exit: Some(0),
                            started_ms: None,
                            ended_ms: None,
                            run: None,
                        }),
                        record(),
                        record(),
                    ],
                ),
                run("Codex", 12, vec![record()]),
            ],
            total: 3,
            pending: false,
            mutations: true,
        };
        let finished = AgentFinished {
            publisher: "claude".into(),
            label: "Claude Code".into(),
        };
        assert_eq!(
            finished.summary(&runs),
            Some(RunSummary {
                run: "mcp:Claude Code #29".into(),
                changes: 8,
                worked: String::new(),
                workspace: Some("Storefront".into()),
            }),
            "its newest run, counting the two changes of each of its four record edits and not the \
             command"
        );
        let unknown = AgentFinished {
            label: "Cursor".into(),
            ..finished
        };
        assert_eq!(unknown.summary(&runs), None);
    }

    /// An agent that leaves has finished, said once and only after a reconnect's grace: one back
    /// within it never left.
    #[test]
    fn an_agent_gone_for_good_has_finished() {
        let mut here = HereState::default();
        let claude = || LiveMember {
            activity: Some("Editing notes/Welcome".into()),
            ..member("claude", "agent", "Claude Code")
        };
        here.set_members(None, Some(vec![claude()]));
        assert!(here.take_agents_finished(1_000).is_empty());
        here.set_members(None, Some(Vec::new()));
        assert!(
            here.take_agents_finished(1_000 + REJOIN_QUIET_MS / 2)
                .is_empty(),
            "a reconnect is not finishing"
        );
        here.set_members(None, Some(vec![claude()]));
        assert!(
            here.take_agents_finished(2_000 + REJOIN_QUIET_MS / 2)
                .is_empty()
        );
        here.set_members(None, Some(Vec::new()));
        let later = 3_000 + REJOIN_QUIET_MS * 2;
        assert_eq!(
            here.take_agents_finished(later),
            [AgentFinished {
                publisher: "claude".into(),
                label: "Claude Code".into(),
            }]
        );
        assert!(here.take_agents_finished(later + 1).is_empty(), "said once");
    }

    /// An agent's client restarting its session is a new publisher working on: the agent has not
    /// finished while any session of it is here, and once the last leaves its sessions are said
    /// finished as one, by the one here last.
    #[test]
    fn an_agent_restarting_its_session_finishes_once() {
        let mut here = HereState::default();
        let session = |publisher: &str| member(publisher, "agent", "Claude Code");
        here.set_members(None, Some(vec![session("first")]));
        assert!(here.take_agents_finished(1_000).is_empty());
        // Its client restarts it: the first session left, a second works on.
        here.set_members(None, Some(vec![session("second")]));
        assert!(here.take_agents_finished(2_000).is_empty());
        let later = 3_000 + REJOIN_QUIET_MS * 2;
        assert!(
            here.take_agents_finished(later).is_empty(),
            "the first session left long ago, but the agent is still here"
        );
        assert_eq!(
            here.next_agent_finished_ms(),
            None,
            "nothing is due while it is here"
        );
        here.set_members(None, Some(Vec::new()));
        assert_eq!(here.next_agent_finished_ms(), Some(later + REJOIN_QUIET_MS));
        assert_eq!(
            here.take_agents_finished(later + REJOIN_QUIET_MS),
            [AgentFinished {
                publisher: "second".into(),
                label: "Claude Code".into(),
            }],
            "one notice for the agent, from the session here last"
        );
        assert!(here.take_agents_finished(later * 3).is_empty(), "said once");
        assert_eq!(here.next_agent_finished_ms(), None);
    }

    /// An agent that left is due to be said finished once the reconnect's grace has passed since
    /// it was last here, so the page can wake for it; none is due while it is here.
    #[test]
    fn an_agent_gone_is_due_once_its_grace_passes() {
        let mut here = HereState::default();
        here.set_members(None, Some(vec![member("claude", "agent", "Claude Code")]));
        assert!(here.take_agents_finished(1_000).is_empty());
        assert_eq!(here.next_agent_finished_ms(), None, "it is still here");
        here.set_members(None, Some(Vec::new()));
        assert_eq!(here.next_agent_finished_ms(), Some(1_000 + REJOIN_QUIET_MS));
        assert_eq!(here.take_agents_finished(1_000 + REJOIN_QUIET_MS).len(), 1);
        assert_eq!(
            here.next_agent_finished_ms(),
            None,
            "said, nothing more is due"
        );
    }

    /// A person's screen is their device's layout; an agent has none, even when its frame says one.
    #[test]
    fn a_person_is_named_by_their_pages_presence() {
        let mut here = HereState::default();
        let device = |publisher: &str, user: &str, label: &str| LiveMember {
            user: Some(user.into()),
            ..member(publisher, "person", label)
        };
        here.set_members(
            None,
            Some(vec![
                device("ada-device", "ada-root", "Ada"),
                device("cy-device", "cy-root", ""),
            ]),
        );
        assert_eq!(
            here.person("ada-root"),
            Some(("Ada".to_owned(), here.colors()["ada-device"])),
            "named and coloured as the bar draws her"
        );
        assert_eq!(
            here.person("cy-root").map(|(name, _)| name),
            Some(guest_label())
        );
        assert_eq!(here.person("someone-who-left"), None);
    }

    /// Someone joining is worth a notice; who was already here when this page arrived, this page
    /// itself, agents and people leaving are not.
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

    /// Someone already here who drops out of the live layer for a moment and is back, or whose
    /// presence arrives again, has not joined again: a page renews its live grant by reconnecting,
    /// which drops and re-adds every member, and a carrier re-announces a member as withdrawn and
    /// back. Neither is news; one arrival is one notice, when it happens.
    #[test]
    fn a_reconnect_or_repeat_from_someone_here_is_not_a_second_arrival() {
        let me = member("me", "person", "Quinn");
        let ben = LiveMember {
            user: Some("ben-root".into()),
            ..member("ben", "person", "Ben")
        };
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
        // This page renews its grant ten minutes on: everyone drops out and is back; then Ben's
        // page does the same.
        for at in [600_000, 660_000] {
            here.set_members(Some(me.clone()), Some(vec![me.clone()]));
            assert!(here.take_arrivals(at).is_empty());
            here.set_members(Some(me.clone()), None);
            assert!(here.take_arrivals(at + 50).is_empty());
            here.set_members(Some(me.clone()), Some(vec![me.clone(), ben.clone()]));
            assert!(here.take_arrivals(at + 300).is_empty(), "Ben never left");
        }
        let ben_tab = LiveMember {
            user: Some("ben-root".into()),
            ..member("ben-tab", "person", "Ben")
        };
        here.set_members(Some(me.clone()), Some(vec![me, ben, ben_tab]));
        assert!(here.take_arrivals(700_000).is_empty(), "his other tab");
    }

    /// A visitor's presence can arrive before it carries their name: the notice waits for the name
    /// rather than calling them a guest, and names them as the bar does once their presence says it.
    #[test]
    fn a_visitor_is_announced_by_the_name_the_bar_shows() {
        let me = member("me", "person", "Quinn");
        let mut here = HereState::default();
        here.set_members(Some(me.clone()), Some(vec![me.clone()]));
        assert!(here.take_arrivals(1).is_empty());
        here.set_members(
            Some(me.clone()),
            Some(vec![me.clone(), member("dolphin", "person", "")]),
        );
        assert!(here.take_arrivals(1).is_empty(), "no name yet");
        here.set_members(
            Some(me.clone()),
            Some(vec![me, member("dolphin", "person", "Quiet Dolphin")]),
        );
        assert_eq!(here.take_arrivals(2), ["Quiet Dolphin"]);
        assert!(here.take_arrivals(3).is_empty(), "announced once");
    }

    /// Where someone is says only what their status line does not: no strip in a workspace of one,
    /// and no app their status already names.
    #[test]
    fn where_someone_is_never_repeats_their_status() {
        use tilo_desktop_model::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceSlot};
        let chat = Tab {
            id: 7,
            title: "chat".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.chat".into(),
                label: "Chat".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
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
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Design");
        workspace.strips = vec![main];
        let mut tree = DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let app = |id: &str| (id == "so.tilo.chat").then(|| "Chat".to_owned());
        let status = |text: &str| Line {
            text: text.into(),
            source: crate::collaborators::LineSource::Observed,
        };
        let place =
            |tree: &DesktopTree, line: Option<&Line>| worded_place(tree, 7, None, &app, line);
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

    /// Two visitors whose identities hash to the same colour still wear two once they are here
    /// together, and each one's page settles the workspace alike, so a person is the same colour on
    /// every screen, their own included.
    #[test]
    fn visitors_here_together_wear_distinct_colours_on_every_page() {
        assert_eq!(
            ui::presence::user_color("visitor-0"),
            ui::presence::user_color("visitor-13")
        );
        let certified = |device: &str| LiveMember {
            user: Some(device.into()),
            ..member(device, "person", device)
        };
        let page = |own: &str, other: &str| {
            let mut here = HereState::default();
            here.set_members(Some(certified(own)), Some(vec![certified(other)]));
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
        let shown = first.people(&tree, &[], &|_| None);
        assert_eq!(shown[0].rgb, second.own_rgb().expect("own colour"));
    }

    /// An agent that announces the colour the person themselves wears does not wear it: the
    /// person keeps their own colour, on their own ring and everywhere else, and the agent reads
    /// apart from them.
    #[test]
    fn an_agent_never_wears_the_persons_own_colour() {
        let mine = ui::presence::user_color("me-root");
        let hex = format!("#{:02x}{:02x}{:02x}", mine.0, mine.1, mine.2);
        let claiming = |member: &mut LiveMember| {
            member.colour.clone_from(&hex);
            member.clone()
        };
        let person = claiming(&mut LiveMember {
            user: Some("me-root".into()),
            ..member("me", "person", "Pat")
        });
        // The agent's face names no client this build knows, so it wears what it was given.
        let agent = claiming(&mut member("helper", "agent", "Helper #1"));
        let mut here = HereState::default();
        here.set_members(Some(person), Some(vec![agent]));
        assert_eq!(
            here.own_rgb(),
            Some(mine),
            "the person wears their own colour"
        );
        assert_ne!(
            here.colors().get("helper").copied(),
            Some(mine),
            "no member is drawn in the person's own colour"
        );
        // A client hue the person themselves wears is not worn by its agent either.
        let orange = ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange);
        let orange_hex = format!("#{:02x}{:02x}{:02x}", orange.0, orange.1, orange.2);
        let mut here = HereState::default();
        here.set_members(
            Some(LiveMember {
                user: Some("me-root".into()),
                colour: orange_hex,
                ..member("me", "person", "Pat")
            }),
            Some(vec![member("claude", "agent", "Claude Code #1")]),
        );
        assert_ne!(
            here.colors().get("claude").copied(),
            Some(orange),
            "the agent steps aside from the colour the person wears"
        );
        assert_eq!(here.own_rgb(), Some(orange));
    }

    /// One agent wears one colour on every surface that draws it: the desk's faces and the cards in
    /// its windows, the People list's row, the bar's face, and the colour its own apps paint its
    /// card, its text marks and its caret in — the one rule ([`ui::presence::participant_color`]),
    /// never a colour a surface picked for itself.
    #[test]
    fn an_agent_wears_one_colour_on_every_surface() {
        let helper = "agent:helper@host";
        let (r, g, b) = ui::presence::participant_color(helper, true);
        let announced = hex_of((r, g, b));
        let me = LiveMember {
            user: Some("me-root".into()),
            ..member("me", "person", "Pat")
        };
        // The agent's own session publishes its colour with its face, as every session does.
        let agent = LiveMember {
            user: Some("helper-root".into()),
            colour: announced,
            ..member(helper, "agent", "Helper #1")
        };
        let mut here = HereState::default();
        here.set_members(Some(me), Some(vec![agent]));

        let tree = DesktopTree::default();
        let people = here.people(&tree, &[], &|_| None);
        let listed = people
            .iter()
            .find(|person| person.id == helper)
            .expect("the agent is listed");
        let mut presence = PresenceCtx::empty();
        here.register_windows(&people, &mut presence);
        assert_eq!(
            (
                here.colors().get(helper).copied(),
                listed.rgb,
                presence.color_for(&UserId(helper.into())),
                ui::presence::participant_color(helper, true),
            ),
            (Some((r, g, b)), (r, g, b), (r, g, b), (r, g, b)),
            "the board, its people row, the windows and every app settle one colour"
        );

        // The bar and the People list read the workspace's rows, which this page fills from its
        // board: the same colour again.
        let mut tree = DesktopTree {
            workspaces: vec![Workspace::new(tilo_desktop_model::WorkspaceSlot(1), "Desk")],
            ..DesktopTree::default()
        };
        tree.active = tree.workspaces[0].slot;
        overlay_collaborators(&mut tree, &people, None);
        let rows = crate::collaborators::collaborator_roster_rows(&tree, "me-root");
        let row = rows
            .iter()
            .find(|row| row.id == helper)
            .expect("the agent has a row in the People list");
        assert_eq!(
            row.color,
            vello::peniko::Color::from_rgb8(r, g, b),
            "the People list"
        );
        let mut bar =
            crate::topbar::TopBarChrome::from_tree_with_local_user(&tree, "me-root".into());
        bar.sync_from_tree(&tree);
        assert_eq!(
            bar.collaborator_colors(),
            [vello::peniko::Color::from_rgb8(r, g, b)],
            "the face on the bar"
        );
    }

    /// The colour someone chose in their own card is the colour they wear here,
    /// on their own page and on everyone else's, and whoever else's identity
    /// names the same one steps to a free entry: one person, one colour, on the
    /// bar, in the People list and in every app they edit in.
    #[test]
    fn a_person_wears_the_colour_they_chose_and_the_others_step_aside() {
        let violet = ui::presence::identity_rgb(ui::tokens::PresenceHue::Violet);
        let hex = format!("#{:02x}{:02x}{:02x}", violet.0, violet.1, violet.2);
        let claiming = |device: &str, root: &str| LiveMember {
            user: Some(root.into()),
            colour: hex.clone(),
            ..member(device, "person", device)
        };
        let quiet = |device: &str, root: &str| LiveMember {
            user: Some(root.into()),
            ..member(device, "person", device)
        };
        let mut own_page = HereState::default();
        own_page.set_members(
            Some(claiming("me", "me-root")),
            Some(vec![quiet("bo", "bo-root")]),
        );
        assert_eq!(own_page.colors()["me"], violet, "they wear what they chose");
        assert_eq!(own_page.own_rgb(), Some(violet), "and see themselves in it");
        let mut peer_page = HereState::default();
        peer_page.set_members(
            Some(quiet("bo", "bo-root")),
            Some(vec![claiming("me", "me-root")]),
        );
        assert_eq!(
            peer_page.colors()["me"],
            violet,
            "every page paints them in it too"
        );
        assert_ne!(
            peer_page.colors()["bo"],
            violet,
            "whoever else lands on it steps aside"
        );
    }

    /// Someone whose device is live here before its certificate pairs it with their person is drawn
    /// neutral, never in a colour their device would key that changes once their person is known:
    /// the face on the bar, the People list's and the windows' all wear their workspace colour from
    /// the first frame it is known, and none before.
    #[test]
    fn a_face_is_neutral_until_the_workspace_knows_whose_it_is() {
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
            let people = here.people(&tree, &[], &|_| None);
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
        here.set_members(
            here.own.clone(),
            Some(vec![
                here.own.clone().expect("this page"),
                LiveMember {
                    user: Some("ben-root".into()),
                    ..member("ben-tab", "person", "Ben")
                },
            ]),
        );
        let settled = ui::presence::assign_peer_colors(&["me-root", "ben-root"], &[]);
        assert_eq!(shown(&mut here), (settled["ben-root"], settled["ben-root"]));
        // A neutral face paints grey, not a ring colour.
        let palette = ui::tokens::colors::Colors::dark().presence;
        let painted = palette.of_rgb(ui::presence::UNSETTLED_RGB);
        assert!(painted.r().abs_diff(painted.g()) <= 1, "{painted:?}");
        assert!(painted.g().abs_diff(painted.b()) <= 1, "{painted:?}");
    }

    /// A person's devices wear one colour (the person's, once each device's certificate names
    /// them), and the presence every window draws from learns it for each device, so a second tab's
    /// pointer matches too.
    #[test]
    fn a_persons_devices_share_their_colour_in_presence() {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(
                ["ada-laptop", "ada-phone"]
                    .map(|device| LiveMember {
                        user: Some("ada-root".into()),
                        ..member(device, "person", "Ada")
                    })
                    .to_vec(),
            ),
        );
        let colors = here.colors();
        assert_eq!(colors["ada-laptop"], colors["ada-phone"]);
        let tree = DesktopTree::default();
        let people = here.people(&tree, &[], &|_| None);
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

    /// What someone is doing is the one line their device publishes, marked by who wrote it: their
    /// own words, or what their device read off their steps, so nothing automatic passes for their
    /// words.
    #[test]
    fn their_own_words_and_their_devices_reading_are_told_apart() {
        use crate::collaborators::LineSource;
        let mut here = HereState::default();
        let mut ada = member("ada", "person", "Ada");
        let mut cy = member("cy", "person", "Cy");
        ada.activity = Some("  Reviewing the launch plan ".into());
        cy.activity = Some("Working in Chat".into());
        cy.automatic = true;
        here.set_members(Some(member("me", "person", "Me")), Some(vec![ada, cy]));
        let people = here.people(&DesktopTree::default(), &[], &|_| None);
        let said: Vec<(&str, LineSource)> = people
            .iter()
            .filter_map(|person| person.status.as_ref())
            .map(|line| (line.text.as_str(), line.source))
            .collect();
        assert_eq!(
            said,
            [
                ("Reviewing the launch plan", LineSource::Written),
                ("Working in Chat", LineSource::Observed),
            ]
        );
        assert!(!people[0].status.as_ref().is_some_and(Line::automatic));
        assert!(people[1].status.as_ref().is_some_and(Line::automatic));
    }

    /// This person's own words are what their page publishes and what
    /// everyone reads of them, unless they share nothing; without words,
    /// what their device read off their steps, only while they share it.
    #[test]
    fn this_persons_own_words_lead_unless_they_share_nothing() {
        use crate::collaborators::LineSource;
        let mut here = HereState::default();
        here.set_members(Some(member("me", "person", "Me")), Some(Vec::new()));
        here.set_own_reading(Some("Working in Notes".into()));
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
        assert_eq!(
            here.own_doing().map(|line| line.text),
            Some("Working in Notes".into()),
            "their device's reading again"
        );
        here.set_sharing(ui::StatusSharing::ManualOnly);
        assert_eq!(here.own_doing(), None, "a reading only while they share it");
        here.set_sharing(ui::StatusSharing::Automatic);
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

    #[test]
    fn people_are_the_live_members_and_leave_out_this_page() {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(vec![
                member("bot", "agent", "Scribe"),
                member("me", "person", "Me"),
                LiveMember {
                    device: "Framework 16".into(),
                    ..member("ann", "person", "")
                },
            ]),
        );
        let tree = DesktopTree::default();
        let people = here.people(&tree, &[], &|_| None);
        let ids: Vec<_> = people.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ann", "bot"],
            "people first, agents after, never this page"
        );
        assert_eq!(
            people[0].label, "Framework 16",
            "a member who has not named themselves goes by their device's name"
        );
        assert!(people[1].agent);
        assert_eq!(
            people[1].status.as_ref().map(|l| l.text.as_str()),
            Some("Reading the notes"),
            "an agent's line is its own activity"
        );
    }

    /// One agent is one row, however it reaches this desk: its live presence lists it with its
    /// colour and what it says it is doing, the person's own agent included; the same agent heard
    /// through a second publisher is not drawn twice; a second session of the client stays its own
    /// row.
    #[test]
    fn one_agent_is_one_row_whichever_way_it_is_heard() {
        let tree = DesktopTree::default();
        let live_agent = |publisher: &str, label: &str| LiveMember {
            user: Some("ann-root".into()),
            activity: Some("Looking at the apps".into()),
            ..member(publisher, "agent", label)
        };
        let rows = |here: &HereState, label: &str| {
            here.people(&tree, &[], &|_| None)
                .into_iter()
                .filter(|person| person.label == label)
                .collect::<Vec<_>>()
        };
        let me = LiveMember {
            user: Some("ann-root".into()),
            ..member("me", "person", "Me")
        };

        // The person's own agent, heard on the live layer.
        let mut here = HereState::default();
        here.set_members(
            Some(me.clone()),
            Some(vec![
                member("bob", "person", "Bob"),
                live_agent("claude", "Claude Code"),
            ]),
        );
        let listed = rows(&here, "Claude Code");
        assert_eq!(listed.len(), 1, "one row for one agent: {listed:?}");
        assert!(listed[0].agent, "never one of the person's devices");
        assert_eq!(
            listed[0].rgb,
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange),
            "in its client's colour"
        );
        assert_eq!(
            listed[0]
                .status
                .as_ref()
                .map(|line| (line.text.as_str(), line.source)),
            Some((
                "Looking at the apps",
                crate::collaborators::LineSource::Written
            )),
            "its activity line, as its own words"
        );

        // The same agent through a second publisher, and a second session of the client.
        here.set_members(
            Some(me),
            Some(vec![
                live_agent("claude", "Claude Code"),
                live_agent("claude-presence", "Claude Code"),
                live_agent("claude-2", "Claude Code #2"),
            ]),
        );
        assert_eq!(
            rows(&here, "Claude Code").len(),
            1,
            "one agent heard twice is one row"
        );
        assert_eq!(
            rows(&here, "Claude Code #2").len(),
            1,
            "a second session is its own row"
        );
    }

    /// A member's own certificate names the person it is a device of: their devices count as one
    /// person.
    #[test]
    fn a_members_certificate_pairs_their_devices() {
        let mut here = HereState::default();
        let certified = |publisher: &str, label: &str| LiveMember {
            user: Some("ann-root".into()),
            ..member(publisher, "person", label)
        };
        here.set_members(
            Some(member("me", "person", "Me")),
            Some(vec![
                certified("laptop", "Framework 16"),
                certified("phone", "Phone"),
            ]),
        );
        let people = here.people(&DesktopTree::default(), &[], &|_| None);
        assert_eq!(people.len(), 1, "two devices of one person are one face");
        assert_eq!(people[0].user.as_deref(), Some("ann-root"));
        assert_eq!(people[0].label, "Framework 16");
        let colors = here.colors();
        assert_eq!(
            colors["laptop"], colors["phone"],
            "a person's devices share their person's colour"
        );
    }

    /// In a workspace whose layout is personal nobody's window is on this desktop: a person's row
    /// still says where they are, from the app their presence focuses, unless their status already
    /// names it.
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
        let mut ben = working("ben", "so.tilo.notes");
        ben.activity = Some("Drafting Welcome in Notes".into());
        ben.automatic = true;
        here.set_members(
            None,
            Some(vec![
                working("ann", "so.tilo.notes"),
                ben,
                working("cy", "so.tilo.unknown"),
            ]),
        );
        let app = |id: &str| (id == "so.tilo.notes").then(|| "Notes".to_owned());
        let people = here.people(&DesktopTree::default(), &[], &app);
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

    /// A member's focus names the record its window shows by that record's short digest, as
    /// presence names it: the host that holds the same window knows which window they are in.
    #[test]
    fn a_focus_naming_a_window_here_puts_them_in_that_window() {
        use tilo_desktop_model::{
            Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceSlot,
        };
        let digest = "0123456789abcdef";
        let welcome = Tab {
            id: 7,
            title: "Welcome".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut strip = Strip::new(StripId(1), "Main");
        strip.window_groups.push(WindowGroup {
            id: 8,
            active_tab: welcome.id,
            tabs: vec![welcome],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Design");
        workspace.strips = vec![strip];
        let tree = DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let mut here = HereState::default();
        here.set_members(
            None,
            Some(vec![LiveMember {
                focus: Some(Focus {
                    app: "so.tilo.notes".into(),
                    record: digest.into(),
                }),
                activity: None,
                ..member("ann", "person", "Ann")
            }]),
        );
        let app = |id: &str| (id == "so.tilo.notes").then(|| "Notes".to_owned());
        let held = [(7, "so.tilo.notes".to_owned(), digest.to_owned())];
        let people = here.people(&tree, &held, &app);
        let ann = people
            .iter()
            .find(|person| person.id == "ann")
            .expect("ann is here");
        assert_eq!(ann.tab, Some(7), "the window her focus names");
        assert_eq!(ann.place.as_deref(), Some("Notes"));
    }

    /// Two pages going by one name are numbered, this page included, by their publishers — the same
    /// numbers on every page in the workspace.
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
            .people(&tree, &[], &|_| None)
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
                .people(&tree, &[], &|_| None)
                .iter()
                .any(|person| person.label == "Ada (2)")
        );

        mine.set_members(
            Some(member("pub-b", "person", "Ada")),
            Some(vec![member("pub-c", "person", "Ben")]),
        );
        assert_eq!(mine.own_shown().as_deref(), Some("Ada"), "alone, no number");
    }

    /// Two tabs of one person are two devices of theirs: listed once to everyone else, and to
    /// either tab never as someone else.
    #[test]
    fn a_persons_devices_are_one_person() {
        let on = |publisher: &str, place: &str| LiveMember {
            place: Some(place.into()),
            user: Some(
                if publisher == "ben-tab" {
                    "ben-root"
                } else {
                    "ada-root"
                }
                .into(),
            ),
            ..member(publisher, "person", "Ada")
        };
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
        let ids: Vec<_> = ben
            .people(&tree, &[], &|_| None)
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
        let ids: Vec<_> = ada
            .people(&tree, &[], &|_| None)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["ben-tab", "bot"]);
    }

    /// A person's devices are listed under their name, each as it calls itself and as it said it
    /// was: what the card's device row and a pointer over a tile read.
    #[test]
    fn a_persons_devices_are_listed_under_their_name_and_never_name_them() {
        let mut here = HereState::default();
        here.set_members(
            None,
            Some(vec![
                LiveMember {
                    device: "Framework 16".into(),
                    availability: Some("available".into()),
                    user: Some("ada-root".into()),
                    ..member("ada-laptop", "person", "Ada")
                },
                LiveMember {
                    device: "ThinkPad".into(),
                    availability: Some("focusing".into()),
                    user: Some("ada-root".into()),
                    ..member("ada-handheld", "person", "Ada")
                },
                LiveMember {
                    device: "Mac".into(),
                    user: Some("ben-root".into()),
                    ..member("ben", "person", "Ben")
                },
            ]),
        );
        let people = here.people(&DesktopTree::default(), &[], &|_| None);
        let ada = people
            .iter()
            .find(|person| person.id == "ada-laptop")
            .expect("Ada, listed once");
        assert_eq!(ada.label, "Ada");
        let devices: Vec<_> = ada
            .devices
            .iter()
            .map(|device| (device.label.as_str(), device.availability))
            .collect();
        assert_eq!(
            devices,
            [
                ("Framework 16", ui::Availability::Available),
                ("ThinkPad", ui::Availability::Focusing),
            ],
            "each device as it calls itself, with how it said it was"
        );

        let names = here.person_names();
        assert_eq!(
            names.get("ada-laptop").map(|(name, _)| name.as_str()),
            Some("Ada")
        );
        assert_eq!(
            names.get("ada-handheld").map(|(name, _)| name.as_str()),
            Some("Ada"),
            "the same person, on either device, by one name"
        );
    }

    /// Whose view this desk follows is said on their row and no other's, each frame: the bar keeps
    /// that face when it has room for only one.
    #[test]
    fn collaborators_overlay_marks_whom_this_desk_follows() {
        let mut tree = DesktopTree::default();
        let workspace = tilo_desktop_model::WorkspaceSlot::default();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace, "Design"));
        tree.active = workspace;
        let people: Vec<Person> = ["ada", "cc"]
            .into_iter()
            .map(|id| Person {
                id: id.into(),
                user: None,
                label: id.into(),
                devices: Vec::new(),
                rgb: (1, 2, 3),
                agent: id == "cc",
                tab: None,
                focus_app: None,
                place: None,
                status: None,
                availability: ui::Availability::Available,
                call: None,
            })
            .collect();
        overlay_collaborators(&mut tree, &people, Some("cc"));
        let following: Vec<String> = tree
            .active_workspace()
            .expect("a workspace")
            .collaborators
            .iter()
            .filter(|c| c.following)
            .map(|c| c.id.clone())
            .collect();
        assert_eq!(following, ["cc"], "the followed one, and only they");
        // Stopping says so on every row, not just the one that was followed.
        overlay_collaborators(&mut tree, &people, None);
        assert!(
            tree.active_workspace()
                .expect("a workspace")
                .collaborators
                .iter()
                .all(|c| !c.following),
            "nobody is followed"
        );
    }

    /// What someone says they are doing reaches their row each frame, both on a row this page
    /// already holds and on one it writes now: an agent whose work the bar shows is never a name
    /// with nothing under it.
    #[test]
    fn collaborators_overlay_carries_what_they_are_doing() {
        let mut tree = DesktopTree::default();
        let workspace = tilo_desktop_model::WorkspaceSlot::default();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace, "Design"));
        tree.active = workspace;
        let person = |status: Option<Line>| Person {
            id: "cc".into(),
            user: None,
            label: "Claude Code".into(),
            devices: Vec::new(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            focus_app: None,
            place: None,
            status,
            availability: ui::Availability::Available,
            call: None,
        };
        let said = Line {
            text: "  Writing in Launch notes ".into(),
            source: crate::collaborators::LineSource::Observed,
        };
        overlay_collaborators(&mut tree, &[person(Some(said.clone()))], None);
        let row = tree
            .active_workspace()
            .and_then(|workspace| workspace.collaborators.first())
            .expect("overlaid");
        assert_eq!(
            row.status.as_ref().map(|status| status.text.as_str()),
            Some("Writing in Launch notes"),
            "their words, trimmed"
        );
        assert!(
            row.status.as_ref().is_some_and(|status| status.automatic),
            "read off what it did, never typed by it"
        );
        // The same row again, with the line it says a moment later.
        overlay_collaborators(&mut tree, &[person(Some(said))], None);
        // A line of nothing but spaces says nothing, and neither does a row nothing said of.
        let blank = Line {
            text: "   ".into(),
            source: crate::collaborators::LineSource::Written,
        };
        overlay_collaborators(&mut tree, &[person(Some(blank))], None);
        overlay_collaborators(&mut tree, &[person(None)], None);
        let row = tree
            .active_workspace()
            .and_then(|workspace| workspace.collaborators.first())
            .expect("still overlaid");
        assert_eq!(row.status, None, "nothing said of it now");
    }

    #[test]
    fn collaborators_overlay_marks_agents_and_keeps_existing_rows() {
        let mut tree = DesktopTree::default();
        let workspace = tilo_desktop_model::WorkspaceSlot::default();
        tree.workspaces
            .push(tilo_desktop_model::Workspace::new(workspace, "Design"));
        tree.active = workspace;
        let people = vec![Person {
            id: "bot".into(),
            user: None,
            label: "Scribe".into(),
            devices: Vec::new(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            focus_app: None,
            place: None,
            status: None,
            availability: ui::Availability::Available,
            call: None,
        }];
        overlay_collaborators(&mut tree, &people, None);
        let workspace = tree.active_workspace().expect("a workspace");
        let bot = workspace
            .collaborators
            .iter()
            .find(|c| c.id == "bot")
            .expect("overlaid");
        assert_eq!(bot.role_id.as_deref(), Some("agent"));
        assert_eq!(
            bot.cursor_color.as_deref(),
            Some("#010203"),
            "the colour everyone here draws them in, as their pointer wears it"
        );
        overlay_collaborators(&mut tree, &people, None);
        assert_eq!(
            tree.active_workspace().map(|w| w
                .collaborators
                .iter()
                .filter(|c| c.id == "bot")
                .count()),
            Some(1)
        );
    }

    /// A roster row this page already held is marked as software on every frame: presence says an
    /// agent is an agent, and a row written before that (or by a host that lists the agent among its
    /// members) would otherwise wear a person's face — a circle where software wears a square.
    #[test]
    fn an_agent_already_on_the_roster_still_wears_the_agent_marker() {
        let mut tree = DesktopTree::default();
        let slot = tilo_desktop_model::WorkspaceSlot::default();
        let mut workspace = tilo_desktop_model::Workspace::new(slot, "Design");
        workspace
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("bot", "Scribe"));
        tree.workspaces.push(workspace);
        tree.active = slot;
        let people = vec![Person {
            id: "bot".into(),
            user: None,
            label: "Scribe".into(),
            devices: Vec::new(),
            rgb: (1, 2, 3),
            agent: true,
            tab: None,
            focus_app: None,
            place: None,
            status: None,
            availability: ui::Availability::Available,
            call: None,
        }];
        overlay_collaborators(&mut tree, &people, None);
        let workspace = tree.active_workspace().expect("a workspace");
        let bot = workspace
            .collaborators
            .iter()
            .find(|c| c.id == "bot")
            .expect("the row it held");
        assert_eq!(
            bot.role_id.as_deref(),
            Some(crate::collaborators::AGENT_ROLE_MARKER),
            "an agent's face is software's, whichever frame wrote its row"
        );
        assert!(crate::collaborators::collaborator_is_agent(
            Some(workspace),
            bot,
            u64::MAX
        ));
    }

    fn finished_agent(publisher: &str) -> AgentFinished {
        AgentFinished {
            publisher: publisher.into(),
            label: "Claude Code".into(),
        }
    }

    fn run_summary(run: &str, changes: usize, worked: &str) -> RunSummary {
        RunSummary {
            run: run.into(),
            changes,
            worked: worked.into(),
            workspace: Some("Workspace".into()),
        }
    }

    /// A finished notice wears the agent's own face, not a person's initials, and says what the run
    /// worked under its title, in its card's words, never the live status it had while it worked.
    #[test]
    fn a_finished_notice_says_what_the_run_worked_with_the_agents_face() {
        let mut told = std::collections::BTreeSet::new();
        let notice = finished_notice(
            &finished_agent("claude"),
            Some(&run_summary("run-1", 8, "Edited Launch notes")),
            &mut told,
        )
        .expect("a run that changed records is told");
        assert_eq!(notice.title, "Claude Code finished: 8 changes in Workspace");
        assert_eq!(notice.body, "Edited Launch notes");
        let from = notice.from.expect("it is from the agent");
        assert!(from.agent, "the agent's square face, in its client's tint");
        assert_eq!(from.identity, "claude");
    }

    /// One run is one notice, however many of the agent's sessions worked it; a session whose run
    /// changed nothing tells nothing.
    #[test]
    fn one_run_is_told_once_and_a_run_of_no_changes_not_at_all() {
        let mut told = std::collections::BTreeSet::new();
        let run = run_summary("run-1", 8, "Edited Launch notes");
        assert!(finished_notice(&finished_agent("first"), Some(&run), &mut told).is_some());
        assert!(
            finished_notice(&finished_agent("second"), Some(&run), &mut told).is_none(),
            "the second session of the same run"
        );
        assert!(
            finished_notice(
                &finished_agent("third"),
                Some(&run_summary("run-2", 0, "")),
                &mut told
            )
            .is_none(),
            "a run that changed nothing"
        );
        let unknown = finished_notice(&finished_agent("fourth"), None, &mut told)
            .expect("without the history, it still says the agent finished");
        assert_eq!(unknown.title, "Claude Code finished");
        assert!(
            unknown.body.is_empty(),
            "and claims no status it no longer has"
        );
    }
}
