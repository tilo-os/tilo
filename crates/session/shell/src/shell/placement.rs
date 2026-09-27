//! The shell's side of window placement. The rule itself — beside the
//! selection, into a tabbed group, else at the seam in view — is
//! [`tilo_desktop_model::commands::open_app_placement`], shared with every
//! other desktop host. This turns its answer into the splice [`Placement`]
//! and leaves it as a note for the launch: a launch and the toplevel it
//! eventually maps are two events on two processes, so the decision is made
//! once, against the layout the person was looking at, and the adoption
//! reads it back ([`crate::shell::launcher::placement_for_toplevel`]).
//!
//! The viewport is the [`StripViewport`] the shell publishes on
//! [`crate::shell::chrome_state::ChromeState`] every frame; `None` (a caller
//! with no layout, e.g. an adoption straight off the Wayland protocol)
//! degrades the seam rule to the strip's trailing edge.

use tilo_desktop_model::commands::{
    OpenContext, OpenIntent, StripViewport, TabPlacement, open_app_placement,
};

use crate::shell::chrome_state::OpenTarget;
use crate::shell::state::{GroupId, WorkspaceTree};
use crate::shell::tab_binding::Placement;

/// Where a window opened with `intent` lands in the active workspace.
pub fn placement_for(
    tree: &WorkspaceTree,
    intent: OpenIntent,
    viewport: Option<&StripViewport>,
) -> Placement {
    let Some(workspace) = tree.desktop.active_workspace() else {
        return Placement::NewGroup;
    };
    let at = OpenContext::focused(&tree.desktop, intent, viewport);
    match open_app_placement(workspace, &at, |_| true) {
        Ok(TabPlacement::InGroup { group, .. }) => Placement::TabIn(group),
        Ok(TabPlacement::NewGroup {
            after: Some(group), ..
        }) => Placement::RightOf(group),
        Ok(TabPlacement::NewGroup { after: None, .. } | TabPlacement::EmptyWorkspace) | Err(_) => {
            Placement::NewGroup
        }
    }
}

/// Record the launch rule's answer for `package_id`'s window.
pub fn note_placement_for_launch(
    package_id: &str,
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) {
    let placement = placement_for(tree, OpenIntent::Launch, viewport);
    crate::shell::launcher::set_pending_placement(package_id, placement);
}

/// Search-result placement for a host launch whose window identities
/// cannot be derived from its id — a `.tapp`'s Wayland app-id follows
/// its package id by convention, but a host app's cannot, so the caller
/// that read the `.desktop`/Flatpak id passes it in instead.
pub fn note_placement_for_launch_identities(
    key: &str,
    identities: &[String],
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) -> Placement {
    let placement = placement_for(tree, OpenIntent::Search, viewport);
    crate::shell::launcher::set_pending_placement_for_identities(key, identities, placement);
    placement
}

/// An explicit destination (the "+" on a group's tab strip) beats
/// inferring one. [`OpenTarget::Here`] lands a tab in a single-window
/// group, which the rule alone would never do. A group that has since
/// closed falls back to the rule rather than to `NewGroup`.
pub fn note_placement_for_launch_targeted(
    package_id: &str,
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
    target: OpenTarget,
) {
    let placement = match target {
        OpenTarget::Here(group) if group_exists(tree, group) => Placement::TabIn(group),
        _ => placement_for(tree, OpenIntent::Search, viewport),
    };
    crate::shell::launcher::set_pending_placement(package_id, placement);
}

