//! `DesktopNavigationV1`: the versioned per-surface navigation contract a
//! host page consumes. Routes name a workspace, strip, window group and tab
//! of the desktop model. Identities are decimal strings on the wire so a
//! full `u64` never passes through a JavaScript number.
//!
//! Wire types ported from the qOS browser runtime (PR 541,
//! `desktop_navigation.rs`) onto `tilo-desktop-model`.
use serde::{Deserialize, Serialize};
use tilo_desktop_model::{
    DesktopTree, GroupId, StripId, TabId, WindowGroup, Workspace, WorkspaceId, WorkspaceTaskStatus,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub workspace: String,
    pub strip: String,
    pub group: String,
    pub tab: String,
}

/// A route resolved against the model's id types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteIds {
    pub workspace: WorkspaceId,
    pub strip: StripId,
    pub group: GroupId,
    pub tab: TabId,
}

fn id(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|id| id.to_string() == value)
        .ok_or_else(|| "Invalid navigation identity".into())
}

impl Route {
    /// # Errors
    /// Refuses an identity that is not a canonical decimal `u64`.
    pub fn ids(&self) -> Result<RouteIds, String> {
        Ok(RouteIds {
            workspace: WorkspaceId(id(&self.workspace)?),
            strip: StripId(id(&self.strip)?),
            group: id(&self.group)?,
            tab: id(&self.tab)?,
        })
    }

