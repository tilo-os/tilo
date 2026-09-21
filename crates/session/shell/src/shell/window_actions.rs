//! Creating windows and tabs from the chrome.
//!
//! `WorkspaceTree::bootstrap_default` supplies no windows on first boot —
//! a window nobody opened is a window nobody opened — so these primitives
//! are not a convenience on top of a pre-populated desktop: on a fresh
//! machine they are the only way a first window exists at all, and they
//! have to work from empty.
//!
//! These are the primitives behind the "+" chip on a group's tab strip,
//! the `Super`-chords, and the palette's action rows. They all funnel
//! here so "new terminal" means the same thing however it was asked for,
//! and so placement stays a single decision made by
//! [`crate::shell::placement`].

use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{AppKind, GroupId, TabId, TileContent};
use crate::shell::tab_binding::{self, Placement};

/// Where a newly-created window should go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Follow [`crate::shell::placement::placement_for_new_window`] — to
    /// the right of the active window, or into its group when that group
    /// is already tabbed.
    Beside,
    /// Join the focused group as a tab, whatever the rule would say.
    /// Falls back to `Beside` when nothing is focused.
    Here,
}

/// The group the user is looking at, or `None` when the active workspace
/// has no groups (a fresh empty workspace).
pub fn focused_group(st: &ChromeState) -> Option<GroupId> {
    let tree = st.tree();
    let ws = tree.active_workspace()?;
    let strip = tree
        .active_strip
        .get(&ws.id)
        .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
        .or_else(|| ws.strips.first())?;
    tree.active_group
        .get(&strip.id)
        .copied()
        .filter(|g| strip.window_groups.iter().any(|w| w.id == *g))
        .or_else(|| strip.window_groups.first().map(|g| g.id))
}

/// Resolve a [`Destination`] into the placement the splice will use.
fn placement_for(st: &ChromeState, dest: Destination) -> Placement {
    match dest {
        Destination::Here => match focused_group(st) {
            Some(g) => Placement::TabIn(g),
            None => rule_placement(st),
        },
        Destination::Beside => rule_placement(st),
    }
}

/// The ordinary placement rule, evaluated against the layout the user is
/// currently looking at.
fn rule_placement(st: &ChromeState) -> Placement {
    let tree = st.tree();
    crate::shell::placement::placement_for_new_window(&tree, st.strip_viewport())
}

/// Hosted in-process, so this is a tree splice rather than a launch — a
/// sane recovery path when the last terminal is gone.
pub fn open_terminal(st: &mut ChromeState, dest: Destination) -> TabId {
    open_hosted(st, AppKind::Terminal, "Terminal", dest)
}

/// Open any hosted app kind at `dest` and focus it.
pub fn open_hosted(
    st: &mut ChromeState,
    kind: AppKind,
    title: impl Into<String>,
    dest: Destination,
) -> TabId {
    let placement = placement_for(st, dest);
    let id = {
        let mut tree = st.tree_mut();
        tab_binding::open_hosted_app(&mut tree, kind, title, placement)
    };
    st.bump();
    id
}

/// Why a duplicate could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateError {
    /// No tab is focused — nothing to copy.
    NoActiveTab,
    /// A foreign Wayland client's window belongs to another process; the
    /// compositor can't clone it, and guessing a package from `app_id`
    /// would as often start the wrong thing.
    ForeignWindow,
}

impl DuplicateError {
    pub fn user_message(self) -> &'static str {
        match self {
            DuplicateError::NoActiveTab => "There is no tab to duplicate.",
            DuplicateError::ForeignWindow => {
                "This window belongs to an app the compositor doesn't host, so it can't be copied. \
                 Open a second one from search instead."
            }
        }
    }
}

