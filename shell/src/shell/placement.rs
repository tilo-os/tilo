//! Where a new window lands in the strip.
//!
//! Opening a window is the moment the layout decides something on the
//! user's behalf, and the strip has unlimited space to the right, so
//! "wherever the tree walk reached first" is not an answer. The rule is:
//!
//! 1. **Right of the window you are in.** A new window goes immediately
//!    to the right of the selected one, not at the end of the strip —
//!    the thing you just opened should be next to the thing you opened
//!    it from.
//! 2. **Into the group you are in, when that group is tabbed.** If the
//!    selection is one tab of several, the person is working *inside*
//!    that group; the new window joins it as a tab immediately right of
//!    the selected one rather than splitting the layout into another
//!    column.
//! 3. **At a seam, when nothing is selected.** With no selection there
//!    is no "here", so the window goes into the space between windows:
//!    a border between two groups that is currently on screen, else the
//!    next border going rightwards.
//!
//! Rules 1 and 2 read the tree alone. Rule 3 needs to know what is on
//! screen, which is a layout fact — [`StripViewport`] carries it, and
//! the shell publishes one on [`crate::shell::chrome_state::ChromeState`]
//! every frame.
//!
//! [`placement_for_new_window`] is the whole decision and the only place
//! it is made. Callers that open a window record its answer for the
//! launch ([`note_placement_for_launch`]); the adoption of the toplevel
//! reads the note back (`crate::shell::launcher::placement_for_toplevel`)
//! and splices the window in.

use crate::compositor::tab_binding::Placement;
use crate::shell::chrome_state::OpenTarget;
use crate::shell::state::{GroupId, StripId, WorkspaceTree};

/// One window group's horizontal extent in the strip's **unscrolled
/// canvas space** — the same space [`ui::widget::GroupRow::usable_at`]
/// resolves widths in, so a seam's position does not move when the user
/// pans the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupSpan {
    pub id: GroupId,
    pub x0: f64,
    pub x1: f64,
}

/// A border between windows, and where it sits on the strip's canvas.
///
/// The seam *after* the last group counts: the strip is unbounded to the
/// right, so the last window's trailing edge is a border between a
/// window and the open space beyond it. That is the seam "rightwards"
/// always terminates at, which is what keeps rule 3 total.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seam {
    /// The group whose right edge this seam is.
    pub after: GroupId,
    /// Canvas-x of the border.
    pub x: f64,
}

/// What the person can currently see of one strip: where its groups sit
/// on the canvas, and which part of that canvas the displays are showing.
///
/// Built by the shell from the live [`ui::widget::GroupRow`] — see
/// [`crate::shell::overlay_host::ShellOverlayHost`] — and published on
/// the chrome so a launch can consult it without reaching into the
/// widget tree.
#[derive(Clone, Debug, PartialEq)]
pub struct StripViewport {
    /// The strip these spans belong to.
    pub strip: StripId,
    /// Every group in the strip, in strip order.
    pub groups: Vec<GroupSpan>,
    /// Canvas-x of the left edge of what is on screen.
    pub visible_x0: f64,
    /// Canvas-x of the right edge of what is on screen.
    pub visible_x1: f64,
}

impl StripViewport {
    /// Every border between windows, left to right, including the one
    /// after the last group. Empty only when the strip has no groups.
    pub fn seams(&self) -> Vec<Seam> {
        self.groups
            .iter()
            .map(|g| Seam {
                after: g.id,
                x: g.x1,
            })
            .collect()
    }

    /// The leftmost border **between two windows** that the user can
    /// actually see a space at.
    ///
    /// Two things this deliberately excludes.
    ///
    /// A seam lying exactly on an edge of the screen is not a space you
    /// can see: the window on its far side is entirely off the display.
    /// That is not a corner case — a full-width window puts its trailing
    /// border precisely on the right edge, which is the resting state of
    /// a default strip. So the bound is strict, and that case falls
    /// through to [`Self::next_seam_rightwards`], which lands the window
    /// immediately after the one filling the screen.
    ///
    /// And the seam after the *last* window, which is a border between a
    /// window and open space rather than between two windows. It is
    /// still where "rightwards" terminates — see
    /// [`Self::next_seam_rightwards`] — just not something to call
    /// visible.
    ///
    /// Leftmost rather than nearest-to-centre: the strip reads left to
    /// right, so the first gap you can see is the one you would point at
    /// if asked where the space between the windows is.
    pub fn visible_seam(&self) -> Option<Seam> {
        let seams = self.seams();
        let between_windows = seams.len().saturating_sub(1);
        seams
            .into_iter()
            .take(between_windows)
            .find(|s| s.x > self.visible_x0 && s.x < self.visible_x1)
    }

    /// The first seam at or past the right edge of the screen — where a
    /// window goes when no space between windows is visible, which is
    /// the ordinary case for a full-width window.
    ///
    /// Falls back to the rightmost seam when every seam is already
    /// behind the viewport (the user has panned into the empty space
    /// past the last window): "rightwards" runs out at the end of the
    /// strip, and that end is still a seam.
    pub fn next_seam_rightwards(&self) -> Option<Seam> {
        let seams = self.seams();
        seams
            .iter()
            .copied()
            .find(|s| s.x >= self.visible_x1)
            .or_else(|| seams.last().copied())
    }
}

