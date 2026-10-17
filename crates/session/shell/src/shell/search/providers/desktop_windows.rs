//! Addressable actions for the live desktop topology.
//!
//! These rows expose tabs and window groups through the same render-free
//! action catalogue used by the command palette and control socket. Going to
//! a tab sets the same four focus coordinates a click does; closing commits
//! through the durable tab lifecycle, including the compositor's normal
//! foreign-surface close request. Neither synthesises pointer input.

use std::rc::Rc;

use tilo_desktop::search::{
    WINDOWS_PROVIDER_ID, WindowAction, WindowRow, WindowRowKind, window_rows, window_rows_revision,
    windows_provider_name,
};
use ui::search::{FieldWeight, ListIndex};

use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{GroupId, StripId, TabId, WorkspaceId};

fn close_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        risk: tilo_actions::RiskLevel::Low,
        retry: tilo_actions::RetryPolicy::Idempotent,
        tags: vec!["desktop".into(), "window".into(), "close".into()],
        ..tilo_actions::ActionContract::default()
    }
}

fn move_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
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

/// Brings the tab to the front, as clicking it would.
fn focus_tab(
    chrome: &Chrome,
    workspace: WorkspaceId,
    strip: StripId,
    group: GroupId,
    tab: TabId,
) -> Result<(), ActionError> {
    tab_in_group(chrome, group, tab)?;
    chrome.borrow_mut().focus_tab(workspace, strip, group, tab);
    Ok(())
}

/// Closes through the one tab lifecycle, so the close can be undone.
fn close_tab(chrome: &Chrome, expected_group: GroupId, tab_id: TabId) -> Result<(), ActionError> {
    tab_in_group(chrome, expected_group, tab_id)?;
    crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), tab_id)
        .map_err(|_| refused(tab_id))
}

/// Moves or copies through the one cross-workspace transfer, the same one a
/// dragged overview preview takes. A client window's document crosses only
/// after the person at this desktop agrees on the sheet this puts up, so an
/// agent asking gets exactly what a person dragging gets.
fn transfer_tab(
    chrome: &Chrome,
    expected_group: GroupId,
    tab_id: TabId,
    target: WorkspaceId,
    mode: crate::shell::window_transfer::TransferMode,
) -> Result<(), ActionError> {
    tab_in_group(chrome, expected_group, tab_id)?;
    crate::shell::window_actions::transfer_tab_to_workspace(
        &mut chrome.borrow_mut(),
        tab_id,
        target,
        mode,
    )
    .map(|_| ())
    .map_err(|err| ActionError::Explained(err.user_message()))
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
        chrome.borrow_mut().remember_closed_tab_after_removal(entry);
    }
    Ok(())
}

fn contract(action: WindowAction) -> tilo_actions::ActionContract {
    match action {
        WindowAction::FocusTab { .. } => tilo_actions::ActionContract::view(),
        WindowAction::CloseGroup { .. } | WindowAction::CloseTab { .. } => close_contract(),
        WindowAction::MoveTab { .. } | WindowAction::CopyTab { .. } => move_contract(),
    }
}

fn invoke(chrome: &Chrome, action: WindowAction) -> Result<(), ActionError> {
    use crate::shell::window_transfer::TransferMode;
    match action {
        WindowAction::FocusTab {
            workspace,
            strip,
            group,
            tab,
        } => focus_tab(chrome, workspace, strip, group, tab),
        WindowAction::CloseGroup { group } => close_group(chrome, group),
        WindowAction::CloseTab { group, tab } => close_tab(chrome, group, tab),
        WindowAction::MoveTab {
            group,
            tab,
            workspace,
        } => transfer_tab(chrome, group, tab, workspace, TransferMode::Move),
        WindowAction::CopyTab {
            group,
            tab,
            workspace,
        } => transfer_tab(chrome, group, tab, workspace, TransferMode::Copy),
    }
}

