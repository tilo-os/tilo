//! `GroupRow` — a horizontal flex container of `WindowGroup`s that also
//! routes cross-group tab drag-drop.
//!
//! Replacing the generic `Row` for the "groups inside a strip" layer lets
//! us intercept release events *after* every child has had a chance to
//! drain its own `TabBar`. If any child published a `pending_detach` (set
//! by `TabBar` when a drag gesture ends outside the bar's own rect), we
//! hit-test the release point against sibling bars and — if one matches —
//! splice the detached tab + its `TileHost` into the target group.
//!
//! Within-group reorder continues to live inside `TabBar` / `WindowGroup`;
//! this widget only adds the between-groups edge of the drag-drop story.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Widget, WindowGroup, WindowWidth};

pub struct GroupRow {
    pub groups: Vec<WindowGroup>,
    rect: Rect,
    /// Per-child taffy node ids, captured during `build_layout` so
    /// `apply_layout` can walk them back out in order.
    node_ids: Vec<NodeId>,
    /// Last seen cursor position in logical coords, used so a release
    /// that went to a sibling's bar (and therefore set `handled=true` on
    /// the target) still has a known release point to drive our
    /// post-dispatch detach sweep.
    last_cursor: Point,
    /// Set when this row's `self.groups` vector is mutated structurally
    /// (e.g. a tab is split into a brand-new `WindowGroup`). The App's
    /// tick handler polls via `take_structure_dirty` and rebuilds the
    /// taffy tree so the new group participates in layout / draw. Without
    /// this signal, `apply_layout` would walk `node_ids` that don't cover
    /// the new group and skip it entirely.
    structure_dirty: bool,
}

impl GroupRow {
    pub fn new(groups: Vec<WindowGroup>) -> Self {
        Self {
            groups,
            rect: Rect::ZERO,
            node_ids: Vec::new(),
            last_cursor: Point::ZERO,
            structure_dirty: false,
        }
    }

    /// Move a tab (by id) from `src` to `dst` so that the dropped-in tab
    /// lands at index `target_idx`. Returns `true` if the move happened,
    /// `false` if the `src` group had no such tab. Exposed separately
    /// from `handle_event` so tests can drive the transfer without
    /// having to stage a full winit event.
    pub fn transfer_tab(
        &mut self,
        src: usize,
        dst: usize,
        tab_id: u64,
        target_idx: usize,
    ) -> bool {
        if src == dst || src >= self.groups.len() || dst >= self.groups.len() {
            return false;
        }
        // Detach from src. We have to keep the mutable borrow scoped so
        // we can reborrow `dst` afterwards.
        let detached = {
            let src_group = &mut self.groups[src];
            src_group.detach_tab(tab_id)
        };
        let Some((desc, host)) = detached else {
            return false;
        };
        let dst_group = &mut self.groups[dst];
        dst_group.insert_detached_tab(target_idx, desc, host);
        true
    }

    /// Classify a drag release point against the row's inter-group drop
    /// zones. Returns `Some(gap_idx)` if the release landed strictly
    /// between two groups (or before the first / after the last) while
    /// still inside the row's vertical band; `None` otherwise.
    ///
    /// `gap_idx` counts gap slots left-to-right:
    /// - `0` → insert a new group before the current first group
    /// - `i` (for `1 ≤ i < N`) → insert between groups `i-1` and `i`
    /// - `N` → append a new group to the right of the current last group
    ///
    /// Only returns `Some(_)` when the release is in the *gap* between
    /// group rects (not inside any group), so a drop inside a group's
    /// tile area still falls through to the sibling-bar detection above.
    pub fn gap_index_for(&self, release_point: Point) -> Option<usize> {
        if self.groups.is_empty() {
            // Empty row → any drop inside its rect becomes the first group.
            return if self.rect.contains(release_point) { Some(0) } else { None };
        }
        // Y must be within the row's vertical band.
        if release_point.y < self.rect.y0 || release_point.y > self.rect.y1 {
            return None;
        }
        // Inside any group rect → not a gap drop.
        for g in &self.groups {
            if g.get_rect().contains(release_point) {
                return None;
            }
        }
        // Release is left of the first group → insert at 0.
        if release_point.x < self.groups[0].get_rect().x0 {
            return Some(0);
        }
        // Between pair (i-1, i)?
        for i in 1..self.groups.len() {
            let left = self.groups[i - 1].get_rect().x1;
            let right = self.groups[i].get_rect().x0;
            if release_point.x >= left && release_point.x <= right {
                return Some(i);
            }
        }
        // Release is right of the last group → append.
        if release_point.x > self.groups.last().unwrap().get_rect().x1 {
            return Some(self.groups.len());
        }
        None
    }

