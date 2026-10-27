use super::parked_workspaces::KEPT_WORKSPACES;
use super::*;
use crate::shell::state::{
    AppKind, StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceId, WorkspaceTree,
};

/// The demo desktop and an empty workspace after it: the shown workspace
/// (which has windows) with its first strip, and the one Super+Page Down
/// goes to from it.
fn desktop() -> (
    ShellOverlayHost,
    crate::shell::chrome_state::Chrome,
    (WorkspaceId, StripId),
    WorkspaceId,
) {
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.append_empty_workspace("Second");
    let home = tree.desktop.active;
    let strip = tree.desktop.active_strip[&home];
    let next = tree
        .desktop
        .workspaces
        .iter()
        .skip_while(|workspace| workspace.id != home)
        .nth(1)
        .expect("a workspace after the shown one")
        .id;
    let (host, chrome) = crate::shell::build_tilo_overlay_host_with_chrome(&tree);
    (host, chrome, (home, strip), next)
}

fn order(chrome: &crate::shell::chrome_state::Chrome) -> Vec<u64> {
    chrome
        .borrow()
        .tree()
        .desktop
        .workspaces
        .iter()
        .map(|workspace| workspace.id.raw())
        .collect()
}

fn switch(host: &mut ShellOverlayHost, chrome: &crate::shell::chrome_state::Chrome, dir: i32) {
    assert!(
        chrome.borrow_mut().switch_workspace(dir),
        "a workspace to switch to"
    );
    assert!(
        host.tick_workspace_content_for_tests(),
        "a workspace switch swaps the content"
    );
}

fn stub_group(id: u64) -> WindowGroup {
    let tab = id.saturating_mul(10);
    WindowGroup {
        id,
        tabs: vec![Tab {
            id: tab,
            title: format!("G{id}"),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: format!("G{id}"),
            },
            app_title: None,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

#[test]
fn switching_back_puts_the_workspace_back_as_it_was_left() {
    let (mut host, chrome, (home, strip), second) = desktop();
    host.group_row_mut_for_tests(strip)
        .expect("the shown workspace's strip is rendered")
        .set_scroll_x_for_test(123.0);

    switch(&mut host, &chrome, 1);
    assert!(
        host.group_row_mut_for_tests(strip).is_none(),
        "only the shown workspace's strips are rendered"
    );
    assert_eq!(host.kept_workspaces_for_tests(), vec![home.raw()]);

    switch(&mut host, &chrome, -1);
    let row = host
        .group_row_mut_for_tests(strip)
        .expect("the workspace switched back to is rendered again");
    assert!(
        (row.scroll_x() - 123.0).abs() < 1e-9,
        "the strip comes back where it was left (the same rows, not rebuilt ones), at {}",
        row.scroll_x()
    );
    assert_eq!(
        host.kept_workspaces_for_tests(),
        vec![second.raw()],
        "the workspace switched away from is kept in turn"
    );
}

#[test]
fn what_changed_while_a_workspace_was_away_shows_when_it_comes_back() {
    let (mut host, chrome, (home, strip), _) = desktop();
    switch(&mut host, &chrome, 1);
    chrome
        .borrow()
        .tree_mut()
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == home)
        .expect("the first workspace")
        .strips[0]
        .window_groups
        .push(stub_group(9_001));
    chrome.borrow_mut().bump();
    assert!(!host.tick_workspace_content_for_tests());
    switch(&mut host, &chrome, -1);
    let row = host
        .group_row_mut_for_tests(strip)
        .expect("the workspace switched back to is rendered");
    assert!(
        row.group_ids().contains(&9_001),
        "a window opened in the workspace while it was away is there when it comes back: {:?}",
        row.group_ids()
    );
}

#[test]
fn a_removed_workspace_is_not_kept() {
    let (mut host, chrome, (home, _), _) = desktop();
    switch(&mut host, &chrome, 1);
    assert_eq!(host.kept_workspaces_for_tests(), vec![home.raw()]);
    {
        let state = chrome.borrow();
        state
            .tree_mut()
            .desktop
            .workspaces
            .retain(|workspace| workspace.id != home);
    }
    chrome.borrow_mut().bump();
    host.tick_workspace_content_for_tests();
    assert!(
        host.kept_workspaces_for_tests().is_empty(),
        "a workspace that no longer exists lets go of its apps"
    );
}

#[test]
fn a_workspace_a_window_moved_out_of_is_not_kept() {
    let (mut host, chrome, (home, _), second) = desktop();
    switch(&mut host, &chrome, 1);
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let moved = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == home)
            .and_then(|workspace| workspace.strips[0].window_groups.pop())
            .expect("a window in the first workspace");
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == second)
            .expect("the second workspace")
            .strips[0]
            .window_groups
            .push(moved);
    }
    chrome.borrow_mut().bump();
    host.tick_workspace_content_for_tests();
    assert!(
        host.kept_workspaces_for_tests().is_empty(),
        "the window is mounted where it went, not also in the kept workspace"
    );
}

#[test]
fn only_the_most_recently_shown_workspaces_are_kept() {
    let (mut host, chrome, (home, _), _) = desktop();
    for index in 0..KEPT_WORKSPACES + 2 {
        chrome
            .borrow()
            .tree_mut()
            .append_empty_workspace(&format!("More {index}"));
    }
    chrome.borrow_mut().bump();
    host.tick_workspace_content_for_tests();
    let order = order(&chrome);
    let start = order
        .iter()
        .position(|id| *id == home.raw())
        .expect("the shown workspace");
    let shown = start + KEPT_WORKSPACES + 2;
    for _ in start..shown {
        switch(&mut host, &chrome, 1);
    }
    assert_eq!(chrome.borrow().tree().desktop.active.raw(), order[shown]);
    assert_eq!(
        host.kept_workspaces_for_tests(),
        order[shown - KEPT_WORKSPACES..shown].to_vec(),
        "the {KEPT_WORKSPACES} workspaces shown last before this one are kept, oldest first"
    );
}
