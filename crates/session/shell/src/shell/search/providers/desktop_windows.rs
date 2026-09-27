//! Addressable actions for the live desktop topology.
//!
//! These rows expose tabs and window groups through the same render-free
//! action catalogue used by the command palette and control socket. Closing a
//! row commits through the durable tab lifecycle, including the compositor's
//! normal foreign-surface close request; it never synthesises pointer input.

use std::fmt::Write as _;
use std::rc::Rc;

use ui::search::{FieldWeight, RankField};

use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{GroupId, TabId, TileContent, WorkspaceId};

pub const PROVIDER_ID: &str = "desktop-windows";

/// The shared ranker over a window's names, every one of which names it.
fn score(needle: &str, fields: &[&str]) -> Option<i32> {
    let fields: Vec<RankField<'_>> = fields
        .iter()
        .map(|field| RankField::new(field, FieldWeight::TITLE))
        .collect();
    crate::shell::search::rank_fields(needle, &fields)
}

fn close_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
        risk: tilo_actions::RiskLevel::Low,
        retry: tilo_actions::RetryPolicy::Idempotent,
        tags: vec!["desktop".into(), "window".into(), "close".into()],
        ..tilo_actions::ActionContract::default()
    }
}

fn move_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
        risk: tilo_actions::RiskLevel::Low,
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        tags: vec!["desktop".into(), "window".into(), "move".into()],
        ..tilo_actions::ActionContract::default()
    }
}

fn app_id(tile: &TileContent) -> Option<&str> {
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => Some(app_id),
        TileContent::AppPlaceholder { .. }
        | TileContent::Stub { .. }
        | TileContent::WorkspaceActivity => None,
    }
}

fn tab_in_group(
    chrome: &Chrome,
    expected_group: GroupId,
    tab_id: TabId,
) -> Result<(), ActionError> {
    let state = chrome.borrow();
    let tree = state.tree();
    let Some(group) = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .find(|group| group.id == expected_group)
    else {
        return Err(ActionError::Refused(format!(
            "window group {expected_group} is no longer present"
        )));
    };
    if !group.tabs.iter().any(|tab| tab.id == tab_id) {
        return Err(ActionError::Refused(format!(
            "tab {tab_id} is no longer in window group {expected_group}"
        )));
    }
    drop(tree);
    drop(state);
    Ok(())
}

fn refused(tab_id: TabId) -> ActionError {
    ActionError::Refused(format!(
        "tab {tab_id} could not be removed from durable desktop state"
    ))
}

/// Closes through the one tab lifecycle, so the close can be undone.
fn close_tab(chrome: &Chrome, expected_group: GroupId, tab_id: TabId) -> Result<(), ActionError> {
    tab_in_group(chrome, expected_group, tab_id)?;
    crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), tab_id)
        .map_err(|_| refused(tab_id))
}

/// Moves through the one cross-workspace move, the same one a dragged
/// overview preview takes.
fn move_tab(
    chrome: &Chrome,
    expected_group: GroupId,
    tab_id: TabId,
    target: WorkspaceId,
) -> Result<(), ActionError> {
    tab_in_group(chrome, expected_group, tab_id)?;
    crate::shell::window_actions::move_tab_to_workspace(&mut chrome.borrow_mut(), tab_id, target)
        .map_err(|err| ActionError::Refused(err.user_message().to_string()))
}

/// Closes every tab and remembers the group as one close, so a single
/// reopen brings the whole group back.
fn close_group(chrome: &Chrome, group_id: GroupId) -> Result<(), ActionError> {
    let (tabs, closed) = {
        let state = chrome.borrow();
        let tree = state.tree();
        let tabs = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .find(|group| group.id == group_id)
            .map(|group| group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>())
            .ok_or_else(|| {
                ActionError::Refused(format!("window group {group_id} is no longer present"))
            })?;
        let closed = crate::shell::closed_tabs::capture_group(&tree, group_id);
        drop(tree);
        drop(state);
        (tabs, closed)
    };
    for tab in tabs {
        tab_in_group(chrome, group_id, tab)?;
        crate::shell::window_actions::close_tab_unrecorded(&mut chrome.borrow_mut(), tab)
            .map_err(|_| refused(tab))?;
    }
    if let Some(entry) = closed {
        chrome.borrow_mut().remember_closed_tab(entry);
    }
    Ok(())
}