fn result(row: WindowRow, score: i32) -> SearchResult {
    let action = row.action;
    SearchResult {
        shortcut: None,
        kind: match row.kind {
            WindowRowKind::Tab => ResourceKind::Tab,
            WindowRowKind::WindowGroup => ResourceKind::WindowGroup,
        },
        operation_kind: match action {
            WindowAction::FocusTab { .. } => tilo_actions::OperationKind::View,
            WindowAction::CloseGroup { .. }
            | WindowAction::CloseTab { .. }
            | WindowAction::MoveTab { .. }
            | WindowAction::CopyTab { .. } => tilo_actions::OperationKind::Effect,
        },
        provider_id: WINDOWS_PROVIDER_ID.into(),
        id: row.id,
        title: row.title,
        subtitle: Some(row.subtitle),
        badge: Some(row.badge),
        score,
        params: Vec::new(),
        contract: Some(contract(action)),
        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
            invoke(chrome, action)
        }),
        on_invoke: None,
        on_begin_invoke: None,
    }
}

#[derive(Default)]
pub struct DesktopWindowsProvider {
    /// The rows' names, indexed while they stay the same.
    ranker: ListIndex,
}

static KINDS: std::sync::LazyLock<Vec<ResourceKind>> =
    std::sync::LazyLock::new(|| vec![ResourceKind::Tab, ResourceKind::WindowGroup]);

