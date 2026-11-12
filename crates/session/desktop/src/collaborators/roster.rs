//! Who the desk says is here: the roster's rows and the names it gives people.
//!
//! A row is one person or agent of the active workspace, with what the tree
//! says of them, the colour they are drawn in, the tab they are in and, for
//! software, the window it works in. The overlay paints what this derives;
//! nothing here measures or paints.

use std::collections::HashMap;

use vello::peniko::Color;

use tilo_desktop_model::{Collaborator, DesktopTree};

use crate::i18n::{t, t_args};

use super::{LineSource, RosterRow, RosterStatus, doing_line};

/// Every person and agent in the active workspace as the roster lists them.
pub fn collaborator_roster_rows(desktop: &DesktopTree, local_user: &str) -> Vec<RosterRow> {
    collaborator_roster_rows_named(desktop, local_user, None).0
}

/// [`collaborator_roster_rows`], with people who share a name numbered ([`ui::presence::ParticipantLabels`]) — `own_name`, this person's own, among them — and what this person is shown as.
pub fn collaborator_roster_rows_named(
    desktop: &DesktopTree,
    local_user: &str,
    own_name: Option<&str>,
) -> (Vec<RosterRow>, Option<String>) {
    let mut rows = unnumbered_roster_rows(desktop, local_user);
    let own_name = own_name.map(str::trim).filter(|name| !name.is_empty());
    let policy = desktop
        .active_workspace()
        .map(|workspace| &workspace.policy);
    let joined = |id: &str| policy.and_then(|policy| policy.joined_at(id));
    let labels = ui::presence::ParticipantLabels::by_joining(
        rows.iter()
            .filter(|row| !row.is_agent && !row.own_device && row.id != local_user)
            .map(|row| (row.id.as_str(), row.label.as_str()))
            .chain(own_name.map(|name| (local_user, name)))
            .map(|(id, name)| (id, name, joined(id))),
    );
    for row in rows
        .iter_mut()
        .filter(|row| !row.is_agent && !row.own_device)
    {
        row.label = labels.label(&row.id, &row.label);
    }
    (rows, own_name.map(|name| labels.label(local_user, name)))
}

/// What each person in the active workspace is called, exactly as the roster calls them (numbered where two share a name), with this person by their own name: the one source every surface that names people reads, so a name is spelt and cased the same in People, the roster and settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeopleNames(HashMap<String, String>);

impl PeopleNames {
    /// The roster's names for the active workspace of `desktop` , as `local_user` (named `own_name` ) sees it.
    #[must_use]
    pub fn of(desktop: &DesktopTree, local_user: &str, own_name: Option<&str>) -> Self {
        let (rows, own) = collaborator_roster_rows_named(desktop, local_user, own_name);
        let mut names: HashMap<String, String> = rows
            .into_iter()
            .filter(|row| !row.label.trim().is_empty())
            .map(|row| (row.id, row.label))
            .collect();
        if let Some(own) = own.filter(|own| !own.trim().is_empty()) {
            names.insert(local_user.to_owned(), own);
        }
        Self(names)
    }

    /// What the person `id` is called, when the roster names them.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&str> {
        self.0.get(id).map(String::as_str)
    }
}

fn unnumbered_roster_rows(desktop: &DesktopTree, local_user: &str) -> Vec<RosterRow> {
    // Presence publication updates the active workspace and the legacy tree-wide mirror together,
    // but readers can observe one side of that handoff before the other (and old control clients
    // still update only the mirror). Merge by stable participant id, preferring the workspace row's
    // richer activity fields, so nobody temporarily disappears from People.
    let mut roster = desktop
        .active_workspace()
        .map(|workspace| workspace.collaborators.clone())
        .unwrap_or_default();
    for participant in &desktop.collaborators {
        if !roster.iter().any(|row| row.id == participant.id) {
            roster.push(participant.clone());
        }
    }
    rows_of(desktop.active_workspace(), &roster, local_user)
}