    /// Detach `tab_id` from `src` and splice it into a brand-new
    /// `WindowGroup` at `gap_idx`, pruning the source group if the
    /// detach left it empty. Returns `true` if the split happened.
    /// Exposed separately so tests can drive the flow without staging a
    /// full winit event sequence.
    pub fn split_tab_into_new_group(
        &mut self,
        src: usize,
        tab_id: u64,
        gap_idx: usize,
    ) -> bool {
        if src >= self.groups.len() {
            return false;
        }
        let detached = {
            let src_group = &mut self.groups[src];
            src_group.detach_tab(tab_id)
        };
        let Some((desc, host)) = detached else {
            return false;
        };
        let src_now_empty = self.groups[src].tab_count() == 0;
        let new_id = self
            .groups
            .iter()
            .map(|g| g.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let mut new_group = WindowGroup::from_detached(new_id, desc, host, WindowWidth::Normal);
        new_group.start_spawn_animation();
        let clamped = gap_idx.min(self.groups.len());
        self.groups.insert(clamped, new_group);
        // Flag a structural mutation so the App's tick handler rebuilds
        // the taffy tree. Without this, `apply_layout` would iterate the
        // stale `node_ids` (one per pre-split group) and skip the newly
        // inserted group entirely — the bug this whole field exists to fix.
        self.structure_dirty = true;
        if src_now_empty {
            // Source index shifts by 1 iff the new group was inserted
            // at or left of the source's original slot.
            let src_after = if clamped <= src { src + 1 } else { src };
            if src_after < self.groups.len() {
                self.groups.remove(src_after);
            }
        }
        true
    }
}

impl Widget for GroupRow {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for g in &mut self.groups {
            g.sync(state, root);
        }
    }