/// A fresh instance, not a copy of state — the new tab starts however that
/// app starts (a second shell, for a terminal, the case this exists for).
///
/// # Errors
///
/// [`DuplicateError::NoActiveTab`] when nothing is focused (an empty
/// workspace, or a strip/group/tab list gone empty underneath the
/// active ids); [`DuplicateError::ForeignWindow`] when the focused tab
/// is a client-owned or peer surface the compositor can't clone.
#[allow(
    clippy::significant_drop_tightening,
    reason = "every field this reads is chained from the previous lookup (ws -> strip -> group \
              -> tab), so the ChromeState borrow can't be dropped before the last one without \
              cloning each step early for no real gain"
)]
pub fn duplicate_active_tab(st: &mut ChromeState) -> Result<TabId, DuplicateError> {
    let (kind, title, group) = {
        let tree = st.tree();
        let ws = tree.active_workspace().ok_or(DuplicateError::NoActiveTab)?;
        let strip = tree
            .active_strip
            .get(&ws.id)
            .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
            .or_else(|| ws.strips.first())
            .ok_or(DuplicateError::NoActiveTab)?;
        let group = tree
            .active_group
            .get(&strip.id)
            .and_then(|g| strip.window_groups.iter().find(|w| w.id == *g))
            .or_else(|| strip.window_groups.first())
            .ok_or(DuplicateError::NoActiveTab)?;
        let tab = group
            .tabs
            .iter()
            .find(|t| t.id == group.active_tab)
            .or_else(|| group.tabs.first())
            .ok_or(DuplicateError::NoActiveTab)?;
        match &tab.tile {
            TileContent::WorkspaceActivity => {
                return Err(DuplicateError::ForeignWindow);
            }
            TileContent::AppPlaceholder { kind, label } | TileContent::Stub { kind, label } => {
                (kind.clone(), label.clone(), group.id)
            }
            // Neither can be duplicated: a live client's surface, or a
            // window belonging to somebody else's machine.
            TileContent::Foreign { .. }
            | TileContent::PeerWindow { .. }
            | TileContent::LocalApp { .. } => {
                return Err(DuplicateError::ForeignWindow);
            }
        }
    };
    let placement = Placement::TabIn(group);
    let id = {
        let mut tree = st.tree_mut();
        tab_binding::open_hosted_app(&mut tree, kind, title, placement)
    };
    st.bump();
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, WorkspaceTree, next_id};
    use std::cell::RefMut;

    fn chrome_with_default_tree() -> crate::shell::chrome_state::Chrome {
        ChromeState::new(WorkspaceTree::bootstrap_demo())
    }

    /// The group the user is looking at, and the number of tabs in it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the borrow is used for exactly one chained expression that is this fn's tail \
                  return value; there is no later point to drop it earlier than"
    )]
    fn focused_group_tab_count(st: &ChromeState) -> usize {
        let g = focused_group(st).expect("a focused group");
        let tree = st.tree();
        tree.active_workspace()
            .unwrap()
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|w| w.id == g)
            .map(|w| w.tabs.len())
            .unwrap()
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the borrow feeds a chain of derived references (strip -> window_groups -> \
                  group -> tab) used through the assertions right after; ending it earlier \
                  would mean cloning test fixture data for no real gain"
    )]
    fn open_terminal_here_adds_a_tab_to_the_focused_group() {
        let chrome = chrome_with_default_tree();
        let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
        let before = focused_group_tab_count(&st);
        let group = focused_group(&st).unwrap();
        let id = open_terminal(&mut st, Destination::Here);
        assert_eq!(
            focused_group_tab_count(&st),
            before + 1,
            "the terminal joined the group the user was looking at"
        );
        let tree = st.tree();
        let g = tree
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|w| w.id == group)
            .unwrap();
        assert_eq!(g.active_tab, id, "the new tab is focused");
        let tab = g.tabs.iter().find(|t| t.id == id).unwrap();
        assert!(
            matches!(
                &tab.tile,
                TileContent::AppPlaceholder {
                    kind: AppKind::Terminal,
                    ..
                }
            ),
            "a real terminal placeholder, not a stub: {:?}",
            tab.tile
        );
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each borrow feeds a chain of derived references (strip -> window_groups -> \
                  original) used through the assertion right after; ending it earlier would mean \
                  cloning test fixture data for no real gain"
    )]
    fn open_terminal_beside_leaves_the_focused_group_alone_when_it_holds_one_tab() {
        let chrome = chrome_with_default_tree();
        let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
        // Collapse to a single tab so the "open beside" branch is under test.
        {
            let mut tree = st.tree_mut();
            let ws = tree.active_workspace_mut().unwrap();
            let g = &mut ws.strips[0].window_groups[0];
            g.tabs.truncate(1);
            g.active_tab = g.tabs[0].id;
        }
        let group = focused_group(&st).unwrap();
        let groups_before = {
            let tree = st.tree();
            tree.active_workspace().unwrap().strips[0]
                .window_groups
                .len()
        };
        open_terminal(&mut st, Destination::Beside);
        let tree = st.tree();
        let strip = &tree.active_workspace().unwrap().strips[0];
        assert_eq!(
            strip.window_groups.len(),
            groups_before + 1,
            "a single-window group gains a neighbour, not a tab"
        );
        let original = strip.window_groups.iter().find(|g| g.id == group).unwrap();
        assert_eq!(original.tabs.len(), 1, "the original group is untouched");
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each borrow feeds a chain of derived references used through the assertion \
                  right after; ending it earlier would mean cloning test fixture data for no \
                  real gain"
    )]
    fn duplicate_active_tab_copies_the_focused_app_into_the_same_group() {
        let chrome = chrome_with_default_tree();
        let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
        let group = focused_group(&st).unwrap();
        // Focus the Terminal tab so the duplicate has a known kind to copy.
        let terminal_tab = {
            let mut tree = st.tree_mut();
            let ws = tree.active_workspace_mut().unwrap();
            let g = ws.strips[0]
                .window_groups
                .iter_mut()
                .find(|g| g.id == group)
                .unwrap();
            let id = g
                .tabs
                .iter()
                .find(|t| {
                    matches!(
                        &t.tile,
                        TileContent::AppPlaceholder {
                            kind: AppKind::Terminal,
                            ..
                        }
                    )
                })
                .map(|t| t.id)
                .expect("the default tree seeds a terminal");
            g.active_tab = id;
            id
        };
        let before = focused_group_tab_count(&st);
        let new_id = duplicate_active_tab(&mut st).expect("a hosted tab duplicates");
        assert_ne!(new_id, terminal_tab, "a duplicate is a new tab");
        assert_eq!(focused_group_tab_count(&st), before + 1);
        let tree = st.tree();
        let g = tree.active_workspace().unwrap().strips[0]
            .window_groups
            .iter()
            .find(|g| g.id == group)
            .unwrap();
        let dup = g.tabs.iter().find(|t| t.id == new_id).unwrap();
        assert!(
            matches!(
                &dup.tile,
                TileContent::AppPlaceholder {
                    kind: AppKind::Terminal,
                    ..
                }
            ),
            "the duplicate is the same app kind: {:?}",
            dup.tile
        );
        assert_eq!(g.active_tab, new_id, "the duplicate takes focus");
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the mutable borrow's only job in this block is building the fixture; it's \
                  already dropped at the block's closing brace, right after its last use"
    )]
    fn duplicating_a_foreign_window_reports_why_it_cannot() {
        let chrome = chrome_with_default_tree();
        let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
        {
            let mut tree = st.tree_mut();
            let ws = tree.active_workspace_mut().unwrap();
            let g = &mut ws.strips[0].window_groups[0];
            let id = next_id();
            g.tabs.push(Tab {
                id,
                title: "Foreign".into(),
                tile: TileContent::Foreign {
                    app_id: "org.example.thing".into(),
                    surface_id: 7,
                },
            });
            g.active_tab = id;
        }
        assert_eq!(
            duplicate_active_tab(&mut st),
            Err(DuplicateError::ForeignWindow),
            "a client-owned window cannot be cloned by the compositor"
        );
    }

    #[test]
    fn opening_a_hosted_app_is_durable_for_restart_and_peer_export() {
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        let chrome = chrome_with_default_tree();
        let mut st = chrome.borrow_mut();
        let baseline = {
            let mut tree = st.tree_mut();
            tree.persist_path = Some(state_path.clone());
            tree.current_heads()
        };
        let mut replica = st.tree().clone();

        let tab_id = open_terminal(&mut st, Destination::Beside);
        let (changes, restarted) = {
            let mut tree = st.tree_mut();
            let changes = tree.export_changes_since(&baseline);
            tree.flush_to_disk();
            drop(tree);
            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("opening a hosted app must leave a loadable state file");
            (changes, restarted)
        };

        replica
            .apply_changes(&changes)
            .expect("hosted app creation must be present in peer export");

        for (authority, tree) in [("restart", &restarted), ("peer export", &replica)] {
            assert!(
                tree.workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .any(|tab| tab.id == tab_id),
                "a hosted app opened in the chrome must survive the {authority} authority read"
            );
        }
    }

    #[test]
    fn switching_workspace_is_durable_across_restart() {
        let state_dir = tempfile::tempdir().expect("state directory");
        let state_path = state_dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.workspaces
            .first_mut()
            .expect("demo has a first workspace")
            .name = "Home (workspace persistence test)".into();
        tree.persist_path = Some(state_path.clone());
        let first = tree
            .workspaces
            .first()
            .expect("demo has a first workspace")
            .id;
        let target = tree
            .workspaces
            .get(1)
            .expect("demo has a second workspace")
            .id;

        tree.set_active(target);

        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("workspace selection must leave a loadable state file");
        assert_eq!(
            restarted.active, target,
            "the workspace selected by the user must be the one reopened"
        );

        let mut moved_up = restarted;
        moved_up.persist_path = Some(state_path.clone());
        assert!(moved_up.switch_up());
        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("moving to the previous workspace must persist immediately");
        assert_eq!(restarted.active, first);

        let mut moved_down = restarted;
        moved_down.persist_path = Some(state_path.clone());
        assert!(moved_down.switch_down());
        let restarted = crate::shell::persist::load_tree_from(&state_path)
            .expect("moving to the next workspace must persist immediately");
        assert_eq!(restarted.active, target);
    }
}
