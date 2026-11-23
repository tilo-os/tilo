//! Who the shell says is here: the roster's rows and the names it gives people.
//!
//! A row is one member of the active workspace — a person, an agent or a bot — with the colour
//! their member row wears, said beside where presence puts them and what they say they are doing.
//! The overlay paints what this derives; nothing here measures or paints.
//!
//! A person nobody named is told apart by when they joined, which is the one fact a row about
//! them can carry that is theirs alone: their name and face are every unnamed person's.

use std::collections::HashMap;

use collab_types::presence::identity_rgb;
use collab_types::{Member, MemberId};
use tilo_desktop_model::{Layout, Present, Workspace};
use ui::vello::peniko::Color;

use crate::desktop::i18n::t_args;

use super::{LineSource, RosterRow, RosterStatus};

/// Every member of the active workspace who is here now, as the roster lists them.
pub fn roster_rows(desktop: &Layout) -> Vec<RosterRow> {
    let Some(workspace) = desktop.active_workspace() else {
        return Vec::new();
    };
    let mut rows = rows_of(workspace, &workspace.present);
    name_the_unnamed(&mut rows);
    rows
}

/// Give each row nobody named the clock they joined by. Two people admitted within one second
/// would read alike otherwise, so those two say the millisecond as well: every unnamed row is a
/// person someone can tell apart and recognise, never a device's name or a key.
fn name_the_unnamed(rows: &mut [RosterRow]) {
    let joined: Vec<(usize, u64)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| ui::presence::is_unnamed_person(&row.label))
        .filter_map(|(index, row)| Some((index, row.joined?)))
        .collect();
    let seconds: Vec<String> = joined
        .iter()
        .map(|(_, at)| clock(*at, ui::format::time_of_with_seconds))
        .collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
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
        if let Some(row) = rows.get_mut(*index)
            && !shown.trim().is_empty()
        {
            row.joined_line = Some(t_args("people-panel-joined", &[("time", shown.into())]));
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

/// What each member in the active workspace is called, exactly as the roster calls them, with this
/// person by their own name: the one source every surface that
/// names people reads, so a name is spelt and cased the same in People, the roster and settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeopleNames(HashMap<MemberId, String>);

impl PeopleNames {
    /// The roster's names for the active workspace of `desktop`, as `local_user` (named
    /// `own_name`) sees it.
    #[must_use]
    pub fn of(desktop: &Layout, local_user: &str, own_name: Option<&str>) -> Self {
        let Some(workspace) = desktop.active_workspace() else {
            return Self::default();
        };
        let mut names: HashMap<MemberId, String> = workspace
            .members
            .iter()
            .filter(|member| !member.name.trim().is_empty())
            .map(|member| (member.id.clone(), member.name.clone()))
            .collect();
        if let Some(own) = own_name.map(str::trim).filter(|own| !own.is_empty()) {
            names.insert(MemberId::person(local_user), own.to_owned());
        }
        Self(names)
    }

    /// What the member `id` is called, when the roster names them.
    #[must_use]
    pub fn get(&self, id: &MemberId) -> Option<&str> {
        self.0.get(id).map(String::as_str)
    }
}

fn rows_of(workspace: &Workspace, present: &[Present]) -> Vec<RosterRow> {
    workspace
        .members
        .iter()
        .filter_map(|member| {
            // Who is here is presence's to say: a member the workspace lists but no device here
            // speaks for is in Members, never in the roster of who is here.
            let here = present.iter().find(|here| here.member == member.id)?;
            Some(row_of(workspace, member, here))
        })
        .collect()
}

fn row_of(workspace: &Workspace, member: &Member, here: &Present) -> RosterRow {
    let is_agent = !member.id.is_person();
    let (red, green, blue) = identity_rgb(member.colour);
    let label = member.name.clone();
    RosterRow {
        id: here.device.clone(),
        initial: roster_initial(&label),
        label,
        color: Color::from_rgb8(red, green, blue),
        member: member.id.clone(),
        role: member
            .role
            .map(crate::desktop::workspace_settings::role_label),
        // A person's row says which record they are looking at. A piece of software's does not:
        // the window it works in is named under its row, beside what it is doing there.
        activity: (!is_agent)
            .then(|| here.tab.and_then(|tab| tab_title_in(workspace, tab)))
            .flatten()
            .map(|title| {
                crate::desktop::i18n::t_args("people-panel-viewing", &[("title", title.into())])
            }),
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
        // When the workspace admitted them: what a row nobody named is told apart by.
        joined: member
            .id
            .person_id()
            .and_then(|person| workspace.policy.joined_at(person)),
        joined_line: None,
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
    crate::desktop::i18n::t_args("people-panel-in-app", &[("app", app.to_owned().into())])
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

/// What the window `tab_id` shows, by the one rule every surface names a window by: its title, or
/// the app's name where the title only names the app.
fn tab_title_in(workspace: &Workspace, tab_id: u64) -> Option<String> {
    let tab = workspace.tabs().find(|tab| tab.id == tab_id)?;
    let name = crate::desktop::state::window_title(tab);
    (!name.trim().is_empty()).then_some(name)
}

/// What a screen reader says about a present row beyond its name: an agent, what they are on now,
/// what they are doing.
#[must_use]
pub fn spoken_details(row: &RosterRow) -> String {
    let mut details = Vec::new();
    if row.is_agent() {
        details.push(crate::desktop::i18n::t("a11y-shell-people-agent"));
    }
    if let Some(activity) = &row.activity {
        details.push(t_args(
            "a11y-shell-people-now",
            &[("activity", activity.into())],
        ));
    }
    if let Some(doing) = super::doing_line(row) {
        details.push(doing);
    }
    // Someone nobody named is told apart by when they joined, and a reader is told it too.
    if let Some(joined) = super::joined_line(row) {
        details.push(joined);
    }
    // Where software works: the line under its row says it on screen, and a reader is told it too.
    if let Some((_, record)) = &row.working_in {
        details.push(in_app(record));
    }
    details.join("; ")
}