    fn update(&mut self, state: &mut AutoCommit, root: &ObjId) -> bool {
        let mut any = false;
        for g in &mut self.groups {
            if g.update(state, root) {
                any = true;
            }
        }
        any
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.node_ids.clear();
        let children: Vec<NodeId> = self
            .groups
            .iter_mut()
            .map(|g| g.build_layout(taffy, theme, node_map))
            .collect();
        self.node_ids = children.clone();
        // The row must fill the remaining vertical space inside the
        // parent Column so each child `WindowGroup`'s `height: percent(1.0)`
        // resolves to the full available height — otherwise the row only
        // measures as tall as the tallest child's natural height (i.e. the
        // 40 px TabBar) and the `TileHost` collapses to zero height,
        // hiding the concept's white card entirely.
        let node = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Row,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    gap: Size {
                        width: taffy::LengthPercentage::length(8.0),
                        height: taffy::LengthPercentage::length(0.0),
                    },
                    ..Default::default()
                },
                &children,
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        for (g, child_id) in self.groups.iter_mut().zip(self.node_ids.iter()) {
            g.apply_layout(taffy, *child_id, child_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for g in &mut self.groups {
            g.draw(scene, transform, tr, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track the cursor so our post-dispatch detach sweep can hit-test
        // sibling bars even after a child has swallowed the event.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Dispatch to every child; stop once one claims the event. We
        // deliberately *do not* bail out of the detach sweep even when a
        // child claimed — the claim is for pointer-capture / focus, while
        // the detach sweep is a separate "did anyone publish a detach?"
        // pass that runs regardless.
        for (i, g) in self.groups.iter_mut().enumerate() {
            g.handle_event(
                event, scale_factor, modifiers, state, root, handled, focus_target,
                self.node_ids[i], taffy,
            );
            if *handled {
                break;
            }
        }

        // Post-dispatch sweep: any child with a pending detach?
        let mut detaches: Vec<(usize, u64, Point)> = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if let Some((tab_id, release_point)) = g.take_tab_detach() {
                detaches.push((i, tab_id, release_point));
            }
        }

        for (src_idx, tab_id, release_point) in detaches {
            // Find a sibling whose bar rect contains the release point.
            let target_idx = self
                .groups
                .iter()
                .enumerate()
                .find(|(i, g)| *i != src_idx && g.bar_rect().contains(release_point))
                .map(|(i, _)| i);
            if let Some(tgt_idx) = target_idx {
                // Resolve the per-pill drop index against the target's tab
                // rects: left of the first pill's midpoint → 0, between
                // pills → that pill's index, past the last pill → append.
                let drop_idx = self.groups[tgt_idx].drop_index_for(release_point);
                let detached = self.groups[src_idx].detach_tab(tab_id);
                if let Some((desc, host)) = detached {
                    self.groups[tgt_idx].insert_detached_tab(drop_idx, desc, host);
                    *handled = true;
                }
                continue;
            }
            // No sibling bar matched — try the inter-group gap zones.
            // Matches the concept's `DropZone` between every pair of
            // groups in `DesktopView.tsx:414`, which calls
            // `onMoveTab(src, 'new', tab, idx)` to splice a new group.
            if let Some(gap_idx) = self.gap_index_for(release_point) {
                // Refuse the degenerate case where the source is a
                // single-tab group and the user is just dropping the tab
                // back next to itself — that would destroy and recreate
                // the same group in the same slot for no visible effect.
                let src_only_tab = self.groups[src_idx].tab_count() == 1;
                let right_of_src = gap_idx == src_idx + 1;
                let left_of_src = gap_idx == src_idx;
                if src_only_tab && (right_of_src || left_of_src) {
                    continue;
                }
                if self.split_tab_into_new_group(src_idx, tab_id, gap_idx) {
                    *handled = true;
                }
            }
        }
    }

    fn name(&self) -> &str {
        "GroupRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Bubble up any child group's animation signal — the default
        // `children()` impl returns an empty vec, so without this our
        // WindowGroup's TabBar drop-zone expand curve would never reach
        // the shell's overlay host and the redraw pump would stall
        // mid-animation.
        self.groups.iter().any(|g| g.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Clear + report our own flag (so a second poll in the same tick
        // returns false), then forward to every group so a nested mutation
        // — e.g. a future tab-close-prunes-group path inside WindowGroup —
        // still bubbles up. `any` plus `|` combined so we don't
        // short-circuit the child traversal (each child must also clear).
        let own = std::mem::take(&mut self.structure_dirty);
        let children = self
            .groups
            .iter_mut()
            .map(|g| g.take_structure_dirty())
            .fold(false, |acc, d| acc || d);
        own || children
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{TabDescriptor, TileContent, WindowWidth};

    fn make_group(id: u64, tab_ids: &[u64]) -> WindowGroup {
        let tabs: Vec<(TabDescriptor, TileContent)> = tab_ids
            .iter()
            .map(|t| {
                (
                    TabDescriptor::new(*t, format!("T{t}")),
                    TileContent::Stub {
                        kind: crate::widget::AppKind::Settings,
                        label: format!("S{t}"),
                    },
                )
            })
            .collect();
        let active = tab_ids[0];
        WindowGroup::new(id, tabs, active, WindowWidth::Normal)
    }

    #[test]
    fn transfer_tab_moves_descriptor_and_host_between_groups() {
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.transfer_tab(0, 1, 2, 0));

        // Tab 2 is gone from g0, present at index 0 of g1 (and active).
        let g0 = &row.groups[0];
        let g1 = &row.groups[1];
        let g0_ids: Vec<u64> = g0.tabs().iter().map(|t| t.id).collect();
        let g1_ids: Vec<u64> = g1.tabs().iter().map(|t| t.id).collect();
        assert_eq!(g0_ids, vec![1, 3]);
        assert_eq!(g1_ids, vec![2, 10, 11]);
    }

    #[test]
    fn transfer_tab_is_noop_for_same_group() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[3]);
        let mut row = GroupRow::new(vec![g0, g1]);
        assert!(!row.transfer_tab(0, 0, 1, 0));
    }

    #[test]
    fn transfer_tab_is_noop_for_unknown_tab() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[3]);
        let mut row = GroupRow::new(vec![g0, g1]);
        // id 999 isn't in either group.
        assert!(!row.transfer_tab(0, 1, 999, 0));
    }

