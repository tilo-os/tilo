//! A closed tab comes back where it was: same content, same group, same
//! index through `tab.reopen`, without producing close notifications.

use shell::shell::chrome_state::ChromeState;
use shell::shell::state::{
    AppKind, FreshId as _, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree,
};
use shell::shell::window_actions::close_tab;

fn notes(id: u64, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::AppPlaceholder {
            kind: AppKind::Terminal,
            label: title.into(),
        },
        app_title: None,
    }
}

fn group(id: u64, tabs: Vec<Tab>, width: WindowWidth) -> WindowGroup {
    let active_tab = tabs.last().map_or(0, |tab| tab.id);
    WindowGroup {
        id,
        tabs,
        active_tab,
        width,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// Groups 100 [1, 2, 3], 200 [4] (wide) and 300 [5] in one strip.
fn desktop() -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_default();
    let strip = tree.desktop.workspaces[0].strips[0].id;
    tree.desktop.workspaces[0].strips[0].window_groups = vec![
        group(
            100,
            vec![notes(1, "Plan"), notes(2, "Draft"), notes(3, "Notes")],
            WindowWidth::Normal,
        ),
        group(200, vec![notes(4, "Wide")], WindowWidth::Wide),
        group(300, vec![notes(5, "Last")], WindowWidth::Normal),
    ];
    tree.desktop.workspaces[0].strips[0].window_groups[0].active_tab = 2;
    tree.desktop.set_active_group(strip, 100);
    tree.seed_doc_from_cache();
    tree
}

fn layout(chrome: &ChromeState) -> Vec<(u64, Vec<u64>)> {
    let tree = chrome.tree();
    let groups = tree.desktop.workspaces[0].strips[0]
        .window_groups
        .iter()
        .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
        .collect();
    drop(tree);
    groups
}

#[test]
fn close_then_reopen_restores_content_group_and_index() {
    let chrome = ChromeState::new(desktop());
    let before = layout(&chrome.borrow());

    close_tab(&mut chrome.borrow_mut(), 2).unwrap();
    assert_eq!(layout(&chrome.borrow())[0], (100, vec![1, 3]));
    assert_eq!(
        chrome.borrow().notification_count(),
        0,
        "a normal close is quiet"
    );

    assert_eq!(chrome.borrow_mut().reopen_closed_tab(), Some(2));
    assert_eq!(layout(&chrome.borrow()), before);
    let state = chrome.borrow();
    let tree = state.tree();
    let restored = &tree.desktop.workspaces[0].strips[0].window_groups[0];
    assert_eq!(restored.active_tab, 2, "the reopened tab is in front");
    assert!(matches!(
        &restored.tabs[1].tile,
        TileContent::AppPlaceholder { kind: AppKind::Terminal, label } if label == "Draft"
    ));
    let strip = tree.desktop.workspaces[0].strips[0].id;
    assert_eq!(tree.desktop.active_group.get(&strip), Some(&100));
    drop(tree);
    assert_eq!(state.notification_count(), 0);
    assert_eq!(state.notification_history().len(), 0);
    drop(state);
    assert_eq!(
        chrome.borrow_mut().reopen_closed_tab(),
        None,
        "each close reopens once"
    );
}

#[test]
fn reopening_quietly_recreates_a_closed_group_in_place() {
    let chrome = ChromeState::new(desktop());
    let before = layout(&chrome.borrow());

    close_tab(&mut chrome.borrow_mut(), 4).unwrap();
    assert_eq!(
        layout(&chrome.borrow())
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [100, 300],
        "closing a group's last tab closes the group"
    );

    assert_eq!(chrome.borrow().notification_count(), 0);
    assert_eq!(chrome.borrow_mut().reopen_closed_tab(), Some(4));
    assert_eq!(layout(&chrome.borrow()), before);
    let state = chrome.borrow();
    let width = state.tree().desktop.workspaces[0].strips[0].window_groups[1].width;
    assert_eq!(width, WindowWidth::Wide, "the group keeps its width");
    assert_eq!(state.notification_count(), 0);
    assert_eq!(state.notification_history().len(), 0);
}

#[test]
fn a_close_whose_workspace_is_gone_is_skipped() {
    let mut tree = desktop();
    let home = tree.desktop.workspaces[0].id;
    let away = WorkspaceId::new();
    let mut elsewhere = Workspace::in_room(away, RoomId::new(), "Away");
    elsewhere.strips = vec![Strip {
        id: StripId::new(),
        name: String::new(),
        window_groups: vec![group(900, vec![notes(9, "Elsewhere")], WindowWidth::Normal)],
    }];
    tree.desktop.workspaces.push(elsewhere);
    tree.desktop.active = away;
    tree.seed_doc_from_cache();
    let chrome = ChromeState::new(tree);

    close_tab(&mut chrome.borrow_mut(), 9).unwrap();
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        tree.desktop
            .workspaces
            .retain(|workspace| workspace.id != away);
        tree.desktop.active = home;
    }
    let before = layout(&chrome.borrow());
    assert_eq!(chrome.borrow_mut().reopen_closed_tab(), None);
    assert_eq!(chrome.borrow_mut().reopen_closed_tab(), None);
    assert_eq!(chrome.borrow().notification_count(), 0);
    assert_eq!(
        layout(&chrome.borrow()),
        before,
        "nothing lands in another workspace"
    );
}