/// One "move to workspace" row per other workspace `tab` may go to. A
/// workspace's own history and a collaborator's window stay where they are.
fn move_rows(
    needle: &str,
    workspaces: &[crate::shell::state::Workspace],
    workspace: &crate::shell::state::Workspace,
    group_id: GroupId,
    tab: &crate::shell::state::Tab,
) -> Vec<SearchResult> {
    if matches!(
        tab.tile,
        TileContent::WorkspaceActivity | TileContent::PeerWindow { .. }
    ) {
        return Vec::new();
    }
    let tab_id = tab.id;
    let title = &tab.title;
    let app = app_id(&tab.tile).unwrap_or("native");
    let mut rows = Vec::new();
    for other in workspaces.iter().filter(|other| other.id != workspace.id) {
        let target = other.id;
        let id = DesktopWindowsProvider::tab_move_id(group_id, tab_id, target);
        let Some(row_score) = score(needle, &[title, app, &workspace.name, &other.name, &id])
        else {
            continue;
        };
        rows.push(SearchResult {
            kind: ResourceKind::Custom("Tabs".into()),
            operation_kind: tilo_actions::OperationKind::Effect,
            provider_id: PROVIDER_ID.into(),
            id,
            title: format!("Move tab “{title}” to workspace “{}”", other.name),
            subtitle: Some(format!(
                "App {app} · from workspace “{}” · group {group_id} · tab {tab_id}",
                workspace.name
            )),
            badge: Some("Tab".into()),
            score: row_score,
            params: Vec::new(),
            contract: Some(move_contract()),
            on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                move_tab(chrome, group_id, tab_id, target)
            }),
            on_invoke: None,
            on_begin_invoke: None,
        });
    }
    rows
}

pub struct DesktopWindowsProvider;

static KINDS: std::sync::LazyLock<Vec<ResourceKind>> = std::sync::LazyLock::new(|| {
    vec![
        ResourceKind::Custom("Tabs".into()),
        ResourceKind::Custom("Window groups".into()),
    ]
});

impl DesktopWindowsProvider {
    pub fn new() -> Self {
        Self
    }

    pub fn tab_close_id(group: GroupId, tab: TabId) -> String {
        format!("tab.close:{group}:{tab}")
    }

    pub fn group_close_id(group: GroupId) -> String {
        format!("group.close:{group}")
    }

    pub fn tab_move_id(group: GroupId, tab: TabId, workspace: WorkspaceId) -> String {
        format!("tab.move:{group}:{tab}:{}", workspace.raw())
    }
}