fn rows_of(
    workspace: Option<&tilo_desktop_model::Workspace>,
    roster: &[Collaborator],
    local_user: &str,
) -> Vec<RosterRow> {
    let now = tilo_clock::unix_now();
    let agents = agent_members(workspace, now);
    let of = |participant: &Collaborator| {
        has_agent_marker(participant)
            || workspace.is_some_and(|workspace| {
                is_workspace_bot(workspace, participant) || names_agent(&agents, participant)
            })
    };
    let colors = roster_colors_with_local_user(roster, local_user, of);
    let rows: Vec<RosterRow> = roster
        .iter()
        .map(|participant| {
            let is_bot =
                workspace.is_some_and(|workspace| is_workspace_bot(workspace, participant));
            let is_agent = of(participant);
            // One rule decides a face's colour wherever it is drawn: the workspace's, as their
            // own shell publishes it, else the one their kind wears
            // ([`ui::presence::participant_color`]).
            let (red, green, blue) = colors
                .get(&participant.id)
                .copied()
                .unwrap_or_else(|| ui::presence::participant_color(&participant.id, is_agent));
            RosterRow {
                id: participant.id.clone(),
                initial: roster_initial(&participant.name),
                label: participant.name.clone(),
                color: Color::from_rgb8(red, green, blue),
                is_agent,
                is_bot,
                role: participant
                    .role_id
                    .as_deref()
                    .filter(|_| !has_agent_marker(participant))
                    .map(display_role_id),
                invited: participant.invited,
                own_device: participant.own_device,
                // A person's row says which record they are looking at. A piece of software's does
                // not: the window it works in is named under its row, beside what it is doing
                // there, and a "Viewing Launch notes" over "Working in Notes · Launch notes" says
                // one record twice.
                activity: (!is_agent)
                    .then(|| {
                        participant
                            .active_tab
                            .and_then(|tab| tab_title_in(workspace, tab))
                    })
                    .flatten()
                    .map(|title| {
                        crate::i18n::t_args("people-panel-viewing", &[("title", title.into())])
                    }),
                // Software works in a window of this workspace, the same tab everyone here sees:
                // its row names it, and pressing it brings that window forward.
                working_in: is_agent
                    .then(|| {
                        let tab = participant.active_tab?;
                        Some((tab, tab_title_in(workspace, tab)?))
                    })
                    .flatten(),
                working_app: is_agent
                    .then(|| {
                        participant.in_app.clone().or_else(|| {
                            participant
                                .active_tab
                                .and_then(|tab| app_id_in(workspace, tab))
                        })
                    })
                    .flatten(),
                // What it is doing, read off its own device's word: an agent's row is where its
                // live work shows, not only its name.
                status: participant
                    .status
                    .as_ref()
                    .map(|status| status.text.trim())
                    .filter(|line| !line.is_empty())
                    .map(|line| RosterStatus {
                        text: line.to_owned(),
                        // Software's line about its work is read off that work ("Writing in Launch
                        // notes"), never typed by it; a person's is theirs.
                        source: if participant
                            .status
                            .as_ref()
                            .is_some_and(|status| status.automatic)
                        {
                            LineSource::Observed
                        } else {
                            LineSource::Written
                        },
                    }),
                // Everyone here can be followed, software included: following
                // an agent that has published no view yet keeps its run in
                // front as that arrives, and the row still says where it is.
                // Offering it only to agents already seen working left people
                // with no way to follow software from a desk that had not
                // watched it work, and none at all on a phone.
                recent: Vec::new(),
                followable: !participant.invited,
                // How available they are and the call they are in are presence's, said of them as
                // they arrive (and of a known member again each frame); a row nothing said of is a
                // person at their machine, in no call.
                availability: participant
                    .availability
                    .as_deref()
                    .and_then(ui::Availability::from_id)
                    .unwrap_or(ui::Availability::Available),
                call: participant
                    .call
                    .as_deref()
                    .and_then(ui::CallPresence::decode)
                    .map(|call| call.name)
                    .filter(|name| !name.is_empty()),
                join: None,
            }
        })
        .collect();
    // The collaborators popover is immediate presence, not membership administration. An agent
    // granted access but not connected belongs in Members; once its live session arrives in
    // `roster` , the identity match above tags it as an agent and exposes the same cursor/follow
    // affordance through truthful semantic activity rather than a fabricated cursor.
    rows
}

/// The role id a workspace with no grants (an ephemeral, hosted roster) sets on a participant that is software, not a person.
pub const AGENT_ROLE_MARKER: &str = "agent";

fn has_agent_marker(participant: &Collaborator) -> bool {
    participant.role_id.as_deref() == Some(AGENT_ROLE_MARKER)
}

/// Whether `participant` is one of `workspace` 's bots, as the workspace's governance names them.
fn is_workspace_bot(workspace: &tilo_desktop_model::Workspace, participant: &Collaborator) -> bool {
    workspace.bots.iter().any(|bot| bot.is(&participant.id))
}