    pub fn from_ids(ids: RouteIds) -> Self {
        Self {
            workspace: ids.workspace.0.to_string(),
            strip: ids.strip.0.to_string(),
            group: ids.group.to_string(),
            tab: ids.tab.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityView {
    CatchUp,
    Explore,
    Replay,
    Tasks,
}

impl ActivityView {
    /// Why a surface with `capabilities` cannot present `self`, if it
    /// cannot. Tasks is carried by the desktop model; the other views read
    /// retained record and desktop history, which only a host that installed
    /// a history source holds.
    pub fn unavailable(self, capabilities: &Capabilities) -> Option<&'static str> {
        match self {
            Self::Tasks if capabilities.workspace_tasks => None,
            Self::CatchUp | Self::Explore if capabilities.workspace_history => None,
            Self::Replay if capabilities.workspace_replay => None,
            Self::Tasks => Some("Tasks are unavailable on this desktop"),
            Self::CatchUp => {
                Some("Catch up needs retained record history, which this desktop does not carry")
            }
            Self::Explore => {
                Some("Explore needs retained record history, which this desktop does not carry")
            }
            Self::Replay => {
                Some("Replay needs retained desktop history, which this desktop does not carry")
            }
        }
    }
}

/// A per-window history move: the `target` of a `history` command.
pub type HistoryDirection = ui::gesture::NavDirection;

fn history_direction(target: &str) -> Result<HistoryDirection, String> {
    match target {
        "back" => Ok(HistoryDirection::Back),
        "forward" => Ok(HistoryDirection::Forward),
        _ => Err("Unknown history target".into()),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    Close {
        generation: String,
        route: Route,
    },
    Workspace {
        generation: String,
        workspace: String,
    },
    WorkspaceActivity {
        generation: String,
        workspace: String,
        view: ActivityView,
    },
    CloseWorkspaceActivity {
        generation: String,
        workspace: String,
    },
    NotificationCenter {
        generation: String,
        open: bool,
    },
    History {
        generation: String,
        route: Route,
        target: String,
    },
    Select {
        generation: String,
        route: Route,
    },
    Strip {
        generation: String,
        workspace: String,
        strip: String,
    },
    Pan {
        generation: String,
        workspace: String,
        strip: String,
        offset: f64,
    },
    Width {
        generation: String,
        route: Route,
        permille: u16,
    },
    Maximize {
        generation: String,
        route: Route,
        maximized: bool,
    },
    Scroll {
        generation: String,
        region: String,
        action: ScrollAction,
    },
    GlobalSearch {
        generation: String,
    },
    Commands {
        generation: String,
        query: String,
    },
    /// Open `app` (an id from the snapshot's `applications`) in a new
    /// window beside the focused one of the shown strip.
    Launch {
        generation: String,
        app: String,
    },
}

impl Command {
    pub fn generation(&self) -> &str {
        match self {
            Self::Close { generation, .. }
            | Self::Workspace { generation, .. }
            | Self::WorkspaceActivity { generation, .. }
            | Self::CloseWorkspaceActivity { generation, .. }
            | Self::NotificationCenter { generation, .. }
            | Self::Pan { generation, .. }
            | Self::History { generation, .. }
            | Self::Select { generation, .. }
            | Self::Strip { generation, .. }
            | Self::Width { generation, .. }
            | Self::Maximize { generation, .. }
            | Self::Scroll { generation, .. }
            | Self::GlobalSearch { generation }
            | Self::Commands { generation, .. }
            | Self::Launch { generation, .. } => generation,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum ScrollAction {
    Line(i8),
    Page(i8),
    Absolute(f64),
    Start,
    End,
}

impl From<ScrollAction> for ui::widget::PrimaryScrollAction {
    fn from(value: ScrollAction) -> Self {
        match value {
            ScrollAction::Line(v) => Self::Line(v),
            ScrollAction::Page(v) => Self::Page(v),
            ScrollAction::Absolute(v) => Self::Absolute(v),
            ScrollAction::Start => Self::Start,
            ScrollAction::End => Self::End,
        }
    }
}

/// A navigation command checked against the tree it will change.
#[derive(Clone, Debug, PartialEq)]
pub enum Admitted {
    Select(RouteIds),
    Close(RouteIds),
    Width(RouteIds, u16),
    Maximize(RouteIds, bool),
    Strip(WorkspaceId, StripId),
    Workspace(WorkspaceId),
    /// An application id; the host checked that it can open it.
    Launch(String),
    /// Open desktop search, with this query when one was given.
    Search(Option<String>),
    /// Pan the shown strip to a finite offset in logical px.
    Pan(StripId, f64),
    /// Scroll the primary region of the window's active tab.
    Scroll(RouteIds, ScrollAction),
    /// Move the tab's own navigation history.
    History(RouteIds, HistoryDirection),
    Activity(WorkspaceId, ActivityView),
    CloseActivity(WorkspaceId),
    /// Open or close the notification centre.
    NotificationCenter(bool),
}

/// The scroll region key of `route`'s primary region `key`.
pub fn scroll_region(route: RouteIds, key: &str) -> String {
    format!(
        "{}/{}/{}/{}/{key}",
        route.workspace.0, route.strip.0, route.group, route.tab
    )
}

fn parse_scroll_region(region: &str) -> Result<RouteIds, String> {
    let mut parts = region.splitn(5, '/');
    let mut next = || parts.next().ok_or("Scroll target is no longer active");
    let route = Route {
        workspace: next()?.into(),
        strip: next()?.into(),
        group: next()?.into(),
        tab: next()?.into(),
    };
    next()?;
    route.ids()
}

/// Which commands this surface performs without host services. The rest are
/// refused with a reason rather than silently ignored. A host that installs
/// a history source adds Catch up, Explore and Replay, and one that installs
/// notifications adds the centre.
pub const CAPABILITIES: Capabilities = Capabilities {
    select: true,
    width: true,
    maximize: true,
    strips: true,
    commands: true,
    global_search: true,
    workspace_select: true,
    workspace_history: false,
    workspace_replay: false,
    workspace_tasks: true,
    close: true,
    close_inactive: false,
    launch: true,
    pan: true,
    scroll: true,
    history: true,
    swipe: true,
    notifications: false,
};

/// Check `command` against `generation` and `tree` without changing either.
///
/// # Errors
/// Refuses a stale generation, a malformed or absent identity, or a command
/// this surface does not perform.
pub fn admit(command: &Command, generation: &str, tree: &DesktopTree) -> Result<Admitted, String> {
    admit_with(command, generation, tree, &CAPABILITIES)
}

/// [`admit`] for a surface performing `capabilities`.
///
/// # Errors
/// As [`admit`].
pub fn admit_with(
    command: &Command,
    generation: &str,
    tree: &DesktopTree,
    capabilities: &Capabilities,
) -> Result<Admitted, String> {
    if command.generation() != generation {
        return Err("Stale desktop surface".into());
    }
    let workspace = |value: &str| -> Result<WorkspaceId, String> {
        let workspace = WorkspaceId(id(value)?);
        tree.workspaces
            .iter()
            .any(|w| w.id == workspace)
            .then_some(workspace)
            .ok_or_else(|| "Workspace unavailable".into())
    };
    let active_workspace = |value: &str| active_workspace(tree, value);
    let route = |route: &Route| -> Result<RouteIds, String> {
        let ids = route.ids()?;
        locate(tree, ids)
            .map(|_| ids)
            .ok_or_else(|| "Window unavailable".into())
    };
    match command {
        Command::Select { route: r, .. } => route(r).map(Admitted::Select),
        Command::Close { route: r, .. } => route(r).map(Admitted::Close),
        Command::Width {
            route: r, permille, ..
        } => {
            let permille = (*permille).clamp(
                ui::widget::WIDTH_MIN_PERMILLE,
                ui::widget::WIDTH_MAX_PERMILLE,
            );
            route(r).map(|ids| Admitted::Width(ids, permille))
        }
        Command::Maximize {
            route: r,
            maximized,
            ..
        } => route(r).map(|ids| Admitted::Maximize(ids, *maximized)),
        Command::Workspace { workspace: w, .. } => workspace(w).map(Admitted::Workspace),
        Command::Strip {
            workspace: w,
            strip,
            ..
        } => {
            let workspace = workspace(w)?;
            let strip = StripId(id(strip)?);
            tree.workspaces
                .iter()
                .find(|w| w.id == workspace)
                .is_some_and(|w| w.strips.iter().any(|s| s.id == strip))
                .then_some(Admitted::Strip(workspace, strip))
                .ok_or_else(|| "Strip unavailable".into())
        }
        Command::WorkspaceActivity {
            workspace: w, view, ..
        } => {
            let workspace = active_workspace(w)?;
            match view.unavailable(capabilities) {
                Some(reason) => Err(reason.into()),
                None => Ok(Admitted::Activity(workspace, *view)),
            }
        }
        Command::CloseWorkspaceActivity { workspace: w, .. } => {
            active_workspace(w).map(Admitted::CloseActivity)
        }
        Command::NotificationCenter { open, .. } => admit_notifications(*open, capabilities),
        Command::History {
            route: r, target, ..
        } => {
            let direction = history_direction(target)?;
            route(r).map(|ids| Admitted::History(ids, direction))
        }
        Command::Pan {
            workspace: w,
            strip,
            offset,
            ..
        } => admit_pan(tree, w, strip, *offset),
        Command::Scroll { region, action, .. } => {
            let ids = parse_scroll_region(region)?;
            if active_route(tree) != Some(ids) {
                return Err("Scroll target is no longer active".into());
            }
            Ok(Admitted::Scroll(ids, *action))
        }
        Command::GlobalSearch { .. } => Ok(Admitted::Search(None)),
        Command::Commands { query, .. } => {
            if tilo_desktop_search::rank::query_input_is_bounded(query) {
                Ok(Admitted::Search(Some(query.clone())))
            } else {
                Err("Search query too long".into())
            }
        }
        Command::Launch { app, .. } => {
            let workspace = tree.active_workspace().ok_or("Workspace unavailable")?;
            active_strip(tree, workspace).ok_or("Strip unavailable")?;
            if app.is_empty() {
                return Err("Application unavailable".into());
            }
            Ok(Admitted::Launch(app.clone()))
        }
    }
}

fn admit_notifications(open: bool, capabilities: &Capabilities) -> Result<Admitted, String> {
    capabilities
        .notifications
        .then_some(Admitted::NotificationCenter(open))
        .ok_or_else(|| "Notifications are unavailable on this desktop".into())
}

fn active_workspace(tree: &DesktopTree, value: &str) -> Result<WorkspaceId, String> {
    let workspace = WorkspaceId(id(value)?);
    if !tree.workspaces.iter().any(|w| w.id == workspace) {
        return Err("Workspace unavailable".into());
    }
    (tree.active == workspace)
        .then_some(workspace)
        .ok_or_else(|| "Workspace is no longer active".into())
}

fn admit_pan(
    tree: &DesktopTree,
    workspace: &str,
    strip: &str,
    offset: f64,
) -> Result<Admitted, String> {
    if !offset.is_finite() {
        return Err("Invalid pan offset".into());
    }
    let workspace = active_workspace(tree, workspace)?;
    let strip = StripId(id(strip)?);
    let shown = tree
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .and_then(|w| active_strip(tree, w));
    if shown != Some(strip) {
        return Err("Strip is no longer active".into());
    }
    Ok(Admitted::Pan(strip, offset))
}

/// The group `ids` names, if the tab is still in it.
pub fn locate(tree: &DesktopTree, ids: RouteIds) -> Option<&WindowGroup> {
    tree.workspaces
        .iter()
        .find(|w| w.id == ids.workspace)?
        .strips
        .iter()
        .find(|s| s.id == ids.strip)?
        .window_groups
        .iter()
        .find(|g| g.id == ids.group)
        .filter(|g| g.tabs.iter().any(|t| t.id == ids.tab))
}

/// The strip a workspace shows: its recorded active strip, else its first.
pub fn active_strip(tree: &DesktopTree, workspace: &Workspace) -> Option<StripId> {
    tree.active_strip
        .get(&workspace.id)
        .copied()
        .filter(|id| workspace.strips.iter().any(|s| s.id == *id))
        .or_else(|| workspace.strips.first().map(|s| s.id))
}

/// The focused window of the active workspace.
pub fn active_route(tree: &DesktopTree) -> Option<RouteIds> {
    let workspace = tree.active_workspace()?;
    let strip = active_strip(tree, workspace)?;
    let group_id = tree.resolve_active_group(strip, |_| false)?;
    let group = workspace
        .strips
        .iter()
        .find(|s| s.id == strip)?
        .window_groups
        .iter()
        .find(|g| g.id == group_id)?;
    group
        .tabs
        .iter()
        .any(|t| t.id == group.active_tab)
        .then_some(RouteIds {
            workspace: workspace.id,
            strip,
            group: group.id,
            tab: group.active_tab,
        })
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the wire contract's independent capability flags"
)]
pub struct Capabilities {
    pub select: bool,
    pub width: bool,
    pub maximize: bool,
    pub strips: bool,
    pub commands: bool,
    pub global_search: bool,
    pub workspace_select: bool,
    pub workspace_history: bool,
    pub workspace_replay: bool,
    pub workspace_tasks: bool,
    pub close: bool,
    pub close_inactive: bool,
    pub launch: bool,
    pub pan: bool,
    pub scroll: bool,
    /// Per-window back and forward through the tab's own history.
    pub history: bool,
    /// The surface takes sideways wheel input itself: a trackpad swipe pans
    /// the strip under the fingers and a mouse's wheel click steps focus. A
    /// host must leave it the wheel rather than turn it into `pan` commands.
    pub swipe: bool,
    /// The surface shows notification banners and opens the notification
    /// centre on `notification-center`.
    pub notifications: bool,
}

/// The shown strip's horizontal geometry, in logical px.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DesktopPan {
    pub workspace: String,
    pub strip: String,
    pub offset: f64,
    pub viewport: f64,
    pub content: f64,
    pub max: f64,
}

/// The focused window's primary vertical scroll region, in logical px.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ScrollState {
    pub region: String,
    pub offset: f64,
    pub viewport: f64,
    pub content: f64,
    pub max: f64,
}

/// Whether the focused window's tab can move through its own history.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryState {
    pub can_back: bool,
    pub can_forward: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityCloseReason {
    Dismissed,
    WorkspaceChanged,
    /// The person opened a resource from the history; focus goes to it.
    ResourceOpened,
}

/// Presentation the surface holds beside the tree: nothing here is saved
/// or shared with peers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presentation {
    pub pan: Option<DesktopPan>,
    pub scroll: Option<ScrollState>,
    pub history: Option<HistoryState>,
    pub activity: Option<(WorkspaceId, ActivityView)>,
    pub activity_close_reason: Option<ActivityCloseReason>,
    /// The history pane the surface draws for `activity`, when it draws one.
    pub pane: Option<PaneState>,
    /// The host installed a history source: Catch up, Explore and Replay.
    pub retained_history: bool,
    /// The host installed notifications, and what they show.
    pub notifications: Option<NotificationsState>,
}

/// The history pane as the surface draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneState {
    pub playing: bool,
    /// Some referenced history could not be read.
    pub partial: bool,
}

/// What the surface's notifications show, for a host's tray control.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsState {
    /// Banners on screen.
    pub active: usize,
    /// Notifications retained in the centre.
    pub retained: usize,
    pub center_open: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskEntry {
    pub id: String,
    pub title: String,
    pub status: WorkspaceTaskStatus,
    pub status_label: String,
    pub route_label: String,
    pub created_by: String,
    pub assigned_agent: Option<String>,
    pub resource: Option<String>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ActivityState {
    pub workspace: String,
    pub view: ActivityView,
    pub playing: bool,
    pub coverage: &'static str,
    pub tasks: Vec<TaskEntry>,
    /// The surface draws this view itself (the shared history pane), so a
    /// host presents no view of its own for it.
    #[serde(rename = "inSurface")]
    pub in_surface: bool,
}

fn activity_state(
    tree: &DesktopTree,
    workspace: WorkspaceId,
    view: ActivityView,
    pane: Option<PaneState>,
) -> ActivityState {
    let mut tasks: Vec<TaskEntry> = tree
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .map(|w| w.tasks.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|task| TaskEntry {
            id: task.id.clone(),
            title: task.title.clone(),
            status: task.status,
            status_label: task.status.label().into(),
            route_label: task.route.label().into(),
            created_by: task.created_by.clone(),
            assigned_agent: task.assigned_agent.clone(),
            resource: task.resource.as_ref().map(|r| r.label.clone()),
            updated_at: task.updated_at,
        })
        .collect();
    tasks.sort_by_key(|task| {
        (
            task.status.is_finished(),
            std::cmp::Reverse(task.updated_at),
        )
    });
    ActivityState {
        workspace: workspace.0.to_string(),
        view,
        playing: pane.is_some_and(|pane| pane.playing),
        coverage: if pane.is_some_and(|pane| pane.partial) {
            "partial"
        } else {
            "complete"
        },
        tasks,
        in_surface: pane.is_some(),
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub route: Route,
    pub label: String,
    pub strip_label: String,
    pub workspace_label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct StripEntry {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub id: String,
    pub label: String,
    pub room: String,
    pub owner: Option<String>,
    pub strips: Vec<StripEntry>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Application {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: u8,
    pub generation: String,
    pub entries: Vec<Entry>,
    pub workspaces: Vec<WorkspaceEntry>,
    pub applications: Vec<Application>,
    pub active_workspace: String,
    pub active_strip: Option<String>,
    pub active: Option<Route>,
    pub width_permille: Option<u16>,
    pub maximized: bool,
    pub capabilities: Capabilities,
    pub desktop_pan: Option<DesktopPan>,
    pub workspace_activity: Option<ActivityState>,
    pub workspace_activity_close_reason: Option<ActivityCloseReason>,
    pub history: Option<HistoryState>,
    pub scroll: Option<ScrollState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifications: Option<NotificationsState>,
}

/// Project `tree` into the navigation snapshot for `generation`.
pub fn snapshot(tree: &DesktopTree, generation: &str, applications: Vec<Application>) -> Snapshot {
    snapshot_with(tree, generation, applications, &Presentation::default())
}

/// [`snapshot`] with what the surface presents beside the tree. Stale
/// presentation (another strip, workspace or window) is left out.
pub fn snapshot_with(
    tree: &DesktopTree,
    generation: &str,
    applications: Vec<Application>,
    presentation: &Presentation,
) -> Snapshot {
    let mut entries = Vec::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    entries.push(Entry {
                        route: Route::from_ids(RouteIds {
                            workspace: workspace.id,
                            strip: strip.id,
                            group: group.id,
                            tab: tab.id,
                        }),
                        label: tab.title.clone(),
                        strip_label: strip.name.clone(),
                        workspace_label: workspace.name.clone(),
                    });
                }
            }
        }
    }
    let active = active_route(tree);
    let active_group = active.and_then(|ids| locate(tree, ids));
    let shown_strip = tree
        .active_workspace()
        .and_then(|w| active_strip(tree, w))
        .map(|s| s.0.to_string());
    let desktop_pan = presentation.pan.clone().filter(|pan| {
        pan.workspace == tree.active.0.to_string() && Some(&pan.strip) == shown_strip.as_ref()
    });
    let scroll = presentation.scroll.clone().filter(|scroll| {
        active.is_some_and(|ids| parse_scroll_region(&scroll.region).ok() == Some(ids))
    });
    let history = presentation.history.filter(|_| active.is_some());
    let workspace_activity = presentation
        .activity
        .filter(|(workspace, _)| *workspace == tree.active)
        .map(|(workspace, view)| activity_state(tree, workspace, view, presentation.pane));
    Snapshot {
        version: 1,
        generation: generation.to_owned(),
        entries,
        workspaces: tree
            .workspaces
            .iter()
            .map(|w| WorkspaceEntry {
                id: w.id.0.to_string(),
                label: w.name.clone(),
                room: w.room.to_string(),
                owner: w.owner.clone(),
                strips: w
                    .strips
                    .iter()
                    .map(|s| StripEntry {
                        id: s.id.0.to_string(),
                        label: s.name.clone(),
                    })
                    .collect(),
            })
            .collect(),
        applications,
        active_workspace: tree.active.0.to_string(),
        active_strip: shown_strip,
        active: active.map(Route::from_ids),
        width_permille: active_group.map(|g| g.width.permille()),
        maximized: active_group.is_some_and(|g| g.maximized),
        capabilities: presentation.capabilities(),
        desktop_pan,
        workspace_activity,
        workspace_activity_close_reason: presentation.activity_close_reason,
        history,
        scroll,
        notifications: presentation.notifications,
    }
}

impl Presentation {
    /// [`CAPABILITIES`] plus what the host's installed services add.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            workspace_history: self.retained_history,
            workspace_replay: self.retained_history,
            notifications: self.notifications.is_some(),
            ..CAPABILITIES
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{Strip, Tab, TileContent, WindowWidth};

    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(u64::MAX), "Desk");
        let mut strip = Strip::new(StripId(9_007_199_254_740_993), "Main");
        strip.window_groups = vec![WindowGroup {
            id: 3,
            tabs: vec![Tab {
                id: 4,
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
            }],
            active_tab: 4,
            width: WindowWidth::Normal,
            maximized: false,
        }];
        workspace.strips = vec![strip];
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    #[test]
    fn route_round_trips_full_u64_and_rejects_numeric_ids() {
        let route = Route {
            workspace: u64::MAX.to_string(),
            strip: "9007199254740993".into(),
            group: "3".into(),
            tab: "4".into(),
        };
        assert_eq!(route.ids().unwrap().workspace.0, u64::MAX);
        assert_eq!(route.ids().unwrap().strip.0, 9_007_199_254_740_993);
        assert_eq!(
            serde_json::from_str::<Route>(&serde_json::to_string(&route).unwrap()).unwrap(),
            route
        );
        assert!(
            serde_json::from_str::<Route>(r#"{"workspace":1,"strip":"2","group":"3","tab":"4"}"#)
                .is_err()
        );
        assert!(
            Route {
                tab: "04".into(),
                ..route
            }
            .ids()
            .is_err()
        );
    }

    #[test]
    fn workspace_wire_commands_are_generation_bound() {
        for kind in [
            "workspace",
            "workspace-activity",
            "close-workspace-activity",
        ] {
            let mut wire = serde_json::json!({"kind":kind,"generation":"mount-1","workspace":"9007199254740993"});
            if kind == "workspace-activity" {
                wire["view"] = "catch-up".into();
            }
            let command: Command = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(command.generation(), "mount-1");
            wire["workspace"] = 1.into();
            assert!(serde_json::from_value::<Command>(wire).is_err());
        }
    }

    #[test]
    fn snapshot_names_the_active_window_and_admits_only_live_routes() {
        let tree = tree();
        let snapshot = snapshot(&tree, "g1", vec![]);
        let active = snapshot.active.clone().unwrap();
        assert_eq!(active.workspace, u64::MAX.to_string());
        assert_eq!(active.tab, "4");
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.width_permille,
            Some(WindowWidth::Normal.permille())
        );
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["activeStrip"], "9007199254740993");
        assert_eq!(json["capabilities"]["workspaceSelect"], true);

