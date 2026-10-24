//! A workspace the person makes.

use super::*;

/// A workspace the person makes holds nothing but its first empty strip,
/// so it shows the empty-workspace launchpad.
#[test]
fn a_new_workspace_is_empty_and_shows_its_launchpad() {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
    let id = chrome.borrow_mut().add_workspace("Fresh");
    let state = chrome.borrow();
    let tree = state.tree();
    assert_eq!(tree.desktop.active, id);
    let workspace = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .expect("the workspace was added");
    assert_eq!(workspace.strips.len(), 1, "only its first strip");
    assert!(
        workspace.strips[0].window_groups.is_empty(),
        "and nothing in it"
    );
    assert!(!workspace.has_windows(), "so the launchpad shows");
    drop(tree);
}
