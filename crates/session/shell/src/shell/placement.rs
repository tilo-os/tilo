//! Where a new window lands in the strip, which has unlimited space to
//! the right, so "wherever the tree walk reached first" isn't an answer:
//!
//! 1. **Right of the window you're in** — next to what opened it, not
//!    at the end of the strip.
//! 2. **Into the group you're in, when it's tabbed** — a tab of several
//!    means the person is working *inside* that group.
//! 3. **At a seam, when nothing is selected** — the visible border
//!    between two groups, else the next one rightwards.
//!
//! Rules 1–2 read the tree alone; rule 3 needs [`StripViewport`], the
//! layout fact the shell publishes on
//! [`crate::shell::chrome_state::ChromeState`] every frame.
//!
//! [`placement_for_new_window`] is the whole decision, made once.
//! Callers record its answer for the launch
//! ([`note_placement_for_launch`]); the toplevel adoption reads the
//! note back and splices the window in.

use crate::shell::chrome_state::OpenTarget;
use crate::shell::state::{GroupId, StripId, WorkspaceTree};
use crate::shell::tab_binding::Placement;

/// In the strip's **unscrolled canvas space** — the same space
/// [`ui::widget::GroupRow::usable_at`] resolves widths in, so a seam's
/// position doesn't move when the user pans the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupSpan {
    pub id: GroupId,
    pub x0: f64,
    pub x1: f64,
}

/// A border between windows. The seam *after* the last group counts —
/// the strip is unbounded to the right, so the trailing edge is a
/// border against open space, which is where "rightwards" terminates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seam {
    /// The group whose right edge this seam is.
    pub after: GroupId,
    pub x: f64,
}

/// What the person can currently see of one strip. Built by the shell
/// from the live [`ui::widget::GroupRow`] and published on the chrome
/// so a launch can consult it without reaching into the widget tree.
#[derive(Clone, Debug, PartialEq)]
pub struct StripViewport {
    pub strip: StripId,
    /// Every group, in strip order.
    pub groups: Vec<GroupSpan>,
    /// Canvas-x of the visible edges.
    pub visible_x0: f64,
    pub visible_x1: f64,
}

impl StripViewport {
    /// Left to right, including the one after the last group.
    pub fn seams(&self) -> Vec<Seam> {
        self.groups
            .iter()
            .map(|g| Seam {
                after: g.id,
                x: g.x1,
            })
            .collect()
    }

    /// The leftmost border **between two windows** actually visible.
    /// Excludes a seam sitting exactly on the screen edge (falls
    /// through to [`Self::next_seam_rightwards`]) and the one after the
    /// *last* window, a border against open space.
    pub fn visible_seam(&self) -> Option<Seam> {
        let seams = self.seams();
        let between_windows = seams.len().saturating_sub(1);
        seams
            .into_iter()
            .take(between_windows)
            .find(|s| s.x > self.visible_x0 && s.x < self.visible_x1)
    }

    /// Falls back to the rightmost seam when every seam is behind the
    /// viewport — the end of the strip is still a seam.
    pub fn next_seam_rightwards(&self) -> Option<Seam> {
        let seams = self.seams();
        seams
            .iter()
            .copied()
            .find(|s| s.x >= self.visible_x1)
            .or_else(|| seams.last().copied())
    }
}

/// `viewport` is only consulted for rule 3 — with a selection the
/// answer is a property of the tree alone. `None` (a caller with no
/// layout, e.g. an adoption straight off the Wayland protocol) degrades
/// rule 3 to the strip's trailing seam.
pub fn placement_for_new_window(
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) -> Placement {
    let Some(ws) = tree.active_workspace() else {
        return Placement::NewGroup;
    };
    // The same resolution `adopt_toplevel_at` performs when splicing the
    // window in, so the two cannot disagree about which strip this was about.
    let strip = match tree
        .active_strip
        .get(&ws.id)
        .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
    {
        Some(s) => s,
        None => match ws.strips.first() {
            Some(s) => s,
            None => return Placement::NewGroup,
        },
    };

    // Rules 1 and 2: there is a selection.
    if let Some(selected) = tree
        .active_group_for(strip.id)
        .and_then(|id| strip.window_groups.iter().find(|g| g.id == id))
    {
        if selected.tabs.len() > 1 {
            return Placement::TabIn(selected.id);
        }
        return Placement::RightOf(selected.id);
    }

    // Rule 3: nothing is selected.
    if let Some(view) = viewport.filter(|v| v.strip == strip.id)
        && let Some(seam) = view.visible_seam().or_else(|| view.next_seam_rightwards())
    {
        return Placement::RightOf(seam.after);
    }
    Placement::NewGroup
}

/// A launch and the toplevel it eventually maps are two events on two
/// processes; this leaves the decision as a note the adoption reads,
/// applied once against the layout the user was looking at.
pub fn note_placement_for_launch(
    package_id: &str,
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) {
    let placement = placement_for_new_window(tree, viewport);
    crate::shell::launcher::set_pending_placement(package_id, placement);
}