fn group_exists(tree: &WorkspaceTree, group: GroupId) -> bool {
    tree.desktop.active_workspace().is_some_and(|ws| {
        ws.strips
            .iter()
            .any(|s| s.window_groups.iter().any(|g| g.id == group))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{
        StripId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree, next_id,
    };
    use tilo_desktop_model::commands::GroupSpan;

    /// A strip of `tab_counts.len()` groups, the i-th holding
    /// `tab_counts[i]` stub tabs. Returns the group ids in strip order.
    fn strip_of(tree: &mut WorkspaceTree, tab_counts: &[usize]) -> Vec<GroupId> {
        let strip = &mut tree.desktop.workspaces[0].strips[0];
        strip.window_groups.clear();
        let mut ids = Vec::new();
        for count in tab_counts {
            let tabs: Vec<Tab> = (0..*count)
                .map(|_| Tab {
                    id: next_id(),
                    title: "t".into(),
                    tile: TileContent::Stub {
                        kind: crate::shell::state::AppKind::Terminal,
                        label: "t".into(),
                    },
                })
                .collect();
            let active = tabs.first().map_or(0, |t| t.id);
            let group = WindowGroup {
                id: next_id(),
                tabs,
                active_tab: active,
                width: WindowWidth::Normal,
                maximized: false,
            };
            ids.push(group.id);
            strip.window_groups.push(group);
        }
        ids
    }

    /// Even spans, 100 wide each, for a strip of `ids`.
    #[allow(
        clippy::cast_precision_loss,
        reason = "test fixture: ids.len() is a handful of groups, nowhere near f64's precision limit"
    )]
    fn viewport(strip: StripId, ids: &[GroupId], visible: (f64, f64)) -> StripViewport {
        StripViewport {
            strip,
            groups: ids
                .iter()
                .enumerate()
                .map(|(i, id)| GroupSpan {
                    id: *id,
                    x0: i as f64 * 100.0,
                    x1: i as f64 * 100.0 + 100.0,
                })
                .collect(),
            visible_x0: visible.0,
            visible_x1: visible.1,
        }
    }

    // End to end: the note a launch leaves, read back by the adoption
    // that splices the window in. `PENDING_PLACEMENT` is process-global,
    // so these are serialised against the launcher's own tests.

    /// Adopt a window for `app_id` and report the strip's group ids and
    /// the group the window landed in.
    fn launch_and_adopt(
        tree: &mut WorkspaceTree,
        package_id: &str,
        app_id: &str,
        surface_id: u64,
        viewport: Option<&StripViewport>,
    ) -> (Vec<GroupId>, GroupId) {
        note_placement_for_launch(package_id, tree, viewport);
        crate::shell::tab_binding::adopt_toplevel(tree, surface_id, Some(app_id.to_string()));
        let landed = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|g| {
                g.tabs.iter().any(|t| {
                    matches!(
                        &t.tile,
                        TileContent::Foreign { surface_id: s, .. } if *s == surface_id
                    )
                })
            })
            .expect("the adopted window lives in some group")
            .id;
        (
            tree.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|g| g.id)
                .collect(),
            landed,
        )
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn launches_land_where_placement_says_until_they_map() {
        fn a_launch_with_no_selection_lands_at_the_seam_the_user_can_see() {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let ids = strip_of(&mut tree, &[1, 1, 1]);
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop
                .active_strip
                .insert(tree.desktop.active, strip_id);
            tree.desktop.active_group.remove(&strip_id);
            // The screen shows canvas 150..280 — the border after group 1 is
            // on it, the one after group 0 is off the left edge.
            let view = viewport(strip_id, &ids, (150.0, 280.0));

            let (order, landed) = launch_and_adopt(
                &mut tree,
                "so.tilo.seamfixture",
                "seamfixture",
                9101,
                Some(&view),
            );

            assert_eq!(
                order,
                vec![ids[0], ids[1], landed, ids[2]],
                "the window goes into the space at the visible border, \
                 between the groups that border separates"
            );
            crate::shell::launcher::clear_pending_placements();
        }

        fn a_launch_with_no_visible_seam_lands_at_the_next_one_rightwards() {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let ids = strip_of(&mut tree, &[1, 1, 1]);
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop
                .active_strip
                .insert(tree.desktop.active, strip_id);
            tree.desktop.active_group.remove(&strip_id);
            let view = viewport(strip_id, &ids, (120.0, 190.0));

            let (order, landed) = launch_and_adopt(
                &mut tree,
                "so.tilo.nextseamfixture",
                "nextseamfixture",
                9102,
                Some(&view),
            );

            assert_eq!(
                order,
                vec![ids[0], ids[1], landed, ids[2]],
                "with no border on screen the window takes the next one \
                 rightwards — after group 1, not before group 1"
            );
            crate::shell::launcher::clear_pending_placements();
        }

        fn a_launch_beside_a_lone_selected_window_lands_right_of_it() {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let ids = strip_of(&mut tree, &[1, 1, 1]);
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop
                .active_strip
                .insert(tree.desktop.active, strip_id);
            tree.desktop.set_active_group(strip_id, ids[0]);
            // A viewport that would send an unselected launch elsewhere, so
            // the selection is demonstrably what decided this.
            let view = viewport(strip_id, &ids, (150.0, 280.0));

            let (order, landed) = launch_and_adopt(
                &mut tree,
                "so.tilo.besidefixture",
                "besidefixture",
                9103,
                Some(&view),
            );

            assert_eq!(
                order,
                vec![ids[0], landed, ids[1], ids[2]],
                "a selection outranks the seam: the window opens beside what \
                 the user is in"
            );
            crate::shell::launcher::clear_pending_placements();
        }

        fn a_launch_from_inside_a_tabbed_group_joins_it_rather_than_the_seam() {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let ids = strip_of(&mut tree, &[1, 3, 1]);
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop
                .active_strip
                .insert(tree.desktop.active, strip_id);
            tree.desktop.set_active_group(strip_id, ids[1]);
            let view = viewport(strip_id, &ids, (150.0, 280.0));

            let (order, landed) = launch_and_adopt(
                &mut tree,
                "so.tilo.tabbedfixture",
                "tabbedfixture",
                9104,
                Some(&view),
            );

            assert_eq!(landed, ids[1], "the window joined the selected group");
            assert_eq!(order, ids, "and the strip did not grow a column to hold it");
            crate::shell::launcher::clear_pending_placements();
        }

        fn a_searched_app_keeps_its_rightmost_target_until_it_maps() {
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            let ids = strip_of(&mut tree, &[3, 1, 1, 1]);
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop
                .active_strip
                .insert(tree.desktop.active, strip_id);
            tree.desktop.set_active_group(strip_id, ids[0]);
            let view = viewport(strip_id, &ids, (0.0, 350.0));

            note_placement_for_launch_targeted(
                "dev.tilo.searchfixture",
                &tree,
                Some(&view),
                OpenTarget::Beside,
            );
            // Mapping is asynchronous. A later focus change must not move the
            // result away from the target captured when Enter was pressed.
            tree.desktop.set_active_group(strip_id, ids[3]);
            crate::shell::tab_binding::adopt_toplevel(
                &mut tree,
                9_105,
                Some("searchfixture".to_string()),
            );

            let groups = &tree.desktop.workspaces[0].strips[0].window_groups;
            let landed = groups
                .iter()
                .find(|group| {
                    group.tabs.iter().any(|tab| {
                        matches!(
                            &tab.tile,
                            TileContent::Foreign {
                                surface_id: 9_105,
                                ..
                            }
                        )
                    })
                })
                .expect("searched app maps into a group")
                .id;
            assert_eq!(
                groups.iter().map(|group| group.id).collect::<Vec<_>>(),
                vec![ids[0], ids[1], ids[2], landed, ids[3]],
                "the mapped app must retain the rightmost on-screen target captured by search",
            );
            crate::shell::launcher::clear_pending_placements();
        }

        a_launch_with_no_selection_lands_at_the_seam_the_user_can_see();
        a_launch_with_no_visible_seam_lands_at_the_next_one_rightwards();
        a_launch_beside_a_lone_selected_window_lands_right_of_it();
        a_launch_from_inside_a_tabbed_group_joins_it_rather_than_the_seam();
        a_searched_app_keeps_its_rightmost_target_until_it_maps();
    }
}
