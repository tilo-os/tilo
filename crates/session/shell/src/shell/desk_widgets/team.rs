//! The Team widget's model: the people in this workspace, what each says
//! they are doing, and where they are.
//!
//! A person's status line is theirs. Two kinds reach this machine through
//! presence ([`ui::presence::PresenceCtx::status_for`]) and are marked apart
//! ([`LineSource`]):
//!
//! - a line they wrote, shown as written;
//! - a line their own machine generated from their recent actions
//!   ([`ui::StatusOrigin::Inferred`], produced by the local status narrator
//!   on *their* desktop and shared only when they turned automatic status
//!   on). This widget never generates a line for someone else; it shows
//!   what their machine published, marked as generated.
//!
//! [`status_line`] is the one reading of that; the People popover uses it
//! too.

use std::time::SystemTime;

use tilo_desktop::collaborators::{LineSource, RosterRow, RosterStatus};
use tilo_desktop_model::DesktopTree;
use ui::presence::PresenceCtx;

/// Most people the open card lists before "and N more".
pub const MAX_ROWS: usize = 6;

/// One person on the Team card.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamMember {
    pub row: RosterRow,
    /// Where they are: "Research · Notes" (the strip, then the window they
    /// are on), or `None` when they are in the workspace but on nothing.
    pub place: Option<String>,
}

/// The people in the active workspace other than this person, people
/// before agents, each with their status line and place.
#[must_use]
pub fn members(
    desktop: &DesktopTree,
    local_user: &str,
    own_id: &str,
    presence: &PresenceCtx,
    now: SystemTime,
) -> Vec<TeamMember> {
    let mut rows: Vec<TeamMember> =
        tilo_desktop::collaborators::collaborator_roster_rows(desktop, local_user)
            .into_iter()
            .filter(|row| !row.invited && row.id != own_id && row.id != local_user)
            .map(|mut row| {
                row.status = status_line(presence, &row, now);
                // Idle or away dims their face; a call rings it, as
                // everywhere else their face shows.
                if let Some(id) = presence_id(presence, &row) {
                    row.availability = presence.availability_of(&id);
                    row.call = presence.call_of(&id).map(str::to_owned);
                }
                let place = place_of(desktop, &row.id);
                TeamMember { row, place }
            })
            .collect();
    rows.sort_by_key(|member| member.row.is_agent);
    rows
}

/// What `row`'s person says they are doing, marked by who wrote it: the
/// status their presence carries under their id, or under a presence name
/// matching their label.
#[must_use]
pub fn status_line(
    presence: &PresenceCtx,
    row: &RosterRow,
    now: SystemTime,
) -> Option<RosterStatus> {
    let direct = ui::permissions::UserId(row.id.clone());
    let id = presence
        .status_for(&direct, now)
        .map(|_| direct.clone())
        .or_else(|| {
            presence
                .names
                .iter()
                .find_map(|(id, name)| name.eq_ignore_ascii_case(&row.label).then(|| id.clone()))
        })?;
    presence.status_for(&id, now).map(|status| RosterStatus {
        text: status.text.clone(),
        source: if matches!(status.origin, ui::StatusOrigin::Inferred { .. }) {
            LineSource::Generated
        } else {
            LineSource::Written
        },
    })
}

/// Who `row` is to live presence: their own id when presence knows it,
/// else the one presence names as they are named.
#[must_use]
pub fn presence_id(presence: &PresenceCtx, row: &RosterRow) -> Option<ui::permissions::UserId> {
    let direct = ui::permissions::UserId(row.id.clone());
    if presence.names.contains_key(&direct) || presence.has_availability(&direct) {
        return Some(direct);
    }
    presence
        .names
        .iter()
        .find_map(|(id, name)| name.eq_ignore_ascii_case(&row.label).then(|| id.clone()))
}

/// The strip and window `participant_id` is on in the active workspace.
#[must_use]
pub fn place_of(desktop: &DesktopTree, participant_id: &str) -> Option<String> {
    let workspace = desktop.active_workspace()?;
    let tab_id = workspace
        .collaborators
        .iter()
        .chain(&desktop.collaborators)
        .find(|participant| participant.id == participant_id)?
        .active_tab?;
    workspace.strips.iter().find_map(|strip| {
        let tab = strip
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .find(|tab| tab.id == tab_id)?;
        let title = tab.title.trim();
        let strip_name = strip.name.trim();
        Some(match (strip_name.is_empty(), title.is_empty()) {
            (false, false) => crate::shell::i18n::t_args(
                "desk-team-place",
                &[("strip", strip_name.into()), ("window", title.into())],
            ),
            (false, true) => strip_name.to_string(),
            (true, _) => title.to_string(),
        })
    })
}
