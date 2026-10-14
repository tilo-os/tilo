//! A closed tab comes back where it was: same content, same group, same
//! index, whether through `tab.reopen` or the close toast's Undo.

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

fn undo_toast(chrome: &ChromeState) -> Option<u64> {
    chrome
        .notifications()
        .iter()
        .find(|toast| toast.title.starts_with("Closed"))
        .map(|toast| toast.id)
}

#[test]
fn close_then_reopen_restores_content_group_and_index() {
    let chrome = ChromeState::new(desktop());
    let before = layout(&chrome.borrow());

    close_tab(&mut chrome.borrow_mut(), 2).unwrap();
    assert_eq!(layout(&chrome.borrow())[0], (100, vec![1, 3]));
    assert!(
        undo_toast(&chrome.borrow()).is_some(),
        "a close says how to undo it"
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
    assert!(
        undo_toast(&state).is_none(),
        "the toast would now undo nothing"
    );
    drop(state);
    assert_eq!(
        chrome.borrow_mut().reopen_closed_tab(),
        None,
        "each close reopens once"
    );
}

#[test]
fn undo_on_the_toast_recreates_a_closed_group_in_place() {
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

    let toast = undo_toast(&chrome.borrow()).expect("undo toast");
    assert!(chrome.borrow_mut().activate_notification(toast));
    assert_eq!(layout(&chrome.borrow()), before);
    let state = chrome.borrow();
    let width = state.tree().desktop.workspaces[0].strips[0].window_groups[1].width;
    assert_eq!(width, WindowWidth::Wide, "the group keeps its width");
    assert!(
        state
            .notification_history()
            .iter()
            .all(|entry| !entry.title.starts_with("Closed")),
        "a used Undo is not kept as a notification"
    );
}

#[test]
fn a_close_whose_workspace_is_gone_is_skipped() {
    let mut tree = desktop();
    let home = tree.desktop.workspaces[0].id;
    let away = WorkspaceId::new();
    tree.desktop.workspaces.push(Workspace {
        id: away,
        room: RoomId::new(),
        name: "Away".into(),
        strips: vec![Strip {
            id: StripId::new(),
            name: String::new(),
            window_groups: vec![group(900, vec![notes(9, "Elsewhere")], WindowWidth::Normal)],
        }],
        ..tree.desktop.workspaces[0].clone()
    });
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
    let toast = undo_toast(&chrome.borrow()).expect("undo toast");
    chrome.borrow_mut().activate_notification(toast);
    assert_eq!(
        layout(&chrome.borrow()),
        before,
        "nothing lands in another workspace"
    );
}
