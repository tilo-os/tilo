//! Addressable actions for the live desktop topology.
//!
//! These rows expose tabs and window groups through the same render-free
//! action catalogue used by the command palette and control socket. Closing a
//! row commits through the durable tab lifecycle, including the compositor's
//! normal foreign-surface close request; it never synthesises pointer input.

use std::rc::Rc;

use tilo_desktop::search::{
    WINDOWS_PROVIDER_ID, WindowAction, WindowRowKind, window_rows, window_rows_revision,
    windows_provider_name,
};
use ui::search::{FieldWeight, RankField};

use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{GroupId, TabId, WorkspaceId};

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
        .map_err(|err| ActionError::Refused(err.user_message()))
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

fn contract(action: WindowAction) -> tilo_actions::ActionContract {
    match action {
        WindowAction::CloseGroup { .. } | WindowAction::CloseTab { .. } => close_contract(),
        WindowAction::MoveTab { .. } => move_contract(),
    }
}

fn invoke(chrome: &Chrome, action: WindowAction) -> Result<(), ActionError> {
    match action {
        WindowAction::CloseGroup { group } => close_group(chrome, group),
        WindowAction::CloseTab { group, tab } => close_tab(chrome, group, tab),
        WindowAction::MoveTab {
            group,
            tab,
            workspace,
        } => move_tab(chrome, group, tab, workspace),
    }
}

pub struct DesktopWindowsProvider;

static KINDS: std::sync::LazyLock<Vec<ResourceKind>> =
    std::sync::LazyLock::new(|| vec![ResourceKind::Tab, ResourceKind::WindowGroup]);

impl DesktopWindowsProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DesktopWindowsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for DesktopWindowsProvider {
    fn id(&self) -> &str {
        WINDOWS_PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Desktop Windows"
    }

    fn display_name(&self) -> String {
        windows_provider_name()
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &KINDS
    }

    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let tree = ctx.chrome.tree();
        let rows = window_rows(&tree.desktop, |fields| score(ctx.needle, fields));
        drop(tree);
        rows.into_iter()
            .map(|row| {
                let action = row.action;
                SearchResult {
                    kind: match row.kind {
                        WindowRowKind::Tab => ResourceKind::Tab,
                        WindowRowKind::WindowGroup => ResourceKind::WindowGroup,
                    },
                    operation_kind: tilo_actions::OperationKind::Effect,
                    provider_id: WINDOWS_PROVIDER_ID.into(),
                    id: row.id,
                    title: row.title,
                    subtitle: Some(row.subtitle),
                    badge: Some(row.badge),
                    score: row.score,
                    params: Vec::new(),
                    contract: Some(contract(action)),
                    on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                        invoke(chrome, action)
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                }
            })
            .collect()
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        let tree = ctx.chrome.tree();
        let revision = window_rows_revision(&tree.desktop);
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
    use crate::shell::state::{FreshId as _, Tab, TileContent, WindowGroup, WindowWidth};
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
                    WINDOWS_PROVIDER_ID,
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
                .select(&ResourceRef::new(WINDOWS_PROVIDER_ID, "tab.close:200:3"))
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
                    WINDOWS_PROVIDER_ID,
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
                    .select(&ResourceRef::new(WINDOWS_PROVIDER_ID, "tab.close:100:999"))
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
        let id = tilo_desktop::search::tab_move_id(200, 3, target);
        let catalog = {
            let state = chrome.borrow();
            ActionCatalog::build_exact(
                &registry(),
                WINDOWS_PROVIDER_ID,
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
            .select(&ResourceRef::new(WINDOWS_PROVIDER_ID, &id))
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