    #[test]
    fn split_tab_into_new_group_appends_with_empty_source_pruned() {
        // Source is single-tab (id 1), destination gap is at the end
        // (gap_idx=2 past the only other group). Expect: source
        // disappears, new group takes its place-plus-one (effectively
        // remains in the same x slot since the source was at index 0).
        let g0 = make_group(100, &[1]);       // single-tab — will be pruned
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 1, 2));

        // Two groups remain: g1 (with its original tabs) and the new
        // one (with just tab 1). The new group's id must be unique
        // relative to existing groups' ids.
        assert_eq!(row.groups.len(), 2);
        let first_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        let second_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(first_ids, vec![10, 11]);
        assert_eq!(second_ids, vec![1]);
        assert_ne!(row.groups[1].id, 200, "new group id must be unique");
    }

    #[test]
    fn split_tab_into_new_group_inserts_between_existing_groups() {
        // Source is multi-tab; drop lands at gap_idx=1 (between g0 and g1).
        // Source keeps its remaining tabs; new group sits between them.
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 2, 1));

        assert_eq!(row.groups.len(), 3);
        let ids: Vec<Vec<u64>> = row.groups.iter()
            .map(|g| g.tabs().iter().map(|t| t.id).collect())
            .collect();
        assert_eq!(ids[0], vec![1, 3]);
        assert_eq!(ids[1], vec![2]); // the new group with just the dropped tab
        assert_eq!(ids[2], vec![10, 11]);
        // Dropped tab must be active in its new solo group.
        assert_eq!(row.groups[1].active_tab_id(), 2);
    }

    #[test]
    fn split_tab_into_new_group_flags_structure_dirty_once() {
        // Regression guard for the "new group never renders" bug: after a
        // successful split, the row must report `take_structure_dirty()`
        // true so `App` rebuilds the taffy tree and the new group gets a
        // NodeId. The flag is edge-triggered — a second poll in the same
        // tick (without another mutation) returns false so we don't churn
        // taffy every frame.
        let g0 = make_group(100, &[1, 2, 3]);
        let g1 = make_group(200, &[10, 11]);
        let mut row = GroupRow::new(vec![g0, g1]);

        // Fresh row has nothing pending.
        assert!(!row.take_structure_dirty(), "fresh row must not be dirty");

        // Split sets the flag.
        assert!(row.split_tab_into_new_group(0, 2, 1));
        assert!(
            row.take_structure_dirty(),
            "successful split must flag structure_dirty for the App to rebuild taffy"
        );
        // Flag is edge-triggered — no second pulse without another mutation.
        assert!(
            !row.take_structure_dirty(),
            "flag must clear after first poll so we don't rebuild every frame"
        );

        // A no-op split (unknown tab) must not flag the row.
        assert!(!row.split_tab_into_new_group(0, 9999, 0));
        assert!(
            !row.take_structure_dirty(),
            "failed split must not flag structure_dirty"
        );
    }

    #[test]
    fn split_tab_into_new_group_noop_for_unknown_tab() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[3]);
        let mut row = GroupRow::new(vec![g0, g1]);
        assert!(!row.split_tab_into_new_group(0, 999, 0));
        // Groups untouched.
        assert_eq!(row.groups.len(), 2);
        let g0_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(g0_ids, vec![1, 2]);
    }

    #[test]
    fn split_tab_into_new_group_prepends_when_gap_zero() {
        let g0 = make_group(100, &[1, 2]);
        let g1 = make_group(200, &[10]);
        let mut row = GroupRow::new(vec![g0, g1]);

        assert!(row.split_tab_into_new_group(0, 1, 0));

        assert_eq!(row.groups.len(), 3);
        let ids: Vec<Vec<u64>> = row.groups.iter()
            .map(|g| g.tabs().iter().map(|t| t.id).collect())
            .collect();
        assert_eq!(ids[0], vec![1]); // the new group
        assert_eq!(ids[1], vec![2]); // original source minus tab 1
        assert_eq!(ids[2], vec![10]);
    }

    #[test]
    fn gap_index_for_classifies_inter_group_drops() {
        // Build a real taffy layout so the groups' rects are populated,
        // then probe `gap_index_for` at positions derived from those
        // rects.
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let g0 = make_group(100, &[1]);
        let g1 = make_group(200, &[10]);
        let mut row = GroupRow::new(vec![g0, g1]);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);

        // Wrap in an outer node so the row gets laid out inside a known
        // viewport (taffy won't otherwise know the row's outer bounds).
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::length(2000.0), height: Dimension::length(600.0) },
                    ..Default::default()
                },
                &[root],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                viewport,
                TSize {
                    width: AvailableSpace::Definite(2000.0),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);

        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let row_rect = row.rect;
        let y = (row_rect.y0 + row_rect.y1) / 2.0;

        // Left of first group → gap 0.
        assert_eq!(row.gap_index_for(Point::new(r0.x0 - 4.0, y)), Some(0));
        // Right of last group → gap N (=2).
        assert_eq!(row.gap_index_for(Point::new(r1.x1 + 4.0, y)), Some(2));
        // Inside a group's rect → None.
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 + 4.0, y)),
            None,
            "drops inside a group tile should not be inter-group gaps"
        );
        // Above the row (y outside) → None.
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 - 4.0, row_rect.y0 - 10.0)),
            None
        );
        // Between groups, if the layout left any gap — only assert if
        // one exists, since flex with gap=8 should give us some space.
        if r1.x0 > r0.x1 {
            let mid = (r0.x1 + r1.x0) / 2.0;
            assert_eq!(row.gap_index_for(Point::new(mid, y)), Some(1));
        }
    }

    #[test]
    fn transfer_active_tab_promotes_sibling_in_source_and_activates_in_dest() {
        // Regression guard for the detach-then-insert activation flow:
        // when the moved tab is the source's *active* tab, the source
        // must fall through to a neighbour, and the destination must
        // promote the newly inserted tab to active (since dragging into
        // a new group implies focus follows the tab).
        let g0 = make_group(100, &[1, 2, 3]); // active = 1
        let g1 = make_group(200, &[10, 11]); // active = 10
        let mut row = GroupRow::new(vec![g0, g1]);
        assert_eq!(row.groups[0].active_tab_id(), 1);

        // Move the active tab (id 1) from g0 → g1 at index 0.
        assert!(row.transfer_tab(0, 1, 1, 0));

        // Source fell through to a neighbour (the first remaining tab).
        let src_ids: Vec<u64> = row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(src_ids, vec![2, 3]);
        assert_eq!(
            row.groups[0].active_tab_id(),
            2,
            "source must pick a surviving neighbour as the new active"
        );

        // Destination inserted the tab and promoted it to active.
        let dst_ids: Vec<u64> = row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(dst_ids, vec![1, 10, 11]);
        assert_eq!(
            row.groups[1].active_tab_id(),
            1,
            "dragged tab should be active in its new group"
        );
    }
}