/// Whether `participant` is an agent: admitted to `workspace` as one (by id or display name), one of its bots, or marked with [`AGENT_ROLE_MARKER`].
#[must_use]
pub fn collaborator_is_agent(
    workspace: Option<&tilo_desktop_model::Workspace>,
    participant: &Collaborator,
    now: u64,
) -> bool {
    has_agent_marker(participant)
        || workspace.is_some_and(|workspace| {
            is_workspace_bot(workspace, participant)
                || names_agent(&agent_members(Some(workspace), now), participant)
        })
}

/// The ids of the agents the workspace's grants name, as agents.
fn agent_members(workspace: Option<&tilo_desktop_model::Workspace>, now: u64) -> Vec<String> {
    workspace.map_or_else(Vec::new, |workspace| {
        workspace
            .agent_members_at(now)
            .into_iter()
            .map(|(agent, _)| agent)
            .collect()
    })
}

/// Whether `participant` is one of `agents` , by id or by the name it shows.
fn names_agent(agents: &[String], participant: &Collaborator) -> bool {
    agents
        .iter()
        .any(|agent| agent == &participant.id || agent == &participant.name)
}

/// A recorded role id as people read it; an id this model does not define shows as recorded.
fn display_role_id(id: &str) -> String {
    tilo_desktop_model::role_from_id(id)
        .map_or_else(|| id.to_string(), crate::workspace_settings::role_label)
}

/// What colour each person on `roster` is drawn in, with `local` the principal this device speaks
/// as and `is_agent` saying which of them is software: a person is settled by their identity, an
/// agent by what it is ([`ui::presence::participant_color`]), so one actor is one colour on the
/// bar, in the People list and in every window.
pub fn roster_colors_with_local_user(
    roster: &[Collaborator],
    local: &str,
    is_agent: impl Fn(&Collaborator) -> bool,
) -> HashMap<String, (u8, u8, u8)> {
    let chosen: HashMap<String, (u8, u8, u8)> = roster
        .iter()
        .filter_map(|c| {
            c.cursor_color
                .as_deref()
                .and_then(ui::presence::parse_hex_color)
                .map(|rgb| (c.id.clone(), rgb))
        })
        .collect();
    let mut naturals: Vec<(&str, (u8, u8, u8))> = roster
        .iter()
        .map(|c| {
            (
                c.id.as_str(),
                ui::presence::participant_color(&c.id, is_agent(c)),
            )
        })
        .collect();
    if !local.is_empty() {
        let agent = roster.iter().find(|c| c.id == local).is_some_and(&is_agent);
        naturals.push((local, ui::presence::participant_color(local, agent)));
    }
    // The person at this device is honoured first: their own colour is not given to anyone on
    // the roster.
    ui::presence::settle_colors_of(&naturals, &chosen, (!local.is_empty()).then_some(local))
}

fn roster_initial(name: &str) -> char {
    name.chars()
        .next()
        .map_or('\u{2022}', |character| character.to_ascii_uppercase())
}

/// Where someone is when no window here shows it: "In Notes".
pub fn in_app(app: &str) -> String {
    crate::i18n::t_args("people-panel-in-app", &[("app", app.to_owned().into())])
}

/// The app the window `tab_id` runs, by its id: what names it where its own presence kept no word
/// of the app, and its tile does.
fn app_id_in(workspace: Option<&tilo_desktop_model::Workspace>, tab_id: u64) -> Option<String> {
    let tile = &workspace?.tabs().find(|tab| tab.id == tab_id)?.tile;
    let tilo_desktop_model::TileContent::LocalApp { app_id, .. } = tile else {
        return None;
    };
    let app_id = app_id.trim();
    (!app_id.is_empty()).then(|| app_id.to_owned())
}

fn tab_title_in(workspace: Option<&tilo_desktop_model::Workspace>, tab_id: u64) -> Option<String> {
    workspace?
        .tabs()
        .find(|tab| tab.id == tab_id)
        .map(|tab| tab.title.trim())
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
}

/// What a screen reader says about a present row beyond its name: an agent, what they are on now, what they are doing.
pub(super) fn spoken_details(row: &RosterRow) -> String {
    let mut details = Vec::new();
    if row.is_agent {
        details.push(t("a11y-shell-people-agent"));
    }
    if let Some(activity) = &row.activity {
        details.push(t_args(
            "a11y-shell-people-now",
            &[("activity", activity.into())],
        ));
    }
    if let Some(doing) = doing_line(row) {
        details.push(doing);
    }
    // Where software works: the line under its row says it on screen, and a reader is told it too.
    if let Some((_, record)) = &row.working_in {
        details.push(in_app(record));
    }
    details.join("; ")
}