impl DesktopWindowsProvider {
    pub fn new() -> Self {
        Self::default()
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
        let rows = window_rows(&tree.desktop, ctx.needle);
        drop(tree);
        // An exact address is how an agent or a pasted id selects one row;
        // it resolves outright instead of being ranked against names.
        let exact = ctx.needle.trim();
        if let Some(row) = rows.iter().find(|row| row.id == exact) {
            return vec![result(row.clone(), self.max_score())];
        }
        let names: Vec<Vec<(String, FieldWeight)>> = rows
            .iter()
            .map(|row| {
                row.names
                    .iter()
                    .map(|name| (name.clone(), FieldWeight::TITLE))
                    .chain(
                        row.hint
                            .iter()
                            .map(|hint| (hint.clone(), FieldWeight::SUBTITLE)),
                    )
                    .collect()
            })
            .collect();
        let rows = rows.into_iter().map(|row| result(row, 0)).collect();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle, rows, &names)
    }

    /// An address is looked up in the whole topology. The empty query's
    /// first page holds [`ui::search::SearchBudget::PALETTE`] rows, fewer
    /// than a long session has window actions, so resolving through it left
    /// `group.close` for the focused window `unknown_action` (A07).
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        let tree = ctx.chrome.tree();
        let rows = window_rows(&tree.desktop, id);
        drop(tree);
        rows.into_iter()
            .find(|row| row.id == id)
            .map(|row| result(row, self.max_score()))
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
    use crate::shell::search::{SearchProvider, SearchRegistry, SearchScope};
    use crate::shell::state::{FreshId as _, Tab, TileContent, WindowGroup, WindowWidth};
    use automerge::{AutoCommit, ROOT};
    use tilo_desktop::search::window_row_tile;

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
                focus_history: Vec::new(),
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
                focus_history: Vec::new(),
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
                "close Profilfehler",
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
                    && action.subtitle.as_deref() == Some("Workspace “Home” · strip “Main”")
            }));
            assert!(
                actions.iter().all(|action| action
                    .subtitle
                    .as_deref()
                    .is_some_and(|text| !text.contains("100") && !text.contains("200"))),
                "a subtitle names a group by its place, never its internal id: {:?}",
                actions.iter().map(|a| &a.subtitle).collect::<Vec<_>>()
            );
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
                    &ResourceRef::new(WINDOWS_PROVIDER_ID, "tab.close:200:3"),
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
                .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
                .expect("close accepted");

            crate::shell::tab_binding::close_tab_for_destroyed_surface_and_persist(
                &mut chrome.borrow_mut().tree_mut(),
                503,
            );
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
                    &ResourceRef::new(WINDOWS_PROVIDER_ID, "tab.close:100:999"),
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

    fn palette(chrome: &Chrome, doc: &AutoCommit, needle: &str) -> Vec<SearchResult> {
        let state = chrome.borrow();
        registry().query(&crate::shell::search::QueryArgs {
            needle,
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc,
            doc_root: &ROOT,
        })
    }

    #[test]
    fn searching_a_window_goes_to_it_and_never_offers_to_close_it() {
        let (chrome, mut doc) = fixture();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let strip = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop.set_active_group(strip, 200);
        }

        // Tab 3 is focused. A search still names every matching window,
        // including that one: findable by name means the open window is a
        // result, not only its close actions.
        let rows = palette(&chrome, &doc, "Profilfehler");
        let mut ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["tab.focus:100:1", "tab.focus:200:3"]);
        let go = rows
            .iter()
            .find(|row| row.id == "tab.focus:100:1")
            .expect("unfocused window stays a focus result");
        assert_eq!(go.title, "Profilfehler aufgetreten");
        assert_eq!(go.operation_kind, tilo_actions::OperationKind::View);
        assert_eq!(
            go.subtitle.as_deref(),
            Some("Open in this workspace · 2 tabs")
        );

        // A window is found by its app when its title names the document.
        let mut ids: Vec<String> = palette(&chrome, &doc, "chrome")
            .into_iter()
            .map(|row| row.id)
            .collect();
        ids.sort();
        assert_eq!(ids, ["tab.focus:100:1", "tab.focus:200:3"]);

        // Enter on the unfocused row brings that window forward, as a click would.
        (go.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("focused");
        let (active_group, active_tab) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let strip = &tree.desktop.workspaces[0].strips[0];
            (
                tree.desktop.active_group_for(strip.id),
                strip.window_groups[0].active_tab,
            )
        };
        assert_eq!((active_group, active_tab), (Some(100), 1));
        assert!(chrome.borrow_mut().take_surface_close_requests().is_empty());

        // Ids and app ids are addresses, not names a person types.
        assert!(palette(&chrome, &doc, "google").is_empty());
        assert!(palette(&chrome, &doc, "200").is_empty());

        // Every row a person reads names places, never ids.
        for row in palette(&chrome, &doc, "") {
            let subtitle = row.subtitle.unwrap_or_default();
            assert!(
                !subtitle.contains("100")
                    && !subtitle.contains("200")
                    && !subtitle.contains("com."),
                "{} carries an internal id in {subtitle:?}",
                row.id
            );
        }
    }

    #[test]
    fn a_listing_keeps_every_window_action_for_agents() {
        let (chrome, doc) = fixture();
        let state = chrome.borrow();
        let catalog = ActionCatalog::build(
            &registry(),
            "",
            SearchScope::All,
            None,
            &state,
            &doc,
            &ROOT,
            &AllVisible,
        );
        let ids: Vec<String> = catalog
            .enumerate()
            .into_iter()
            .map(|action| action.resource.id)
            .collect();
        for id in [
            "tab.focus:100:1",
            "tab.close:100:1",
            "group.close:100",
            "tab.close:200:3",
            "group.close:200",
        ] {
            assert!(
                ids.iter().any(|listed| listed == id),
                "{id} missing from {ids:?}"
            );
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
                &ResourceRef::new(WINDOWS_PROVIDER_ID, &id),
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
        let tabs_in = |chrome: &Chrome, workspace: usize| -> Vec<(TabId, TileContent)> {
            chrome.borrow().tree().desktop.workspaces[workspace]
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .map(|tab| (tab.id, tab.tile.clone()))
                .collect()
        };

        // An agent asking with nobody at the desktop to agree gets a refusal,
        // and nothing moves.
        chrome.borrow_mut().window_transfers.set_attended(|| false);
        assert!(
            action
                .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
                .is_err()
        );
        assert_eq!(tabs_in(&chrome, 1), []);

        // With someone there, the sheet asks first. This window keeps its
        // work only in its running process, so nothing can travel: the sheet
        // offers a new window there instead, and never closes this one.
        let queue = crate::shell::portal_dialog::PortalDialogQueue::default();
        let profiles = tempfile::tempdir().unwrap();
        {
            let mut state = chrome.borrow_mut();
            state.window_transfers.set_attended(|| true);
            state.window_transfers.set_dialog_queue(queue.clone());
            state
                .window_transfers
                .set_data_home(profiles.path().to_path_buf());
        }
        action
            .invoke(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome, [])
            .expect("the sheet is up");
        assert_eq!(tabs_in(&chrome, 1), [], "nothing changes before the answer");
        let mut sheet = crate::shell::portal_dialog::PortalDialogController::new(queue);
        assert!(sheet.pump());
        let view = sheet.view().expect("the transfer sheet");
        assert_eq!(view.choices().len(), 1, "only a new window can be offered");
        assert!(view.detail().contains("Elsewhere"));
        assert!(sheet.confirm());
        crate::shell::window_transfer::tick(&mut chrome.borrow_mut());

        let home: Vec<TabId> = tabs_in(&chrome, 0).into_iter().map(|(id, _)| id).collect();
        assert_eq!(home, [1, 2, 3], "the original window stays where it is");
        let opened = tabs_in(&chrome, 1);
        assert_eq!(opened.len(), 1);
        assert!(
            matches!(&opened[0].1, TileContent::LocalApp { app_id, note, .. }
                if app_id == "com.google.Chrome" && note.is_empty()),
            "a new window of the same app opens in the destination"
        );
        assert!(
            chrome.borrow_mut().take_surface_close_requests().is_empty(),
            "a window whose state cannot travel is never asked to close"
        );
    }

    /// Stands in for the machine's Linux apps: a long list the empty
    /// palette must not open with.
    struct LinuxApps;

    impl crate::shell::search::SearchProvider for LinuxApps {
        fn id(&self) -> &'static str {
            "host-apps"
        }
        fn name(&self) -> &'static str {
            "Linux apps"
        }
        fn resource_kinds(&self) -> &[ResourceKind] {
            &[ResourceKind::HostApplication]
        }
        fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
            (0..40)
                .map(|i| {
                    SearchResult::nullary(
                        ResourceKind::HostApplication,
                        "host-apps",
                        format!("org.example.App{i}"),
                        format!("App {i}"),
                        None,
                        10,
                        |_, _, _| {},
                    )
                })
                .collect()
        }
    }

    #[test]
    fn the_empty_palette_offers_windows_to_go_back_to_not_an_inventory() {
        let (chrome, doc) = fixture();
        let mut registry = registry();
        registry.register(Rc::new(
            crate::shell::search::providers::ActionsProvider::new(),
        ));
        registry.register(Rc::new(LinuxApps));
        let state = chrome.borrow();
        let groups = registry.query_grouped(
            &crate::shell::search::QueryArgs {
                needle: "",
                scope: SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &state,
                doc: &doc,
                doc_root: &ROOT,
            },
            12,
        );
        let first = groups.first().expect("the empty palette has rows");
        assert_eq!(first.kind, ResourceKind::Tab, "windows come first");
        assert!(
            first
                .results
                .iter()
                .all(|row| row.id.starts_with("tab.focus:")),
            "every window row goes to it: {:?}",
            first.results.iter().map(|row| &row.id).collect::<Vec<_>>()
        );
        let rows: Vec<&SearchResult> = groups.iter().flat_map(|group| &group.results).collect();
        assert!(
            rows.iter().all(
                |row| row.operation_kind == tilo_actions::OperationKind::View
                    || row.kind == ResourceKind::Application
            ),
            "nothing on the empty palette changes anything: {:?}",
            rows.iter().map(|row| &row.id).collect::<Vec<_>>()
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.kind == ResourceKind::HostApplication),
            "the Linux app list is a keystroke away, not the default"
        );
        assert!(rows.iter().any(|row| row.id == "room.activity.open"));
        assert!(
            rows.iter().any(|row| row.id == "shortcut-help.toggle"),
            "help stays on the empty palette even when it is last in the catalogue"
        );
        assert!(
            groups
                .iter()
                .all(|group| group.total == group.results.len())
        );

        let app_scoped = registry.query_grouped(
            &crate::shell::search::QueryArgs {
                needle: "",
                scope: SearchScope::ActiveApp,
                active_provider_id: Some("host-apps"),
                wanted_kinds: &[],
                chrome: &state,
                doc: &doc,
                doc_root: &ROOT,
            },
            12,
        );
        assert_eq!(
            app_scoped
                .iter()
                .find(|group| group.kind == ResourceKind::HostApplication)
                .map(|group| group.total),
            Some(40),
            "scoped to one app, the empty palette lists that app's inventory, not a curated pick"
        );

        let listing = registry.query_exhaustive(&crate::shell::search::QueryArgs {
            needle: "",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc: &doc,
            doc_root: &ROOT,
        });
        assert!(
            listing.iter().any(|row| row.id == "tab.close:100:1")
                && listing
                    .iter()
                    .filter(|row| row.kind == ResourceKind::HostApplication)
                    .count()
                    == 40,
            "agents still enumerate every action and app"
        );
    }

    #[test]
    fn every_window_row_resolves_to_the_tile_whose_face_it_wears() {
        let (chrome, _doc) = fixture();
        let state = chrome.borrow();
        let tree = state.tree();
        let rows = window_rows(&tree.desktop, "");
        assert!(
            !rows.is_empty(),
            "the fixture must offer window rows to resolve"
        );
        for row in &rows {
            let tile = window_row_tile(&tree.desktop, &row.id)
                .unwrap_or_else(|| panic!("{} must resolve to the tile it wears", row.id));
            let app = match tile {
                TileContent::Foreign { app_id, .. }
                | TileContent::LocalApp { app_id, .. }
                | TileContent::PeerWindow { app_id, .. }
                | TileContent::HostedWindow { app_id, .. } => app_id.as_str(),
                other => panic!("{} wore a tile with no app face: {other:?}", row.id),
            };
            let expected = if row.id.contains(":100:2") || row.id == "group.close:100" {
                "com.openai.ChatGPT"
            } else if row.id.contains(":100:1")
                || row.id.contains(":200:3")
                || row.id == "group.close:200"
            {
                "com.google.Chrome"
            } else {
                panic!("unexpected window row {}", row.id);
            };
            assert_eq!(
                app, expected,
                "{} must wear the same face as its tab or group",
                row.id
            );
        }
        assert!(
            window_row_tile(&tree.desktop, "tab.focus:100:1").is_some(),
            "an address still resolves when it is not in this listing"
        );
        assert!(
            window_row_tile(&tree.desktop, "no-such-row").is_none(),
            "an unknown address has no face"
        );
        drop(tree);
    }

    fn many_groups(count: usize, focused: u64) -> (Chrome, AutoCommit) {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].strips[0].window_groups = (1..=count)
            .map(|i| {
                let gid = u64::try_from(i).unwrap_or(1);
                WindowGroup {
                    id: gid,
                    tabs: vec![Tab {
                        id: gid,
                        title: format!("Win {i}"),
                        tile: TileContent::Foreign {
                            app_id: format!("app.{i}"),
                            surface_id: 500_u64.saturating_add(gid),
                        },
                    }],
                    active_tab: gid,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }
            })
            .collect();
        let strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.set_active_group(strip, focused);
        tree.seed_doc_from_cache();
        (ChromeState::new(tree), AutoCommit::new())
    }

    #[test]
    fn invoke_resolves_the_focused_groups_close_when_the_listing_is_truncated() {
        let count = crate::shell::search::SearchBudget::PALETTE.max_results;
        let focused = u64::try_from(count).unwrap_or(1);
        let (chrome, doc) = many_groups(count, focused);
        let id = format!("group.close:{focused}");
        let catalog = {
            let state = chrome.borrow();
            ActionCatalog::build_for_resource(
                &registry(),
                &ResourceRef::new(WINDOWS_PROVIDER_ID, &id),
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
                .select(&ResourceRef::new(WINDOWS_PROVIDER_ID, &id))
                .is_some(),
            "A07 invoke uses build_for_resource, not the PALETTE page"
        );
        let state = chrome.borrow();
        let page = DesktopWindowsProvider::new().query_page(
            crate::shell::search::SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: KINDS.as_slice(),
                chrome: &state,
                doc: &doc,
                doc_root: &ROOT,
            },
            None,
            crate::shell::search::SearchBudget::PALETTE,
        );
        assert!(
            page.results.iter().any(|row| row.id == id),
            "the focused group's close stays on the empty page: {:?}",
            page.results.iter().map(|row| &row.id).collect::<Vec<_>>()
        );
    }
}
