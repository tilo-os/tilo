//! "Happening now", the launcher's first section: who is in which of your
//! workspaces, what they are in, and the calls you could join.
//!
//! One row per workspace with someone in it, wearing their faces, and under
//! it a row for each window someone is in (Enter goes there) and each call
//! in progress (Enter joins it). It reads the presence the strip, the window
//! bar and the overview read: each workspace's collaborators as their
//! presence last said, through [`tilo_desktop::collaborators`]'s roster rows,
//! and in each the calls the people panel offers to join, as that
//! workspace's people say there ([`ChromeState::live_calls`], set by the
//! overlay host from the same presence). Scoped to
//! the current workspace, it lists that one; widened, every workspace.

use std::rc::Rc;

use tilo_desktop::collaborators::{RosterRow, VoiceChannel};
use tilo_desktop_model::{DesktopTree, GroupId, StripId, Workspace, WorkspaceId};
use ui::widget::command_palette::RowFace;

use crate::shell::chrome_state::{Chrome, ChromeState, PresenceRequest, SearchScope};
use crate::shell::i18n::t_args;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchResult};

/// Row ids: a workspace, a window in one, a call.
const WORKSPACE_PREFIX: &str = "workspace:";
const WINDOW_PREFIX: &str = "window:";
const CALL_PREFIX: &str = "call:";

/// A call in progress in a workspace, that this device is not in: the
/// workspace, its channel, its name and who is in it.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveCall {
    pub workspace: WorkspaceId,
    pub channel: VoiceChannel,
    pub name: String,
    pub people: Vec<RosterRow>,
}

/// A person as a face in a row.
fn face(row: &RosterRow) -> RowFace {
    RowFace {
        name: row.label.clone(),
        identity: row.id.clone(),
        colour: row.color,
        is_agent: row.is_agent,
        dimmed: !matches!(row.availability, ui::Availability::Available),
    }
}

/// "Morgan is here", "Morgan and Alex are here", "Morgan and 3 others are
/// here".
fn here_line(people: &[RosterRow]) -> String {
    names_line(people, "search-launcher-here")
}

/// "With Morgan", "With Morgan and Alex", "With Morgan and 3 others".
fn with_line(people: &[RosterRow]) -> String {
    names_line(people, "search-launcher-with")
}

fn names_line(people: &[RosterRow], key: &str) -> String {
    let name = |at: usize| people.get(at).map_or("", |row| row.label.as_str());
    match people.len() {
        0 | 1 => t_args(&format!("{key}-one"), &[("name", name(0).into())]),
        2 => t_args(
            &format!("{key}-two"),
            &[("name", name(0).into()), ("other", name(1).into())],
        ),
        n => t_args(
            &format!("{key}-many"),
            &[
                ("name", name(0).into()),
                ("count", n.saturating_sub(1).into()),
            ],
        ),
    }
}

/// The others present in `workspace`, people before agents.
fn present(
    desktop: &DesktopTree,
    workspace: &Workspace,
    local_user: &str,
    own_id: &str,
) -> Vec<RosterRow> {
    let rows = if workspace.id == desktop.active {
        tilo_desktop::collaborators::collaborator_roster_rows(desktop, local_user)
    } else {
        tilo_desktop::collaborators::workspace_roster_rows(workspace, local_user)
    };
    let mut rows: Vec<RosterRow> = rows
        .into_iter()
        .filter(|row| !row.invited && row.id != local_user && row.id != own_id)
        .collect();
    rows.sort_by_key(|row| row.is_agent);
    rows
}

/// The tab a collaborator's presence names, when it is a window of
/// `workspace`.
fn tab_of(workspace: &Workspace, desktop: &DesktopTree, id: &str) -> Option<u64> {
    let tab = workspace
        .collaborators
        .iter()
        .chain(&desktop.collaborators)
        .find(|participant| participant.id == id)?
        .active_tab?;
    workspace
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .any(|candidate| candidate.id == tab)
        .then_some(tab)
}

/// Where tab `tab` of `workspace` is, and what it is called.
fn locate(workspace: &Workspace, tab: u64) -> Option<(StripId, GroupId, String)> {
    workspace.strips.iter().find_map(|strip| {
        strip.window_groups.iter().find_map(|group| {
            group
                .tabs
                .iter()
                .find(|candidate| candidate.id == tab)
                .map(|candidate| (strip.id, group.id, candidate.title.trim().to_owned()))
        })
    })
}

/// A launcher row and the faces it wears.
pub type FacedRow = (SearchResult, Vec<RowFace>);

fn row(id: String, title: String, reason: String, people: &[RosterRow]) -> FacedRow {
    let row = SearchResult {
        facts: Vec::new(),
        kind: ResourceKind::Custom(crate::shell::i18n::t("search-kind-people")),
        operation_kind: tilo_actions::OperationKind::View,
        provider_id: super::people::PROVIDER_ID.into(),
        id,
        title,
        subtitle: Some(reason),
        badge: None,
        shortcut: None,
        score: 0,
        params: Vec::new(),
        contract: Some(tilo_actions::ActionContract::view()),
        on_open: Rc::new(|_doc, _root, _chrome: &Chrome, _args: &ActionArgs| Ok(())),
        on_invoke: None,
        on_begin_invoke: None,
    };
    (row, people.iter().map(face).collect())
}

