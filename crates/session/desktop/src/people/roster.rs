//! Who the desk says is here: the roster's rows and the names it gives people.
//!
//! A row is one member of the active workspace — a person, an agent or a bot — with the colour
//! their member row wears, said beside where presence puts them and what they say they are doing.
//! The overlay paints what this derives; nothing here measures or paints.

use std::collections::HashMap;

use collab_types::presence::identity_rgb;
use collab_types::{Member, MemberId};
use tilo_desktop_model::{DesktopTree, Present, Workspace};
use vello::peniko::Color;

use crate::i18n::{t, t_args};

use super::{LineSource, RosterRow, RosterStatus, doing_line};

/// Every member of the active workspace who is here now, as the roster lists them.
pub fn roster_rows(desktop: &DesktopTree, local_user: &str) -> Vec<RosterRow> {
    roster_rows_named(desktop, local_user, None).0
}

/// [`roster_rows`], with people who share a name numbered ([`ui::presence::ParticipantLabels`]) —
/// `own_name`, this person's own, among them — and what this person is shown as.
pub fn roster_rows_named(
    desktop: &DesktopTree,
    local_user: &str,
    own_name: Option<&str>,
) -> (Vec<RosterRow>, Option<String>) {
    let Some(workspace) = desktop.active_workspace() else {
        return (Vec::new(), own_name.map(str::to_owned));
    };
    let labels = names_of(workspace, local_user, own_name);
    let own_name = own_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| labels.label(&MemberId::person(local_user).key(), name));
    let rows = rows_of(workspace, &workspace.present, &labels);
    (rows, own_name)
}

/// What each member of `workspace` is called, numbered where two people share a name.
fn names_of(
    workspace: &Workspace,
    local_user: &str,
    own_name: Option<&str>,
) -> ui::presence::ParticipantLabels {
    let own_id = MemberId::person(local_user).key();
    let mut people: Vec<(String, String, Option<u64>)> = workspace
        .members
        .iter()
        .filter(|member| member.id.is_person() && member.id.key() != own_id)
        .map(|member| {
            let id = member.id.key();
            let joined = workspace.policy.joined_at(&id).or(member.joined_at);
            (id, member.name.clone(), joined)
        })
        .collect();
    if let Some(name) = own_name.map(str::trim).filter(|name| !name.is_empty()) {
        people.push((
            own_id,
            name.to_owned(),
            workspace.policy.joined_at(local_user),
        ));
    }
    let people: Vec<(&str, &str, Option<u64>)> = people
        .iter()
        .map(|(id, name, joined)| (id.as_str(), name.as_str(), *joined))
        .collect();
    ui::presence::ParticipantLabels::by_joining(people)
}

/// What each member in the active workspace is called, exactly as the roster calls them (numbered
/// where two share a name), with this person by their own name: the one source every surface that
/// names people reads, so a name is spelt and cased the same in People, the roster and settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeopleNames(HashMap<String, String>);

impl PeopleNames {
    /// The roster's names for the active workspace of `desktop`, as `local_user` (named
    /// `own_name`) sees it.
    #[must_use]
    pub fn of(desktop: &DesktopTree, local_user: &str, own_name: Option<&str>) -> Self {
        let Some(workspace) = desktop.active_workspace() else {
            return Self::default();
        };
        let labels = names_of(workspace, local_user, own_name);
        let mut names: HashMap<String, String> = workspace
            .members
            .iter()
            .filter(|member| !member.name.trim().is_empty())
            .map(|member| {
                (
                    member.id.key(),
                    labels.label(&member.id.key(), &member.name),
                )
            })
            .collect();
        if let Some(own) = own_name.map(str::trim).filter(|own| !own.is_empty()) {
            let id = MemberId::person(local_user).key();
            names.insert(id, labels.label(&MemberId::person(local_user).key(), own));
        }
        Self(names)
    }

    /// What the person `id` is called, when the roster names them.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&str> {
        self.0.get(id).map(String::as_str)
    }
}

fn rows_of(
    workspace: &Workspace,
    present: &[Present],
    labels: &ui::presence::ParticipantLabels,
) -> Vec<RosterRow> {
    workspace
        .members
        .iter()
        .filter_map(|member| {
            // Who is here is presence's to say: a member the workspace lists but no device here
            // speaks for is in Members, never in the roster of who is here.
            let here = present.iter().find(|here| here.member == member.id)?;
            Some(row_of(workspace, member, here, labels))
        })
        .collect()
}

fn row_of(
    workspace: &Workspace,
    member: &Member,
    here: &Present,
    labels: &ui::presence::ParticipantLabels,
) -> RosterRow {
    let is_agent = !member.id.is_person();
    let is_bot = matches!(member.id, MemberId::Bot(_));
    let (red, green, blue) = identity_rgb(member.colour);
    let label = if is_agent {
        member.name.clone()
    } else {
        labels.label(&member.id.key(), &member.name)
    };
    RosterRow {
        id: here.device.clone(),
        initial: roster_initial(&label),
        label,
        color: Color::from_rgb8(red, green, blue),
        is_agent,
        is_bot,
        role: member.role.map(crate::workspace_settings::role_label),
        // A person's row says which record they are looking at. A piece of software's does not:
        // the window it works in is named under its row, beside what it is doing there.
        activity: (!is_agent)
            .then(|| here.tab.and_then(|tab| tab_title_in(workspace, tab)))
            .flatten()
            .map(|title| crate::i18n::t_args("people-panel-viewing", &[("title", title.into())])),
        // Software works in a window of this workspace, the same tab everyone here sees: its row
        // names it, and pressing it brings that window forward.
        working_in: is_agent
            .then(|| {
                let tab = here.tab?;
                Some((tab, tab_title_in(workspace, tab)?))
            })
            .flatten(),
        working_app: is_agent
            .then(|| {
                here.app
                    .clone()
                    .or_else(|| here.tab.and_then(|tab| app_id_in(workspace, tab)))
            })
            .flatten(),
        status: here
            .status
            .as_ref()
            .map(|status| status.text.trim())
            .filter(|line| !line.is_empty())
            .map(|line| RosterStatus {
                text: line.to_owned(),
                source: if here.status.as_ref().is_some_and(|status| status.automatic) {
                    LineSource::Observed
                } else {
                    LineSource::Written
                },
            }),
        // Everyone here can be followed, software included: following an agent that has published
        // no view yet keeps its run in front as that arrives.
        recent: Vec::new(),
        followable: true,
        availability: here.availability,
        call: here
            .call
            .as_ref()
            .map(|call| call.name.clone())
            .filter(|name| !name.is_empty()),
        join: None,
    }
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
fn app_id_in(workspace: &Workspace, tab_id: u64) -> Option<String> {
    let tile = &workspace.tabs().find(|tab| tab.id == tab_id)?.tile;
    let tilo_desktop_model::TileContent::LocalApp { app_id, .. } = tile else {
        return None;
    };
    let app_id = app_id.trim();
    (!app_id.is_empty()).then(|| app_id.to_owned())
}

fn tab_title_in(workspace: &Workspace, tab_id: u64) -> Option<String> {
    workspace
        .tabs()
        .find(|tab| tab.id == tab_id)
        .map(|tab| tab.title.trim())
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
}

/// What a screen reader says about a present row beyond its name: an agent, what they are on now,
/// what they are doing.
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
