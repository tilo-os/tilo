//! Which windows are asking for the person's attention.
//!
//! A window can be refused the keyboard — a stale activation token, a
//! background app opening something, a dialog mapping where the person is
//! not looking — and then nothing says so. This is the small, transient
//! record behind the tab pill's dot and the window menu's "Go to this
//! window": never persisted, never replicated, cleared the moment the tab
//! has the person's focus.
//!
//! Doubly keyed because a refusal names a surface before the chrome has
//! resolved it to a tab, while a docked dialog names the parent surface it
//! belongs to. A tab asks while either its own id or its surface is in the
//! set.

use std::cell::RefCell;
use std::collections::BTreeSet;

use crate::shell::state::WorkspaceTree;

#[derive(Default)]
struct Asking {
    tabs: BTreeSet<u64>,
    surfaces: BTreeSet<u64>,
}

impl Asking {
    /// Whether anything is waiting. Cheap enough to ask while painting.
    fn any(&self) -> bool {
        !self.tabs.is_empty() || !self.surfaces.is_empty()
    }
}

thread_local! {
    static ASKING: RefCell<Asking> = RefCell::new(Asking::default());
}

/// A tab is waiting for the person.
pub fn ask_tab(tab: u64) {
    ASKING.with(|asking| {
        asking.borrow_mut().tabs.insert(tab);
    });
}

/// A surface is waiting for the person, before its tab is known or through
/// the parent a dialog belongs to.
pub fn ask_surface(surface: u64) {
    ASKING.with(|asking| {
        asking.borrow_mut().surfaces.insert(surface);
    });
}

/// Whether this tab is waiting, by its own id or its window's surface.
pub fn is_asking(tab: u64, surface: Option<u64>) -> bool {
    ASKING.with(|asking| {
        let asking = asking.borrow();
        asking.tabs.contains(&tab)
            || surface.is_some_and(|surface| asking.surfaces.contains(&surface))
    })
}

/// Whether any window is waiting: the tab strip paints its quiet state when
/// none is.
pub fn any_asking() -> bool {
    ASKING.with(|asking| asking.borrow().any())
}

/// The tab has the person's focus (or has gone): stop asking. Takes the
/// tab's window surface too, because a refusal may have named either.
pub fn answered(tab: u64, surface: Option<u64>) {
    ASKING.with(|asking| {
        let mut asking = asking.borrow_mut();
        asking.tabs.remove(&tab);
        if let Some(surface) = surface {
            asking.surfaces.remove(&surface);
        }
    });
}

/// Clear every tab in the workspace's focused window: the person is looking
/// at it, whether they arrived by click, chord or action.
pub fn clear_focused(tree: &WorkspaceTree) {
    let Some(workspace) = tree.desktop.active_workspace() else {
        return;
    };
    let strip = tree
        .desktop
        .active_strip
        .get(&workspace.id)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first());
    let Some(strip) = strip else {
        return;
    };
    let group = tree
        .desktop
        .active_group
        .get(&strip.id)
        .and_then(|id| strip.window_groups.iter().find(|group| group.id == *id))
        .or_else(|| strip.window_groups.first());
    let Some(group) = group else {
        return;
    };
    let active = group
        .tabs
        .iter()
        .find(|tab| tab.id == group.active_tab)
        .or_else(|| group.tabs.first());
    let Some(tab) = active else {
        return;
    };
    let surface = match &tab.tile {
        crate::shell::state::TileContent::Foreign { surface_id, .. } => Some(*surface_id),
        _ => None,
    };
    answered(tab.id, surface);
}

/// Forget every mark. A fresh lock or a cleared desktop starts quiet.
pub fn clear_all() {
    ASKING.with(|asking| *asking.borrow_mut() = Asking::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attention is per thread and test in the same process must not leak.
    fn reset() {
        clear_all();
    }

    #[test]
    fn a_tab_asks_by_its_own_id_or_its_surface() {
        reset();
        ask_tab(7);
        assert!(is_asking(7, None));
        assert!(!is_asking(8, None));
        answered(7, None);
        assert!(!is_asking(7, None));

        ask_surface(44);
        assert!(is_asking(7, Some(44)));
        assert!(any_asking());
        answered(7, Some(44));
        assert!(!any_asking());
    }

    #[test]
    fn focusing_the_active_tab_answers_it_and_a_cleared_desktop_is_quiet() {
        let tree = WorkspaceTree::bootstrap_demo();
        let tab = tree
            .desktop
            .active_workspace()
            .and_then(|workspace| workspace.strips.first())
            .and_then(|strip| strip.window_groups.first())
            .and_then(|group| group.tabs.first())
            .map(|tab| tab.id)
            .expect("the demo desk has a tab");
        reset();
        ask_tab(tab);
        assert!(any_asking());
        clear_focused(&tree);
        assert!(!any_asking(), "the focused tab is no longer asking");

        ask_tab(tab);
        clear_all();
        assert!(!any_asking());
    }
}