/// The rows of Happening now for `state` in `scope`, workspace by workspace:
/// the one in front first, then the others in their order.
pub fn rows(state: &ChromeState, scope: SearchScope, calls: &[LiveCall]) -> Vec<FacedRow> {
    let local_user = state.active_local_user();
    let own_id = state.local_user_id.clone();
    let tree = state.tree();
    let desktop = &tree.desktop;
    let mut workspaces: Vec<&Workspace> = desktop
        .workspaces
        .iter()
        .filter(|workspace| scope == SearchScope::All || workspace.id == desktop.active)
        .collect();
    workspaces.sort_by_key(|workspace| workspace.id != desktop.active);
    let mut out = Vec::new();
    for workspace in workspaces {
        let people = present(desktop, workspace, &local_user, &own_id);
        let calls: Vec<&LiveCall> = calls
            .iter()
            .filter(|call| call.workspace == workspace.id)
            .collect();
        if people.is_empty() && calls.is_empty() {
            continue;
        }
        out.extend(workspace_rows(desktop, workspace, &people, &calls));
    }
    drop(tree);
    out
}

fn workspace_rows(
    desktop: &DesktopTree,
    workspace: &Workspace,
    people: &[RosterRow],
    calls: &[&LiveCall],
) -> Vec<FacedRow> {
    let ws = workspace.id;
    let mut out = Vec::new();
    let mut header = row(
        format!("{WORKSPACE_PREFIX}{}", ws.0),
        workspace.name.clone(),
        here_line(people),
        people,
    );
    let here = ws == desktop.active;
    header.0.on_open = Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
        let mut state = chrome.borrow_mut();
        if here {
            // Who is here, in the people panel.
            state.overlays.collaborators = true;
            state.bump();
        } else if !state.switch_to_workspace(ws) {
            return Err(ActionError::Refused("the workspace is gone".into()));
        }
        Ok(())
    });
    out.push(header);
    // A window per row, in the order its first person is listed.
    let mut windows: Vec<(u64, Vec<RosterRow>)> = Vec::new();
    for person in people {
        let Some(tab) = tab_of(workspace, desktop, &person.id) else {
            continue;
        };
        match windows.iter_mut().find(|(at, _)| *at == tab) {
            Some((_, with)) => with.push(person.clone()),
            None => windows.push((tab, vec![person.clone()])),
        }
    }
    for (tab, with) in windows {
        let Some((strip, group, title)) = locate(workspace, tab) else {
            continue;
        };
        if title.is_empty() {
            continue;
        }
        let mut item = row(
            format!("{WINDOW_PREFIX}{}:{tab}", ws.0),
            title,
            with_line(&with),
            &with,
        );
        item.0.on_open = Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
            chrome.borrow_mut().select_tab(ws, strip, group, tab);
            Ok(())
        });
        out.push(item);
    }
    for call in calls {
        let title = if call.name.trim().is_empty() {
            crate::shell::i18n::t("search-launcher-call")
        } else {
            t_args(
                "search-launcher-call-in",
                &[("channel", call.name.trim().into())],
            )
        };
        let mut item = row(
            format!("{CALL_PREFIX}{}", call.channel.channel),
            title,
            with_line(&call.people),
            &call.people,
        );
        item.0.operation_kind = tilo_actions::OperationKind::Effect;
        item.0.contract = None;
        let channel = call.channel.clone();
        item.0.on_open = Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
            let mut state = chrome.borrow_mut();
            state.push_presence_request(PresenceRequest::JoinCall(channel.clone()));
            state.bump();
            Ok(())
        });
        out.push(item);
    }
    out
}

/// What choosing the Happening now row `id` does: going to a workspace
/// opens it, a window someone is in or a call is joined.
pub fn outcome(id: &str) -> Option<String> {
    if id.starts_with(WORKSPACE_PREFIX) {
        Some(crate::shell::i18n::t("search-launcher-outcome-open"))
    } else if id.starts_with(WINDOW_PREFIX) || id.starts_with(CALL_PREFIX) {
        Some(crate::shell::i18n::t("search-launcher-outcome-join"))
    } else {
        None
    }
}

/// What the Happening now row `id` wears at its start: the people in a
/// workspace, a window, or a call.
pub fn icon(id: &str) -> Option<ui::icons::Icon> {
    if id.starts_with(WORKSPACE_PREFIX) {
        Some(ui::icons::Icon::Users)
    } else if id.starts_with(WINDOW_PREFIX) {
        Some(ui::icons::Icon::LayoutGrid)
    } else if id.starts_with(CALL_PREFIX) {
        Some(ui::icons::Icon::Phone)
    } else {
        None
    }
}

/// Whether `id` names a Happening now row.
pub fn is_row_id(id: &str) -> bool {
    [WORKSPACE_PREFIX, WINDOW_PREFIX, CALL_PREFIX]
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

/// The workspace a Happening now row is about, when it names one.
pub fn workspace_of(id: &str) -> Option<WorkspaceId> {
    let rest = id
        .strip_prefix(WORKSPACE_PREFIX)
        .or_else(|| id.strip_prefix(WINDOW_PREFIX))?;
    rest.split(':').next()?.parse::<u64>().ok().map(WorkspaceId)
}