/// Where the next window opened in `tree` should land.
///
/// `viewport` is the layout snapshot for the active strip, when the
/// caller has one. It is only consulted for rule 3 — with a selection
/// the answer is a property of the tree alone. Passing `None` (a caller
/// with no layout to hand, such as an adoption arriving straight off the
/// Wayland protocol) degrades rule 3 to the strip's trailing seam, which
/// is where "the next seam rightwards" ends up once no other seam
/// qualifies.
pub fn placement_for_new_window(
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) -> Placement {
    let Some(ws) = tree.active_workspace() else {
        return Placement::NewGroup;
    };
    // The strip a new window lands in is the focused one, falling back
    // to the first — the same resolution `adopt_toplevel_at` performs
    // when it splices the window in, so the two cannot disagree about
    // which strip the decision was about.
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
        // A group holding more than one tab is a group the person is
        // working *in*, so the new window joins it. A single-window
        // group is just that window, and a second window beside it is
        // the strip doing its job.
        if selected.tabs.len() > 1 {
            return Placement::TabIn(selected.id);
        }
        return Placement::RightOf(selected.id);
    }

    // Rule 3: nothing is selected, so the window goes into the space
    // between windows.
    if let Some(view) = viewport.filter(|v| v.strip == strip.id) {
        if let Some(seam) = view.visible_seam().or_else(|| view.next_seam_rightwards()) {
            return Placement::RightOf(seam.after);
        }
    }
    // No layout to consult, or a strip with no groups at all: the end of
    // the strip is the only seam left.
    Placement::NewGroup
}

/// Record where the window of the launch about to start should land.
///
/// The one call an "open this app" path needs. A launch and the toplevel
/// it eventually maps are two events on two processes; this leaves the
/// decision as a note for the adoption to read, so the rule is applied
/// once, at the moment the user asked, against the layout they were
/// looking at.
pub fn note_placement_for_launch(
    package_id: &str,
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) {
    let placement = placement_for_new_window(tree, viewport);
    crate::shell::launcher::set_pending_placement(package_id, placement);
}

/// [`note_placement_for_launch`] for a launch whose window identities
/// cannot be derived from its id.
///
/// A `.qpkg`'s wayland app-id follows its package id by convention, so
/// the qOS path derives both from one string. A host application's
/// cannot be derived from anything qOS controls — the `.desktop` entry
/// states it, or the toolkit picks the desktop-file id, or a Flatpak
/// reports its application id — so the caller that read the entry
/// passes them in. Without this a foreign toplevel matches no note and
/// lands wherever the default puts it, which is the bug: the user asked
/// for it somewhere specific and the window ignored them.
pub fn note_placement_for_launch_identities(
    key: &str,
    identities: &[String],
    tree: &WorkspaceTree,
    viewport: Option<&StripViewport>,
) {
    let placement = placement_for_new_window(tree, viewport);
    crate::shell::launcher::set_pending_placement_for_identities(key, identities, placement);
}

/// [`note_placement_for_launch`], with an explicit destination allowed to
/// override the rule.
///
/// The rule infers where a window belongs from what is selected. That is
/// the right answer when the user did not say — but the "+" on a group's
/// tab strip *does* say, and naming a destination has to beat inferring
/// one. Notably [`OpenTarget::Here`] lands a tab in a single-window group,
/// which the rule would never do (it opens beside instead, so a lone
/// window never silently acquires a tab bar).
///
/// A target naming a group that has since closed falls back to the rule
/// rather than to `NewGroup`, so a stale aim degrades to the ordinary
/// behaviour instead of to an arbitrary one.
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

/// Whether `group` is still a live window group in the active workspace.
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
            let active = tabs.first().map(|t| t.id).unwrap_or(0);
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
    fn viewport(
        strip: StripId,
        ids: &[GroupId],
        visible: (f64, f64),
    ) -> StripViewport {
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
        // The screen shows canvas 120..190 — inside group 1, with its
        // left border behind the viewport and its right one ahead of it.
        // That is what a full-width window scrolled into the middle of a
        // strip looks like: no border between windows anywhere on screen.
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
    /// between the window you are looking at and the next one lands
    /// exactly on the screen's right edge. Nothing of the space is on
    /// screen, so the window goes after the one filling it — not two
    /// windows along.
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

    // -----------------------------------------------------------------
    // End to end: the note a launch leaves, read back by the adoption
    // that splices the window in. Same two calls the palette's
    // Applications row and the compositor's `new_toplevel` make, in the
    // same order. `PENDING_PLACEMENT` is process-global, so these are
    // serialised against each other and against the launcher's own tests.
    // -----------------------------------------------------------------

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
        crate::compositor::tab_binding::adopt_toplevel(
            tree,
            surface_id,
            Some(app_id.to_string()),
        );
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
            "dev.qos.seamfixture",
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
        // The screen shows canvas 120..190 — the inside of group 1, its
        // left border behind the viewport and its right one ahead of it.
        // No border between windows is on screen at all.
        let view = viewport(strip_id, &ids, (120.0, 190.0));

        let (order, landed) = launch_and_adopt(
            &mut tree,
            "dev.qos.nextseamfixture",
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
            "dev.qos.besidefixture",
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
            "dev.qos.tabbedfixture",
            "tabbedfixture",
            9104,
            Some(&view),
        );

        assert_eq!(landed, ids[1], "the window joined the selected group");
        assert_eq!(
            order, ids,
            "and the strip did not grow a column to hold it"
        );
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