        let select = Command::Select {
            generation: "g1".into(),
            route: active.clone(),
        };
        assert!(matches!(
            admit(&select, "g1", &tree),
            Ok(Admitted::Select(_))
        ));
        assert_eq!(
            admit(&select, "g2", &tree).unwrap_err(),
            "Stale desktop surface"
        );
        let gone = Command::Close {
            generation: "g1".into(),
            route: Route {
                tab: "5".into(),
                ..active
            },
        };
        assert_eq!(admit(&gone, "g1", &tree).unwrap_err(), "Window unavailable");
        let search = Command::GlobalSearch {
            generation: "g1".into(),
        };
        assert_eq!(admit(&search, "g1", &tree), Ok(Admitted::Search(None)));
        assert!(admit(&search, "g2", &tree).is_err());
        let wire = serde_json::json!({"kind":"commands","generation":"g1","query":"sheets"});
        let commands: Command = serde_json::from_value(wire).unwrap();
        assert_eq!(
            admit(&commands, "g1", &tree),
            Ok(Admitted::Search(Some("sheets".into())))
        );
        assert_eq!(json["capabilities"]["globalSearch"], true);
    }

    fn admitted(tree: &DesktopTree, wire: serde_json::Value) -> Result<Admitted, String> {
        admit(&serde_json::from_value(wire).unwrap(), "g", tree)
    }

    #[test]
    fn pan_is_admitted_only_for_the_shown_strip_at_a_finite_offset() {
        let tree = tree();
        let (w, s) = (u64::MAX.to_string(), "9007199254740993");
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"pan","generation":"g","workspace":w,"strip":s,"offset":120.5})
            ),
            Ok(Admitted::Pan(StripId(9_007_199_254_740_993), 120.5))
        );
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"pan","generation":"g","workspace":w,"strip":"8","offset":0})
            )
            .unwrap_err(),
            "Strip is no longer active"
        );
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"pan","generation":"g","workspace":"5","strip":s,"offset":0})
            )
            .unwrap_err(),
            "Workspace unavailable"
        );
        let infinite = Command::Pan {
            generation: "g".into(),
            workspace: w,
            strip: s.into(),
            offset: f64::INFINITY,
        };
        assert_eq!(
            admit(&infinite, "g", &tree).unwrap_err(),
            "Invalid pan offset"
        );
    }

    #[test]
    fn scroll_and_history_name_a_live_window() {
        let tree = tree();
        let active = active_route(&tree).unwrap();
        let region = scroll_region(active, "primary");
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"scroll","generation":"g","region":region,"action":{"kind":"page","value":-1}})
            ),
            Ok(Admitted::Scroll(active, ScrollAction::Page(-1)))
        );
        let gone = scroll_region(RouteIds { tab: 99, ..active }, "primary");
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"scroll","generation":"g","region":gone,"action":{"kind":"end"}})
            )
            .unwrap_err(),
            "Scroll target is no longer active"
        );
        assert!(
            admitted(
                &tree,
                serde_json::json!({"kind":"scroll","generation":"g","region":"nonsense","action":{"kind":"end"}})
            )
            .is_err()
        );
        let route = Route::from_ids(active);
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"history","generation":"g","route":route,"target":"forward"})
            ),
            Ok(Admitted::History(active, HistoryDirection::Forward))
        );
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"history","generation":"g","route":route,"target":"up"})
            )
            .unwrap_err(),
            "Unknown history target"
        );
    }

    #[test]
    fn only_the_tasks_view_is_admitted_and_only_on_the_active_workspace() {
        let mut tree = tree();
        let w = u64::MAX.to_string();
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"workspace-activity","generation":"g","workspace":w,"view":"tasks"})
            ),
            Ok(Admitted::Activity(
                WorkspaceId(u64::MAX),
                ActivityView::Tasks
            ))
        );
        for view in ["catch-up", "explore", "replay"] {
            let reason = admitted(
                &tree,
                serde_json::json!({"kind":"workspace-activity","generation":"g","workspace":w,"view":view}),
            )
            .unwrap_err();
            assert!(
                reason.contains("history, which this desktop does not carry"),
                "{view}: {reason}"
            );
        }
        tree.workspaces
            .push(Workspace::new(WorkspaceId(2), "Other"));
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"workspace-activity","generation":"g","workspace":"2","view":"tasks"})
            )
            .unwrap_err(),
            "Workspace is no longer active"
        );
        assert_eq!(
            admitted(
                &tree,
                serde_json::json!({"kind":"close-workspace-activity","generation":"g","workspace":w})
            ),
            Ok(Admitted::CloseActivity(WorkspaceId(u64::MAX)))
        );
        let caps =
            serde_json::to_value(snapshot(&tree, "g", vec![])).unwrap()["capabilities"].clone();
        assert_eq!(caps["workspaceTasks"], true);
        assert_eq!(caps["swipe"], true);
        assert_eq!(caps["workspaceHistory"], false);
        assert_eq!(caps["workspaceReplay"], false);
        assert_eq!(
            (
                caps["pan"].clone(),
                caps["scroll"].clone(),
                caps["history"].clone()
            ),
            (true.into(), true.into(), true.into())
        );
    }

    #[test]
    fn a_history_source_admits_every_view_and_the_snapshot_says_who_draws_it() {
        let tree = tree();
        let w = u64::MAX.to_string();
        let presentation = Presentation {
            retained_history: true,
            notifications: Some(NotificationsState::default()),
            ..Presentation::default()
        };
        let capabilities = presentation.capabilities();
        for (wire, view) in [
            ("catch-up", ActivityView::CatchUp),
            ("explore", ActivityView::Explore),
            ("replay", ActivityView::Replay),
            ("tasks", ActivityView::Tasks),
        ] {
            let command: Command = serde_json::from_value(
                serde_json::json!({"kind":"workspace-activity","generation":"g","workspace":w,"view":wire}),
            )
            .unwrap();
            assert_eq!(
                admit_with(&command, "g", &tree, &capabilities),
                Ok(Admitted::Activity(WorkspaceId(u64::MAX), view))
            );
        }
        let center: Command = serde_json::from_value(
            serde_json::json!({"kind":"notification-center","generation":"g","open":true}),
        )
        .unwrap();
        assert_eq!(
            admit_with(&center, "g", &tree, &capabilities),
            Ok(Admitted::NotificationCenter(true))
        );
        assert_eq!(
            admit(&center, "g", &tree).unwrap_err(),
            "Notifications are unavailable on this desktop"
        );
        let shown = Presentation {
            activity: Some((WorkspaceId(u64::MAX), ActivityView::Replay)),
            pane: Some(PaneState {
                playing: true,
                partial: true,
            }),
            ..presentation
        };
        let json = serde_json::to_value(snapshot_with(&tree, "g", vec![], &shown)).unwrap();
        assert_eq!(json["capabilities"]["workspaceHistory"], true);
        assert_eq!(json["capabilities"]["workspaceReplay"], true);
        assert_eq!(json["capabilities"]["notifications"], true);
        assert_eq!(json["workspaceActivity"]["view"], "replay");
        assert_eq!(json["workspaceActivity"]["inSurface"], true);
        assert_eq!(json["workspaceActivity"]["playing"], true);
        assert_eq!(json["workspaceActivity"]["coverage"], "partial");
        assert_eq!(json["notifications"]["retained"], 0);
    }

    #[test]
    fn stale_presentation_is_left_out_of_the_snapshot() {
        let tree = tree();
        let active = active_route(&tree).unwrap();
        let presentation = Presentation {
            pan: Some(DesktopPan {
                workspace: u64::MAX.to_string(),
                strip: "1".into(),
                offset: 0.0,
                viewport: 1.0,
                content: 2.0,
                max: 1.0,
            }),
            scroll: Some(ScrollState {
                region: scroll_region(RouteIds { tab: 7, ..active }, "primary"),
                offset: 0.0,
                viewport: 1.0,
                content: 2.0,
                max: 1.0,
            }),
            history: Some(HistoryState {
                can_back: true,
                can_forward: false,
            }),
            activity: Some((WorkspaceId(2), ActivityView::Tasks)),
            ..Presentation::default()
        };
        let json = serde_json::to_value(snapshot_with(&tree, "g", vec![], &presentation)).unwrap();
        assert!(json["desktopPan"].is_null(), "another strip");
        assert!(json["scroll"].is_null(), "another window");
        assert!(json["workspaceActivity"].is_null(), "another workspace");
        assert_eq!(json["history"]["canBack"], true);
        assert_eq!(json["history"]["canForward"], false);
    }

    #[test]
    fn launch_names_an_application_on_the_shown_strip() {
        let tree = tree();
        let wire = serde_json::json!({"kind":"launch","generation":"g1","app":"so.tilo.notes"});
        let launch: Command = serde_json::from_value(wire).unwrap();
        assert_eq!(
            admit(&launch, "g1", &tree),
            Ok(Admitted::Launch("so.tilo.notes".into()))
        );
        assert_eq!(
            admit(&launch, "g2", &tree).unwrap_err(),
            "Stale desktop surface"
        );
        let empty = Command::Launch {
            generation: "g1".into(),
            app: String::new(),
        };
        assert!(admit(&empty, "g1", &tree).is_err());
        assert_eq!(
            serde_json::to_value(snapshot(&tree, "g1", vec![])).unwrap()["capabilities"]["launch"],
            true
        );
    }
}