/// [`note_placement_for_launch`] for a launch whose window identities
/// cannot be derived from its id — a `.tapp`'s wayland app-id follows
/// its package id by convention, but a host app's cannot, so the caller
/// that read the `.desktop`/Flatpak id passes it in instead.
pub fn note_placement_for_launch_identities(
    key: &str,
    identities: &[String],
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) -> Placement {
    let placement = placement_for_new_window(tree, viewport);
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
        _ => placement_for_new_window(tree, viewport),
    };
    crate::shell::launcher::set_pending_placement(package_id, placement);
}

fn group_exists(tree: &WorkspaceTree, group: GroupId) -> bool {
    tree.active_workspace().is_some_and(|ws| {
        ws.strips
            .iter()
            .any(|s| s.window_groups.iter().any(|g| g.id == group))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree, next_id};

    /// A strip of `tab_counts.len()` groups, the i-th holding
    /// `tab_counts[i]` stub tabs. Returns the group ids in strip order.
    fn strip_of(tree: &mut WorkspaceTree, tab_counts: &[usize]) -> Vec<GroupId> {
        let strip = &mut tree.workspaces[0].strips[0];
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

    #[test]
    fn a_single_window_selection_opens_beside_it() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, ids[1]);
        assert_eq!(
            placement_for_new_window(&tree, None),
            Placement::RightOf(ids[1]),
        );
    }

    #[test]
    fn a_tab_selection_inside_a_tabbed_group_opens_in_that_group() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 3, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, ids[1]);
        assert_eq!(
            placement_for_new_window(&tree, None),
            Placement::TabIn(ids[1]),
        );
    }

    #[test]
    fn no_selection_lands_at_the_visible_seam() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
        // Screen shows canvas 150..280: the seam at 200 (after group 1)
        // is on it; the one at 100 is behind the left edge.
        let view = viewport(strip_id, &ids, (150.0, 280.0));
        assert_eq!(
            placement_for_new_window(&tree, Some(&view)),
            Placement::RightOf(ids[1]),
        );
    }

    #[test]
    fn no_selection_and_no_visible_seam_takes_the_next_seam_rightwards() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
        // Inside group 1: no border between windows anywhere on screen.
        let view = viewport(strip_id, &ids, (120.0, 190.0));
        assert_eq!(
            placement_for_new_window(&tree, Some(&view)),
            Placement::RightOf(ids[1]),
        );
    }

    #[test]
    fn a_lone_full_width_window_puts_the_next_one_after_it() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
        // One group filling the display: the only seam is its own
        // trailing edge, and it is the one the window goes to.
        let view = viewport(strip_id, &ids, (0.5, 99.5));
        assert_eq!(
            placement_for_new_window(&tree, Some(&view)),
            Placement::RightOf(ids[0]),
        );
    }

    /// The resting state of a strip of full-width windows: the border
    /// lands exactly on the screen's right edge, so the window goes
    /// after the one filling it — not two windows along.
    #[test]
    fn a_border_exactly_on_the_screen_edge_is_not_a_space_you_can_see() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
        let view = viewport(strip_id, &ids, (0.0, 100.0));
        assert_eq!(view.visible_seam(), None, "nothing of the gap is on screen");
        assert_eq!(
            placement_for_new_window(&tree, Some(&view)),
            Placement::RightOf(ids[0]),
        );
    }

    #[test]
    fn panning_past_the_last_window_still_lands_at_the_end() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
        // Panned into empty space: every seam is behind the viewport.
        let view = viewport(strip_id, &ids, (400.0, 800.0));
        assert_eq!(
            placement_for_new_window(&tree, Some(&view)),
            Placement::RightOf(ids[1]),
        );
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
        let landed = tree.workspaces[0].strips[0]
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
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|g| g.id)
                .collect(),
            landed,
        )
    }

    #[test]
    #[serial_test::serial]
    fn a_launch_with_no_selection_lands_at_the_seam_the_user_can_see() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
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

    #[test]
    #[serial_test::serial]
    fn a_launch_with_no_visible_seam_lands_at_the_next_one_rightwards() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
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

    #[test]
    #[serial_test::serial]
    fn a_launch_beside_a_lone_selected_window_lands_right_of_it() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, ids[0]);
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

    #[test]
    #[serial_test::serial]
    fn a_launch_from_inside_a_tabbed_group_joins_it_rather_than_the_seam() {
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 3, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.set_active_group(strip_id, ids[1]);
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

    #[test]
    fn a_viewport_for_another_strip_is_ignored() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ids = strip_of(&mut tree, &[1, 1, 1]);
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.active_strip.insert(tree.active, strip_id);
        tree.active_group.remove(&strip_id);
        let mut view = viewport(strip_id, &ids, (150.0, 280.0));
        view.strip = StripId::from(next_id());
        assert_eq!(
            placement_for_new_window(&tree, Some(&view)),
            Placement::NewGroup,
            "a snapshot of a different strip must not decide this one",
        );
    }
}