impl Default for DesktopWindowsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for DesktopWindowsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Desktop Windows"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &KINDS
    }

    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let tree = ctx.chrome.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                let group_id = group.id;
                let group_title = group
                    .tabs
                    .iter()
                    .find(|tab| tab.id == group.active_tab)
                    .or_else(|| group.tabs.first())
                    .map_or_else(
                        || format!("Window group {group_id}"),
                        |tab| tab.title.clone(),
                    );
                let group_id_string = Self::group_close_id(group_id);
                if let Some(row_score) = score(
                    ctx.needle,
                    &[&group_title, &workspace.name, &strip.name, &group_id_string],
                ) {
                    rows.push(SearchResult {
                        kind: ResourceKind::Custom("Window groups".into()),
                        operation_kind: tilo_actions::OperationKind::Effect,
                        provider_id: PROVIDER_ID.into(),
                        id: group_id_string,
                        title: format!("Close window group “{group_title}”"),
                        subtitle: Some(format!(
                            "Workspace “{}” · strip “{}” · group {} · {} tabs",
                            workspace.name,
                            strip.name,
                            group_id,
                            group.tabs.len()
                        )),
                        badge: Some("Window group".into()),
                        score: row_score,
                        params: Vec::new(),
                        contract: Some(close_contract()),
                        on_open: Rc::new(
                            move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                                close_group(chrome, group_id)
                            },
                        ),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                }
                for tab in &group.tabs {
                    let tab_id = tab.id;
                    let title = tab.title.clone();
                    let app = app_id(&tab.tile).unwrap_or("native");
                    let id = Self::tab_close_id(group_id, tab_id);
                    if let Some(row_score) = score(
                        ctx.needle,
                        &[&title, app, &workspace.name, &strip.name, &id],
                    ) {
                        rows.push(SearchResult {
                        kind: ResourceKind::Custom("Tabs".into()),
                        operation_kind: tilo_actions::OperationKind::Effect,
                        provider_id: PROVIDER_ID.into(),
                        id,
                        title: format!("Close tab “{title}”"),
                        subtitle: Some(format!(
                            "App {app} · workspace “{}” · strip “{}” · group {group_id} · tab {tab_id}",
                            workspace.name, strip.name
                        )),
                        badge: Some("Tab".into()),
                        score: row_score,
                        params: Vec::new(),
                        contract: Some(close_contract()),
                        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                            close_tab(chrome, group_id, tab_id)
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                    }
                    rows.extend(move_rows(
                        ctx.needle,
                        &tree.desktop.workspaces,
                        workspace,
                        group_id,
                        tab,
                    ));
                }
            }
        }
        drop(tree);
        rows
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        let tree = ctx.chrome.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return "desktop-windows:empty".into();
        };
        let mut revision = format!("desktop-windows:{}", workspace.id.raw());
        // Every other workspace is somewhere a tab can move to.
        for other in &tree.desktop.workspaces {
            let _ = write!(revision, ":w{}:{}", other.id.raw(), other.name);
        }
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                let _ = write!(revision, ":g{}", group.id);
                for tab in &group.tabs {
                    let _ = write!(revision, ":t{}", tab.id);
                }
            }
        }
        drop(tree);
        revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use crate::shell::state::{FreshId as _, Tab, WindowGroup, WindowWidth};
    use automerge::{AutoCommit, ROOT};

    fn fixture() -> (Chrome, AutoCommit) {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].strips[0].window_groups = vec![
            WindowGroup {
                id: 100,
                tabs: vec![
                    Tab {
                        id: 1,
                        title: "Profilfehler aufgetreten".into(),
                        tile: TileContent::Foreign {
                            app_id: "com.google.Chrome".into(),
                            surface_id: 501,
                        },
                    },
                    Tab {
                        id: 2,
                        title: "ChatGPT".into(),
                        tile: TileContent::Foreign {
                            app_id: "com.openai.ChatGPT".into(),
                            surface_id: 502,
                        },
                    },
                ],
                active_tab: 2,
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: 200,
                tabs: vec![Tab {
                    id: 3,
                    title: "Profilfehler aufgetreten".into(),
                    tile: TileContent::Foreign {
                        app_id: "com.google.Chrome".into(),
                        surface_id: 503,
                    },
                }],
                active_tab: 3,
                width: WindowWidth::Normal,
                maximized: false,
            },
        ];
        tree.seed_doc_from_cache();
        (ChromeState::new(tree), AutoCommit::new())
    }

    fn registry() -> SearchRegistry {
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(DesktopWindowsProvider::new()));
        registry
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn desktop_windows_are_exactly_addressed_and_closed() {
        // Phase `enumerates_exact_duplicate_titled_tabs_and_groups`.
        {
            let (chrome, doc) = fixture();
            let state = chrome.borrow();
            let catalog = ActionCatalog::build(
                &registry(),
                "Profilfehler",
                SearchScope::All,
                None,
                &state,
                &doc,
                &ROOT,
                &AllVisible,
            );
            let actions = catalog.enumerate();
            let ids = actions
                .iter()
                .map(|action| action.resource.id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                ["tab.close:100:1", "tab.close:200:3", "group.close:200"]
            );
            assert!(actions[..2].iter().all(|action| {
                action.title.contains("Profilfehler")
                    && action
                        .subtitle
                        .as_deref()
                        .is_some_and(|text| text.contains("group"))
            }));
            assert_eq!(
                actions[2].title,
                "Close window group “Profilfehler aufgetreten”"
            );
        }

        // Phase `exact_close_removes_only_target_and_queues_its_protocol_surface`.
        {
            let (chrome, mut doc) = fixture();
            let catalog = {
                let state = chrome.borrow();
                ActionCatalog::build_exact(
                    &registry(),
                    PROVIDER_ID,
                    "tab.close:200:3",
                    SearchScope::All,
                    None,
                    &state,
                    &doc,
                    &ROOT,
                    &AllVisible,
                )
            };
            catalog
                .select(&ResourceRef::new(PROVIDER_ID, "tab.close:200:3"))
                .expect("exact live tab action")
                .invoke(&mut doc, &ROOT, &chrome, [])
                .expect("close accepted");

            let mut state = chrome.borrow_mut();
            let tree = state.tree();
            let ids = tree.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
                .collect::<Vec<_>>();
            assert_eq!(ids, [1, 2], "the same-title tab and ChatGPT stay open");
            drop(tree);
            assert_eq!(state.take_surface_close_requests(), [503]);
        }

        // Phase `stale_exact_address_refuses_without_closing_another_window`.
        {
            let (chrome, mut doc) = fixture();
            let catalog = {
                let state = chrome.borrow();
                ActionCatalog::build_exact(
                    &registry(),
                    PROVIDER_ID,
                    "tab.close:100:999",
                    SearchScope::All,
                    None,
                    &state,
                    &doc,
                    &ROOT,
                    &AllVisible,
                )
            };
            assert!(
                catalog
                    .select(&ResourceRef::new(PROVIDER_ID, "tab.close:100:999"))
                    .is_none()
            );
            assert_eq!(
                chrome.borrow().tree().desktop.workspaces[0].strips[0]
                    .window_groups
                    .len(),
                2
            );
            assert!(chrome.borrow_mut().take_surface_close_requests().is_empty());
            let _ = &mut doc;
        }
    }

    #[test]
    fn a_tab_moves_to_another_workspace_by_its_exact_action() {
        let (chrome, mut doc) = fixture();
        let target = {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let id = crate::shell::state::WorkspaceId::new();
            let mut other = crate::shell::state::Workspace::new(id, "Elsewhere");
            other.strips.push(crate::shell::state::Strip::new(
                crate::shell::state::StripId::new(),
                "Main",
            ));
            tree.desktop.workspaces.push(other);
            tree.seed_doc_from_cache();
            id
        };
        let id = DesktopWindowsProvider::tab_move_id(200, 3, target);
        let catalog = {
            let state = chrome.borrow();
            ActionCatalog::build_exact(
                &registry(),
                PROVIDER_ID,
                &id,
                SearchScope::All,
                None,
                &state,
                &doc,
                &ROOT,
                &AllVisible,
            )
        };
        let action = catalog
            .select(&ResourceRef::new(PROVIDER_ID, &id))
            .expect("every tab can move to every other workspace");
        assert_eq!(
            action.title(),
            "Move tab “Profilfehler aufgetreten” to workspace “Elsewhere”"
        );
        action
            .invoke(&mut doc, &ROOT, &chrome, [])
            .expect("move accepted");

        let mut state = chrome.borrow_mut();
        let tree = state.tree();
        let home: Vec<TabId> = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
            .collect();
        assert_eq!(home, [1, 2]);
        let moved = &tree.desktop.workspaces[1].strips[0].window_groups[0].tabs[0];
        assert_eq!(moved.id, 3);
        assert!(
            matches!(&moved.tile, TileContent::LocalApp { note, .. } if note.is_empty()),
            "the client relaunches in the destination rather than crossing sandboxes"
        );
        drop(tree);
        assert_eq!(state.take_surface_close_requests(), [503]);
    }
}
