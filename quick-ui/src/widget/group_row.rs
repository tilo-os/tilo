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
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

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
    /// Index of the inter-group gap the cursor is currently hovering over
    /// while a child `TabBar` has a drag in flight. `None` when no drag
    /// is active or the cursor is inside a group's own rect. `draw` reads
    /// this to paint a single indigo capsule at the hovered gap so the
    /// user sees "drop here" feedback before releasing. Cleared on mouse
    /// release regardless of the drop resolution, and whenever no child
    /// reports `drag_in_flight()` so stale indicators don't persist past
    /// the gesture that set them.
    hover_gap_idx: Option<usize>,
}

impl GroupRow {
    pub fn new(groups: Vec<WindowGroup>) -> Self {
        Self {
            groups,
            rect: Rect::ZERO,
            node_ids: Vec::new(),
            last_cursor: Point::ZERO,
            structure_dirty: false,
            hover_gap_idx: None,
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
            // Empty row → any drop becomes the first group. We don't
            // y-band-restrict: a fresh shell with no groups has a degenerate
            // row rect and we still want drops anywhere in the strip to
            // land as "first group".
            return Some(0);
        }
        // Don't y-band-restrict: the release point comes from the source
        // `TabBar` which is always in our row, so a drop anywhere in
        // screen space that doesn't land on one of our groups is a
        // new-window gap. The strip area below / above the row's rendered
        // rect was silently refused by the old y-band check — that was
        // the root cause of the "drop zones show but drop has no effect"
        // bug. See `drop_into_new_window_works_when_release_is_…` tests.
        // Inside any group rect → not a gap drop (within-group / sibling
        // reorder is handled elsewhere).
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

        // Inter-group drop-zone indicator: while any child has a drag in
        // flight and the cursor is hovering an inter-group gap, paint a
        // single translucent indigo capsule centred on that gap. Mirrors
        // the concept's `DropZone` component in `DesktopView.tsx:414`,
        // which only highlights the gap the cursor is over (not every
        // gap in the row) — this is the "drop here" confirmation that
        // was missing from the previous feel-bad "zones always visible"
        // treatment.
        let any_drag = self.groups.iter().any(|g| g.drag_in_flight());
        if any_drag
            && !self.groups.is_empty()
            && let Some(idx) = self.hover_gap_idx
        {
            let fill = theme.tokens.colors.accent[5].with_alpha_factor(0.5);
            let bar_w = 4.0_f64;
            let radius = 2.0_f64;
            let mid_x = if idx == 0 {
                self.groups[0].get_rect().x0 - 4.0
            } else if idx >= self.groups.len() {
                self.groups.last().unwrap().get_rect().x1 + 4.0
            } else {
                (self.groups[idx - 1].get_rect().x1
                    + self.groups[idx].get_rect().x0)
                    / 2.0
            };
            let capsule = RoundedRect::from_rect(
                Rect::new(mid_x - bar_w / 2.0, self.rect.y0, mid_x + bar_w / 2.0, self.rect.y1),
                radius,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &capsule,
            );
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
            // While any child has a drag in flight, publish the inter-group
            // gap the cursor is currently over so `draw` can paint the
            // "drop here" indicator. We explicitly gate on an active drag
            // so the indicator never shows for idle cursor motion — the
            // concept only surfaces gap targets *during* a drag gesture.
            let any_drag = self.groups.iter().any(|g| g.drag_in_flight());
            self.hover_gap_idx = if any_drag {
                self.gap_index_for(self.last_cursor)
            } else {
                None
            };
        }

        // Drop is resolved on release — the indicator has served its
        // purpose, and leaving it set would let a stale gap highlight
        // persist into the next frame (and possibly the next gesture,
        // since a fresh press replays CursorMoved before any drag starts).
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.hover_gap_idx = None;
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

        // Group-close prune sweep: any child whose group-level close
        // fade has fully elapsed is dropped from `self.groups` and
        // the structure is flagged dirty so `App` rebuilds the taffy
        // tree. Mirrors the tab-close sweep inside `WindowGroup`:
        // the fade plays out in place, then the parent reaps the
        // shell only once the curve has settled. Collected
        // highest-index-first so the removals don't shift indices
        // out from under the loop. Children still in-flight
        // (`is_closing() && !close_complete()`) are left alone; the
        // group keeps painting its shrink.
        let mut prune: Vec<usize> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.close_complete())
            .map(|(i, _)| i)
            .collect();
        if !prune.is_empty() {
            prune.sort_unstable_by(|a, b| b.cmp(a));
            for i in prune {
                self.groups.remove(i);
            }
            self.structure_dirty = true;
        }

