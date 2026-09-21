//! Addressable actions for the live desktop topology.
//!
//! These rows expose tabs and window groups through the same render-free
//! action catalogue used by the command palette and control socket. Closing a
//! row commits through the durable tab lifecycle, including the compositor's
//! normal foreign-surface close request; it never synthesises pointer input.

use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{GroupId, TabId, TileContent};

pub const PROVIDER_ID: &str = "desktop-windows";

fn score(needle: &str, fields: &[&str]) -> Option<i32> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return Some(0);
    }
    fields.iter().find_map(|field| {
        let field = field.to_lowercase();
        if field == needle {
            Some(100)
        } else if field.starts_with(&needle) {
            Some(90)
        } else if field.contains(&needle) {
            Some(60)
        } else {
            None
        }
    })
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

fn app_id(tile: &TileContent) -> Option<&str> {
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => Some(app_id),
        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. } => None,
    }
}

fn close_tab(chrome: &Chrome, expected_group: GroupId, tab_id: TabId) -> Result<(), ActionError> {
    let mut state = chrome.borrow_mut();
    let surface = {
        let tree = state.tree();
        let Some(group) = tree
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
        let Some(tab) = group.tabs.iter().find(|tab| tab.id == tab_id) else {
            return Err(ActionError::Refused(format!(
                "tab {tab_id} is no longer in window group {expected_group}"
            )));
        };
        match &tab.tile {
            TileContent::Foreign { surface_id, .. } => Some(*surface_id),
            _ => None,
        }
    };
    if !crate::shell::tab_binding::remove_tab(&mut state.tree_mut(), tab_id) {
        return Err(ActionError::Refused(format!(
            "tab {tab_id} could not be removed from durable desktop state"
        )));
    }
    state.tree().flush_to_disk();
    if let Some(surface) = surface {
        state.request_surface_close(surface);
    }
    state.bump();
    Ok(())
}

fn close_group(chrome: &Chrome, group_id: GroupId) -> Result<(), ActionError> {
    let tabs = {
        let state = chrome.borrow();
        let tree = state.tree();
        tree.workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .find(|group| group.id == group_id)
            .map(|group| group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>())
            .ok_or_else(|| {
                ActionError::Refused(format!("window group {group_id} is no longer present"))
            })?
    };
    for tab in tabs {
        close_tab(chrome, group_id, tab)?;
    }
    Ok(())
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

    fn name(&self) -> &str {
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
        let Some(workspace) = tree.active_workspace() else {
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
                    let Some(row_score) = score(
                        ctx.needle,
                        &[&title, app, &workspace.name, &strip.name, &id],
                    ) else {
                        continue;
                    };
                    rows.push(SearchResult {
                        kind: ResourceKind::Custom("Tabs".into()),
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
            }
        }
        rows
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        let tree = ctx.chrome.tree();
        let Some(workspace) = tree.active_workspace() else {
            return "desktop-windows:empty".into();
        };
        let mut revision = format!("desktop-windows:{}", workspace.id.raw());
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                revision.push_str(&format!(":g{}", group.id));
                for tab in &group.tabs {
                    revision.push_str(&format!(":t{}", tab.id));
                }
            }
        }
        revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use crate::shell::state::{Tab, WindowGroup, WindowWidth};
    use automerge::{AutoCommit, ROOT};

    fn fixture() -> (Chrome, AutoCommit) {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.workspaces[0].strips[0].window_groups = vec![
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
    fn enumerates_exact_duplicate_titled_tabs_and_groups() {
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
        assert_eq!(actions[2].title, "Close this window group");
    }

    #[test]
    fn exact_close_removes_only_target_and_queues_its_protocol_surface() {
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
        let ids = tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
            .collect::<Vec<_>>();
        assert_eq!(ids, [1, 2], "the same-title tab and ChatGPT stay open");
        drop(tree);
        assert_eq!(state.take_surface_close_requests(), [503]);
    }

    #[test]
    fn stale_exact_address_refuses_without_closing_another_window() {
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
            chrome.borrow().tree().workspaces[0].strips[0]
                .window_groups
                .len(),
            2
        );
        assert!(chrome.borrow_mut().take_surface_close_requests().is_empty());
        let _ = &mut doc;
    }
}