        // Belt-and-suspenders: if dispatch (or our own release branch
        // above) cleared every child's drag, drop any lingering hover
        // target so `draw` can't paint a ghost indicator next frame. The
        // CursorMoved branch only reclassifies on motion, so without this
        // sweep a child that dropped its `pending_drag` internally (e.g.
        // a canceled press) could leave `hover_gap_idx` stuck at its last
        // value until the cursor moves again.
        if self.hover_gap_idx.is_some()
            && !self.groups.iter().any(|g| g.drag_in_flight())
        {
            self.hover_gap_idx = None;
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
            //
            // We deliberately do NOT refuse the "single-tab source dropped
            // into an adjacent gap" case. Previously this was blanket-
            // refused as a suspected degenerate "drop-in-place", but for
            // single-tab rows that refuse rule blocks *every* drag-out
            // (gap_idx can only be 0 or 1 with a single group, and both
            // are adjacent). The detach + insert + prune flow produces
            // the correct state even when the visible result is identical
            // to the pre-drag state — so the refuse was costing us all
            // drag-out-to-new-window flows to fix a cosmetic no-op.
            if let Some(gap_idx) = self.gap_index_for(release_point)
                && self.split_tab_into_new_group(src_idx, tab_id, gap_idx)
            {
                *handled = true;
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
        // Above the row (y outside) → still resolves to a gap. The
        // y-band restriction was removed so drops into the strip area
        // above/below the row's rect no longer silently fail (see
        // `drop_into_new_window_works_when_release_is_above_row`).
        assert_eq!(
            row.gap_index_for(Point::new(r0.x0 - 4.0, row_rect.y0 - 10.0)),
            Some(0)
        );
        // Between groups, if the layout left any gap — only assert if
        // one exists, since flex with gap=8 should give us some space.
        if r1.x0 > r0.x1 {
            let mid = (r0.x1 + r1.x0) / 2.0;
            assert_eq!(row.gap_index_for(Point::new(mid, y)), Some(1));
        }
    }

    /// Build a row with N groups (one per sub-slice) and drive a taffy
    /// layout pass so each group's rect and the row's rect are populated.
    /// The viewport defaults to 2000×600 so groups always fit and the
    /// row fills the vertical axis (`height: percent(1.0)` on each
    /// `WindowGroup`). Returns the row alongside the taffy tree + root
    /// node id so callers can invoke `handle_event` with a live tree.
    fn make_row_with_groups_with_taffy(
        tab_specs: &[&[u64]],
    ) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let groups: Vec<WindowGroup> = tab_specs
            .iter()
            .enumerate()
            .map(|(i, ids)| make_group(100 + i as u64 * 100, ids))
            .collect();
        let mut row = GroupRow::new(groups);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let root = row.build_layout(&mut taffy, &theme, &mut node_map);
        let viewport = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(2000.0),
                        height: Dimension::length(600.0),
                    },
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
        (row, taffy, root)
    }

    /// Layout-only helper: discards the taffy tree + root after setting
    /// the row's rects. Most drop-zone tests only probe `gap_index_for`
    /// and don't need a live tree.
    fn make_row_with_groups(tab_specs: &[&[u64]]) -> GroupRow {
        make_row_with_groups_with_taffy(tab_specs).0
    }

    #[test]
    fn drop_into_new_window_works_when_release_is_in_empty_strip_area_below_group() {
        // Regression for the "drop zones show but drop has no effect" bug:
        // when the user drags a tab and releases into the strip area below
        // the row's rect (common because the shell's strip is taller than
        // any single group's rendered height, and mouse release jitter
        // pushes past the row edge), `gap_index_for` must still resolve to
        // an inter-group gap so `split_tab_into_new_group` can fire. Today
        // the y-band check in `gap_index_for` returns None for any point
        // outside `[rect.y0, rect.y1]`, silently dropping the action.
        let row = make_row_with_groups(&[&[1, 2, 3, 4]]);
        // 20 px below the row's lower edge, 4 px right of the only group's
        // right edge: classic "drop into empty strip" position.
        let release = Point::new(row.groups[0].get_rect().x1 + 4.0, row.rect.y1 + 20.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(1),
            "release below the row's vertical band should still resolve to the right-of-last gap so \
             the tab splits into a new window"
        );
    }

    #[test]
    fn drop_into_new_window_works_when_release_is_above_row() {
        // Companion to the "below" case above: if the user drags a tab up
        // and slightly overshoots the top of the row (into the strip's
        // header chrome), the release must still splice a new group at
        // the correct gap rather than falling through silently.
        let row = make_row_with_groups(&[&[1, 2], &[10, 11]]);
        let r0 = row.groups[0].get_rect();
        // 15 px above row.y0, x left of the first group → gap 0.
        let release = Point::new(r0.x0 - 4.0, row.rect.y0 - 15.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(0),
            "release above the row should resolve to the left-of-first gap"
        );
    }

    #[test]
    fn single_tab_group_can_split_into_new_group_on_any_adjacent_gap() {
        // Regression for the refuse rule in `handle_event` that blocked
        // single-tab source groups from splitting into a new window when
        // the dropped gap was adjacent (the only kind of gap available in
        // a one-group row). The direct API has no refuse — the sweep did.
        // See `single_tab_group_split_is_not_refused_by_handle_event_sweep`
        // for the end-to-end event-driven counterpart. Keeping this as a
        // unit guard so `split_tab_into_new_group` stays callable for any
        // gap_idx regardless of source tab count.
        let mut row = make_row_with_groups(&[&[1]]);
        // Adjacent gap to the right of the sole group.
        assert!(
            row.split_tab_into_new_group(0, 1, 1),
            "single-tab group must be splittable into an adjacent gap"
        );
        // Source was pruned (empty after detach), new group took its slot.
        assert_eq!(row.groups.len(), 1);
        assert_eq!(row.groups[0].tabs().len(), 1);
        assert_eq!(row.groups[0].tabs()[0].id, 1);
        assert!(row.take_structure_dirty(), "split must flag structure_dirty");
    }

    #[test]
    fn single_tab_group_split_is_not_refused_by_handle_event_sweep() {
        // The direct `split_tab_into_new_group` API never had the refuse
        // rule — it lived in the post-dispatch sweep inside `handle_event`.
        // This test drives the sweep end-to-end: we publish a pending
        // detach on the source group's TabBar and dispatch a no-op event,
        // then assert a new group was spliced (source pruned because it
        // was single-tab). Before the fix, the sweep's
        // `src_only_tab && (left_of_src || right_of_src)` guard silently
        // refused the split.
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[7]]);
        let g0_rect = row.groups[0].get_rect();
        // Release point: right of the group (gap 1), inside the row's
        // y-band so the y-check passes today; this isolates the refuse
        // rule as the sole failing gate.
        let release = Point::new(g0_rect.x1 + 10.0, (g0_rect.y0 + g0_rect.y1) / 2.0);
        // Manually publish a detach on the source's TabBar — mimics what
        // `TabBar::handle_event` does when a drag release lands outside
        // its own rect.
        row.groups[0].bar_mut().pending_detach = Some((7, release));

        // Drive a handle_event pass; a CursorMoved event is a no-op that
        // triggers the post-dispatch detach sweep.
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(release.x, release.y),
        };
        row.handle_event(
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        // Post-sweep: the split must have fired end-to-end. The
        // observable signal is `structure_dirty` — `split_tab_into_new_group`
        // sets it on success; the refuse path never does.
        assert!(
            row.take_structure_dirty(),
            "single-tab source must split into adjacent gap: handle_event sweep should \
             flag structure_dirty. Today the `src_only_tab && adjacent` refuse rule \
             silently skips this path"
        );
        // And the group bookkeeping: row ends up with exactly one group
        // (new group inserted, then source pruned) carrying tab id 7.
        assert_eq!(row.groups.len(), 1);
        assert_eq!(row.groups[0].tabs().len(), 1);
        assert_eq!(row.groups[0].tabs()[0].id, 7);
        // And the source's pending_detach must have been drained by the
        // sweep (consumed, not left pending for the next tick).
        assert!(row.groups[0].bar_mut().pending_detach.is_none());
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

    /// Stage a drag-in-flight on the given group's `TabBar` by setting
    /// its `pending_drag` directly. Mirrors what `TabBar::handle_event`
    /// does once the press-to-drag threshold is crossed, without needing
    /// us to replay a full press → move-past-threshold gesture. Shared
    /// by the three `hover_gap_idx` tests below.
    fn start_drag(row: &mut GroupRow, group_idx: usize, tab_id: u64) {
        use crate::dnd::DragPayload;
        row.groups[group_idx].bar_mut().pending_drag = Some(DragPayload::Tab {
            group: 0,
            tab: tab_id,
        });
    }

    /// Dispatch a `CursorMoved` at the given logical-space point through
    /// `GroupRow::handle_event`. Uses scale_factor=1 so the logical
    /// coords we pass are what `last_cursor` sees. Returns nothing — the
    /// test reads state off the row afterwards.
    fn fire_cursor_moved(row: &mut GroupRow, taffy: &TaffyTree<()>, root: NodeId, p: Point) {
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, taffy,
        );
    }

    #[test]
    fn hover_gap_tracks_cursor_during_drag() {
        // Regression guard for outstanding-work item #2 sub-3: while a
        // child TabBar has a drag in flight, `GroupRow` must classify
        // the cursor's current position against its inter-group gaps and
        // publish the result as `hover_gap_idx` so `draw` can paint a
        // "drop here" indicator. Without this, the user gets no visual
        // confirmation that releasing will split into a new window.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        // Target the gap between group 0 and group 1 (gap_idx == 1).
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = (row.rect.y0 + row.rect.y1) / 2.0;
        let mid_gap = Point::new((r0.x1 + r1.x0) / 2.0, mid_y);

        fire_cursor_moved(&mut row, &taffy, root, mid_gap);

        assert_eq!(
            row.hover_gap_idx,
            Some(1),
            "cursor in the gap between groups 0 and 1 must set hover_gap_idx = Some(1)"
        );
    }

    #[test]
    fn hover_gap_clears_on_release() {
        // After a drop is resolved (mouse released), the indicator must
        // vanish regardless of whether the release happened over a valid
        // gap or fell through. Leaving it set would persist a ghost
        // highlight into the next frame — the exact "zones don't go
        // away" feel-bad outstanding-work item #2 was about.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        start_drag(&mut row, 0, 1);
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = (row.rect.y0 + row.rect.y1) / 2.0;
        let mid_gap = Point::new((r0.x1 + r1.x0) / 2.0, mid_y);
        fire_cursor_moved(&mut row, &taffy, root, mid_gap);
        assert!(row.hover_gap_idx.is_some(), "precondition: drag tracked the gap");

        // Release — the drop is now resolved. `handle_event` clears the
        // indicator unconditionally on left-button release.
        let event = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        assert_eq!(
            row.hover_gap_idx, None,
            "MouseInput::Released must clear hover_gap_idx so no ghost indicator persists"
        );
    }

    /// When a child `WindowGroup` has finished its close fade
    /// (`close_complete()` returns true), the next `handle_event`
    /// tick must drop it from `self.groups` and flag
    /// `structure_dirty` so the App rebuilds the taffy tree. This
    /// is the pruning path the pre-feature code was missing — the
    /// only existing prune path was `split_tab_into_new_group`,
    /// so a tab-close-emptied group used to hang around forever.
    #[test]
    fn group_pruned_after_close_fade_completes() {
        use std::time::{Duration, Instant};
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Seed group 0 with an already-elapsed close timestamp so
        // the sweep treats it as ready-to-prune on the very next
        // tick.
        row.groups[0].set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(200),
        );
        assert_eq!(row.groups.len(), 2, "precondition: two groups before prune");

        // Drive a no-op CursorMoved to trigger the sweep.
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        assert_eq!(
            row.groups.len(),
            1,
            "fully-faded group must be dropped from self.groups",
        );
        assert!(
            row.take_structure_dirty(),
            "prune must flag structure_dirty so App rebuilds taffy",
        );
    }

    /// Mirror of the prune test: a group whose close fade is still
    /// in flight (elapsed < `GROUP_CLOSE_FADE_MS`) must be left
    /// alone. The prune only fires once `close_complete()` returns
    /// true. Guards against a too-aggressive sweep that reaps the
    /// group before the user sees the animation.
    #[test]
    fn group_retained_during_close_fade() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Freshly-latched close — fade window has barely started,
        // far from elapsed.
        row.groups[0].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[0].is_closing());
        assert!(!row.groups[0].close_complete());

        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(5000.0, 5000.0),
        };
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        row.handle_event(
            &event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        assert_eq!(
            row.groups.len(),
            2,
            "group mid-fade must NOT be pruned — the user is still watching the animation",
        );
    }

    #[test]
    fn hover_gap_is_none_when_no_drag() {
        // Motion without a drag must *never* populate `hover_gap_idx` —
        // the gap indicators are strictly a drag-time affordance. If we
        // classified idle cursor motion, the indicator would flash
        // during plain hover, which is not the concept's behaviour.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        // Intentionally NO `start_drag` call.
        let r0 = row.groups[0].get_rect();
        let r1 = row.groups[1].get_rect();
        let mid_y = (row.rect.y0 + row.rect.y1) / 2.0;
        let mid_gap = Point::new((r0.x1 + r1.x0) / 2.0, mid_y);

        fire_cursor_moved(&mut row, &taffy, root, mid_gap);

        assert_eq!(
            row.hover_gap_idx, None,
            "cursor motion without an active drag must not publish a hover_gap_idx"
        );
    }
}
