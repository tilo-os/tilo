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
use std::time::{Duration, Instant};
use taffy::{Dimension, Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Widget, WindowGroup, WindowWidth};

/// Idle threshold after the last wheel event before we kick off a
/// snap-to-nearest-group animation. Mirrors the concept's
/// `snap-x snap-proximity` heuristic in `DesktopView.tsx:396` —
/// while the user is actively wheeling we don't snap; once they stop,
/// the strip eases to the nearest group's left edge.
const SNAP_IDLE_MS: u64 = 200;
/// Duration of the snap animation itself (cubic-out ease). Matches the
/// 200 ms feel the spawn animation in `window_group.rs` uses for
/// secondary motions — long enough to read as eased, short enough to
/// stay out of the user's way if they want to wheel again.
const SNAP_ANIM_MS: u64 = 200;

/// In-flight snap state: from `from` to `to` over `SNAP_ANIM_MS`,
/// started at `started`. Cleared when the curve settles or the user
/// starts wheeling again (a fresh wheel event resets `last_wheel_at`,
/// and the next `tick_snap` call notices the snap target no longer
/// matches the current scroll target and rebases).
#[derive(Clone, Copy, Debug)]
struct SnapAnim {
    from: f64,
    to: f64,
    started: Instant,
}

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
    /// Horizontal scroll offset in logical pixels. The strip's natural
    /// content width (sum of group widths + gaps) can exceed the row's
    /// viewport-aligned `rect.width()`; `scroll_x` shifts every child's
    /// rendered x by `-scroll_x` so the user can pan groups that would
    /// otherwise sit off the right edge into view. Mirrors the concept's
    /// `flex-1 overflow-x-auto` strip in `DesktopView.tsx:396`. Updated
    /// by mouse-wheel events while the cursor is over the row's rect
    /// (vertical wheel pans horizontally — Niri-style global tabbing).
    /// Clamped to `[0, max(0, content_w - viewport_w)]` after every
    /// layout pass so a structural mutation that shrinks the content
    /// (e.g. closing a group) auto-snaps the view back into bounds.
    scroll_x: f64,
    /// Total content width including inter-group gaps, captured each
    /// `apply_layout` so `handle_event`'s wheel handler can clamp
    /// `scroll_x` without re-summing children.
    content_w: f64,
    /// Per-group local x-positions (relative to the row's content origin),
    /// captured during `apply_layout`. These are the snap targets — the
    /// strip eases `scroll_x` to whichever entry is nearest after a
    /// wheel-idle window. Re-built every layout pass so a structural
    /// mutation that inserts / removes / reorders groups produces the
    /// updated set of legal snap stops automatically.
    group_left_edges: Vec<f64>,
    /// Timestamp of the most recent `MouseWheel` event handled by this
    /// row. `tick_snap` waits for `SNAP_IDLE_MS` of quiet on this
    /// timestamp before kicking off the snap animation; a fresh wheel
    /// event during a snap cancels the in-flight curve (the timestamp
    /// jumps forward, the idle gate fails again, and the existing snap
    /// is dropped on the next tick).
    last_wheel_at: Option<Instant>,
    /// In-flight snap animation, if any. `None` when the strip is at
    /// rest or actively being wheeled. `Some` after the idle gate has
    /// fired and we've committed to a target group's left edge.
    snap_anim: Option<SnapAnim>,
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
            scroll_x: 0.0,
            content_w: 0.0,
            group_left_edges: Vec::new(),
            last_wheel_at: None,
            snap_anim: None,
        }
    }

    /// Current horizontal scroll offset in logical pixels. Exposed so
    /// tests can assert `scroll_x` updates after a wheel event without
    /// reaching through private fields.
    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }

    /// Total natural content width (sum of group widths + inter-group
    /// gaps), as captured by the most recent `apply_layout`. Returns
    /// `0.0` before the first layout pass.
    pub fn content_width(&self) -> f64 {
        self.content_w
    }

    /// Maximum legal `scroll_x`: zero if the strip's content fits inside
    /// the row's viewport rect, otherwise the overflow distance.
    pub fn max_scroll_x(&self) -> f64 {
        (self.content_w - self.rect.width()).max(0.0)
    }

    /// True while a snap-to-nearest-group animation is in flight, or
    /// while the user just-wheeled (within the idle threshold). The
    /// shell's `needs_animation_frame()` chain reads this so the App
    /// keeps pumping redraws — without that, a wheel-then-idle would
    /// freeze the strip mid-pan and never ease into the snap target.
    fn snap_in_flight(&self) -> bool {
        if self.snap_anim.is_some() {
            return true;
        }
        // Pending snap: user has wheeled but the idle gate hasn't
        // fired yet. Keep redraws pumping so `tick_snap` runs the
        // moment the gate clears, even if no other event arrives.
        if let Some(t) = self.last_wheel_at
            && t.elapsed() < Duration::from_millis(SNAP_IDLE_MS + SNAP_ANIM_MS)
            && !self.is_aligned_to_group()
        {
            return true;
        }
        false
    }

    /// True iff `scroll_x` is within 0.5 px of one of the group left
    /// edges — i.e. the strip is already at rest at a snap stop, so
    /// the idle gate's tick has nothing to do. Used to short-circuit
    /// `snap_in_flight` after the snap settles so we stop pumping
    /// redraws.
    fn is_aligned_to_group(&self) -> bool {
        if self.group_left_edges.is_empty() {
            return true;
        }
        self.group_left_edges
            .iter()
            .any(|&e| (e - self.scroll_x).abs() < 0.5)
    }

    /// Find the snap target nearest to the current `scroll_x`, clamped
    /// into `[0, max_scroll_x()]`. Returns `None` if there are no
    /// groups (nothing to snap to) or if the strip can't actually
    /// scroll (max_scroll_x == 0 — content fits).
    fn nearest_group_edge(&self) -> Option<f64> {
        if self.group_left_edges.is_empty() {
            return None;
        }
        let max = self.max_scroll_x();
        if max <= 0.0 {
            return None;
        }
        let mut best: Option<f64> = None;
        let mut best_dist = f64::MAX;
        for &edge in &self.group_left_edges {
            let candidate = edge.clamp(0.0, max);
            let d = (candidate - self.scroll_x).abs();
            if d < best_dist {
                best_dist = d;
                best = Some(candidate);
            }
        }
        best
    }

    /// Per-frame snap tick. Called from `apply_layout` so the snap
    /// animation runs whenever the App relayouts (which happens every
    /// frame while `needs_animation_frame()` keeps redraws pumping
    /// AND `take_structure_dirty()` keeps `layout_dirty` true). Split
    /// out from `apply_layout` so headless tests can advance simulated
    /// time deterministically without staging a full taffy tree.
    ///
    /// Behaviour:
    /// - If a snap is in flight, ease `scroll_x` along the cubic-out
    ///   curve from `from` to `to` over `SNAP_ANIM_MS`. Clear the snap
    ///   when the curve settles.
    /// - Otherwise, if `last_wheel_at` is set and at least `SNAP_IDLE_MS`
    ///   has elapsed since the last wheel event, kick off a fresh snap
    ///   targeting the nearest group's left edge.
    pub fn tick_snap_at(&mut self, now: Instant) {
        // No snap in flight — should we start one? Only if the user
        // actually wheeled (`last_wheel_at` is set) AND has been quiet
        // for at least the idle threshold AND the current scroll_x is
        // not already aligned to a group edge. Kicking off here BEFORE
        // the curve-advance branch lets the same `tick_snap_at` call
        // both start the snap AND apply its first eased frame — the
        // snap's `started` is anchored at `last_wheel_at + SNAP_IDLE_MS`
        // so the eased frame reflects the elapsed time since the idle
        // gate cleared, not since this particular tick fired.
        if self.snap_anim.is_none() {
            if let Some(last_wheel) = self.last_wheel_at {
                let idle = now.saturating_duration_since(last_wheel);
                if idle >= Duration::from_millis(SNAP_IDLE_MS) {
                    if self.is_aligned_to_group() {
                        self.last_wheel_at = None;
                    } else if let Some(target) = self.nearest_group_edge() {
                        if (target - self.scroll_x).abs() < 0.5 {
                            self.scroll_x = target;
                            self.last_wheel_at = None;
                        } else {
                            // Anchor `started` at the moment the idle
                            // gate was met (`last_wheel + SNAP_IDLE_MS`),
                            // not `now`. That way the very tick that
                            // detected idle already applies the curve's
                            // first eased frame for whatever duration
                            // has passed since the gate cleared.
                            let started = last_wheel
                                + Duration::from_millis(SNAP_IDLE_MS);
                            self.snap_anim = Some(SnapAnim {
                                from: self.scroll_x,
                                to: target,
                                started,
                            });
                        }
                    } else {
                        self.last_wheel_at = None;
                    }
                }
            }
        }
        // Active snap (either pre-existing OR freshly kicked off
        // above): advance the cubic-out curve.
        if let Some(anim) = self.snap_anim {
            let elapsed = now.saturating_duration_since(anim.started);
            let total = Duration::from_millis(SNAP_ANIM_MS);
            if elapsed >= total {
                self.scroll_x = anim.to;
                self.snap_anim = None;
                self.last_wheel_at = None;
                return;
            }
            let raw = (elapsed.as_secs_f64() / (SNAP_ANIM_MS as f64 / 1000.0))
                .clamp(0.0, 1.0);
            // Cubic-out ease — same shape the spawn animation in
            // `window_group.rs` uses for its slide-in. Punchy at the
            // start, gentle settle at the end.
            let eased = 1.0 - (1.0 - raw).powi(3);
            self.scroll_x = anim.from + (anim.to - anim.from) * eased;
        }
    }

    /// Test helper: directly seed `scroll_x` so tests can probe the
    /// snap behaviour without driving a full wheel-event sequence.
    #[cfg(test)]
    pub(crate) fn set_scroll_x_for_test(&mut self, x: f64) {
        self.scroll_x = x;
    }

    /// Test helper: directly seed `last_wheel_at` so tests can simulate
    /// the user having stopped wheeling some duration ago without
    /// having to actually sleep or run a real event loop.
    #[cfg(test)]
    pub(crate) fn set_last_wheel_at_for_test(&mut self, t: Instant) {
        self.last_wheel_at = Some(t);
    }

    /// Test helper: directly seed `group_left_edges` so tests that
    /// don't run a real taffy layout pass can still exercise the snap
    /// math.
    #[cfg(test)]
    pub(crate) fn set_group_left_edges_for_test(&mut self, edges: Vec<f64>) {
        self.group_left_edges = edges;
    }

    /// Test helper: directly seed `content_w` so `max_scroll_x` returns
    /// a non-zero value in tests that don't run a real taffy layout.
    #[cfg(test)]
    pub(crate) fn set_content_w_for_test(&mut self, w: f64) {
        self.content_w = w;
    }

    /// Test helper: directly seed the row's outer rect (for `max_scroll_x`).
    #[cfg(test)]
    pub(crate) fn set_rect_for_test(&mut self, r: Rect) {
        self.rect = r;
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
        // Pin `flex_shrink: 0.0` on every child so the row is permitted
        // to overflow horizontally when the sum of group widths exceeds
        // the row's viewport-aligned width — without this override,
        // Taffy's default `flex_shrink: 1.0` squishes every group below
        // its declared `WindowWidth` and the user can't see overflowed
        // groups at all. Non-maximized groups already declare a fixed
        // width via `Dimension::length`, so refusing to shrink keeps
        // them at that exact size; the maximized group keeps its
        // `flex_grow: 1e6` and absorbs leftover space *within* the
        // visible viewport, with siblings sitting at their natural
        // width past the right edge until the user scrolls.
        for &node in &children {
            let mut style = taffy.style(node).unwrap().clone();
            style.flex_shrink = 0.0;
            taffy.set_style(node, style).unwrap();
        }
        // The row must fill the remaining vertical space inside the
        // parent Column so each child `WindowGroup`'s `height: percent(1.0)`
        // resolves to the full available height — otherwise the row only
        // measures as tall as the tallest child's natural height (i.e. the
        // 40 px TabBar) and the `TileHost` collapses to zero height,
        // hiding the concept's white card entirely.
        //
        // `width: percent(1.0)` keeps the row's outer rect viewport-
        // aligned (so it doesn't push parent Column wider when content
        // overflows). Children laid out beyond `rect.x1` are still
        // walked by `apply_layout` and shifted into screen-space by the
        // `scroll_x` translation in our origin computation; they remain
        // hit-testable because `gap_index_for` uses each group's stored
        // post-shift `Rect`.
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
        // Total natural content width = (rightmost child's x + width) -
        // (leftmost child's x). With Taffy laying children out in order
        // from local x=0, this is simply `last.x + last.width` (no gap
        // after the last child). We read each child's *taffy-reported*
        // position so the inter-group gap (8 px declared on the row's
        // style) is included automatically.
        //
        // Same loop captures every child's local x position into
        // `group_left_edges` — these are the snap targets the
        // wheel-idle snap animation eases `scroll_x` toward. Re-built
        // every layout pass so insertions / removals / reorderings
        // produce the updated set of legal snap stops automatically.
        let mut content_w = 0.0_f64;
        let mut edges: Vec<f64> = Vec::with_capacity(self.node_ids.len());
        for &cn in &self.node_ids {
            let cl = taffy.layout(cn).unwrap();
            edges.push(cl.location.x as f64);
            let right = cl.location.x as f64 + cl.size.width as f64;
            if right > content_w {
                content_w = right;
            }
        }
        self.content_w = content_w;
        self.group_left_edges = edges;
        // Clamp scroll into bounds. A structural mutation that shrinks
        // the strip (group close, tab merge that prunes) can leave
        // `scroll_x` past the new max; clamping here keeps the next
        // frame's draw inside the visible content without requiring the
        // mutation site to know about scroll state.
        let max_scroll = self.max_scroll_x();
        if self.scroll_x > max_scroll {
            self.scroll_x = max_scroll;
        }
        if self.scroll_x < 0.0 {
            self.scroll_x = 0.0;
        }
        // Per-frame snap tick: advance any in-flight snap animation,
        // or kick a fresh one off if the user has been idle past the
        // threshold. Done AFTER the clamp so a structural mutation
        // that snapped scroll_x to max doesn't get re-eased.
        self.tick_snap_at(Instant::now());
        // Children's origin is shifted left by `scroll_x` so a positive
        // scroll reveals groups originally laid out off the right edge.
        // Each child's stored `Rect` is in screen space (post-shift),
        // so hit-testing against `last_cursor` (also screen space)
        // remains consistent for `gap_index_for` and the per-tab
        // sibling-bar matching in the post-dispatch detach sweep.
        let child_origin = Point::new(self.rect.x0 - self.scroll_x, self.rect.y0);
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

        // Horizontal scroll for the strip. Niri-style: the user spins
        // the vertical wheel while the cursor is over the row's rect
        // and the strip pans horizontally so off-screen-right groups
        // come into view. We claim the event (`*handled = true`) only
        // when there's actually content to scroll — otherwise the
        // sibling `StripView` retains the wheel for inter-strip
        // pagination, preserving its existing behaviour.
        //
        // Wheel handling lives BEFORE the per-child dispatch below so a
        // child's `TileHost` can't swallow the wheel for its own
        // (non-existent) internal scroll. The dispatch loop still runs
        // after, but with `handled = true` the loop early-exits on the
        // first child it visits.
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.rect.contains(self.last_cursor)
            && self.max_scroll_x() > 0.0
        {
            // 30 px per LineDelta unit lines up with the `scrollable.rs`
            // module's per-line factor (it uses 20 px there; we go a
            // touch wider so a single wheel detent reveals the next
            // group's leading edge with a 600 px window). PixelDelta is
            // honoured 1:1 — trackpad gestures already supply pixels.
            //
            // Sign convention matches GTK / browser `overflow-x-auto`:
            // winit gives `delta.y > 0` for wheel-up; rotating the
            // wheel UP scrolls content LEFT (reveals stuff to the
            // left, equivalent to decreasing `scroll_x`). So the y
            // contribution is `-y`. Trackpad horizontal: `delta.x > 0`
            // means "two-finger swipe right", which reveals content to
            // the right (increases `scroll_x`), so the x contribution
            // is `+x`. PixelDelta carries the same axis convention
            // 1:1.
            let dx = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    let raw = (*x - *y) as f64;
                    raw * 30.0
                }
                MouseScrollDelta::PixelDelta(pp) => pp.x - pp.y,
            };
            if dx != 0.0 {
                let max = self.max_scroll_x();
                self.scroll_x = (self.scroll_x + dx).clamp(0.0, max);
                // Reset the idle gate and cancel any in-flight snap —
                // a fresh wheel event means the user is actively
                // panning, so we restart the 200 ms idle countdown
                // from now and drop whatever snap target the previous
                // gesture committed to. The next `tick_snap_at` after
                // the user stops will compute a new target against
                // the current `scroll_x`.
                self.last_wheel_at = Some(Instant::now());
                self.snap_anim = None;
                *handled = true;
            }
        }

        // Dispatch to every child; stop once one claims the event. We
        // deliberately *do not* bail out of the detach sweep even when a
        // child claimed — the claim is for pointer-capture / focus, while
        // the detach sweep is a separate "did anyone publish a detach?"
        // pass that runs regardless.
        //
        // `node_ids` is rebuilt every frame in `build_layout`, but
        // `groups` can grow mid-frame when a tab is dropped into a fresh
        // gap (the detach sweep below pushes a new `WindowGroup` and
        // marks `structure_dirty`). The mid-frame transient is benign —
        // the fresh group has no node_id this frame and gets one on the
        // next layout pass — so we tolerate the imbalance with `.get`
        // instead of indexing, and silently skip dispatch for the
        // newly-spawned group until layout catches up.
        for (i, g) in self.groups.iter_mut().enumerate() {
            let Some(&node_id) = self.node_ids.get(i) else {
                break;
            };
            g.handle_event(
                event, scale_factor, modifiers, state, root, handled, focus_target,
                node_id, taffy,
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
            // Parallel-vec hardening (Wave 2 / 2-D): `node_ids` is
            // populated by `build_layout` to match `groups` 1:1 and is
            // re-cleared / re-populated on the next layout pass after
            // we set `structure_dirty = true` below. The intermediate
            // post-prune state therefore has a *deliberately stale*
            // `node_ids.len() == pre-prune groups.len()` until the App
            // re-runs `build_layout`. The debug_assert below pins that
            // expectation: if some future code path popped a `node_ids`
            // slot outside the rebuild gate, the assert trips loudly.
            // We snapshot the prune count *before* the loop because the
            // `prune` vec is consumed by the removal walk.
            let prune_count = prune.len();
            prune.sort_unstable_by(|a, b| b.cmp(a));
            for i in prune {
                self.groups.remove(i);
            }
            debug_assert_eq!(
                self.node_ids.len(),
                self.groups.len() + prune_count,
                "GroupRow parallel-vec invariant violated: node_ids.len() ({}) \
                 must equal pre-prune groups.len() (post-prune {} + pruned {}). \
                 node_ids stays at the pre-prune length until structure_dirty \
                 triggers a build_layout rebuild.",
                self.node_ids.len(),
                self.groups.len(),
                prune_count,
            );
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
            // Closing groups are filtered out: a `WindowGroup` whose
            // group-level close fade has latched still paints its bar
            // rect for the 180 ms shrink, so a cross-group drop can
            // visually resolve onto a group that's in the process of
            // disappearing. `insert_detached_tab` doesn't clear
            // `closing_at`, so the re-inserted tab would get pruned
            // alongside the group at fade-end — the tab would be
            // silently lost. Treating closing groups as if they
            // weren't there at all lets the drop fall through to the
            // gap-based new-group path below, where we splice the
            // dropped tab into a brand-new group in the gap to the
            // LEFT of the closing group. That's the cleanest UX: the
            // dragged tab lands next to where the user aimed (the
            // closing group is about to vacate the slot anyway) and
            // the user gets immediate visual confirmation that their
            // drop produced a new window.
            let target_idx = self
                .groups
                .iter()
                .enumerate()
                .find(|(i, g)| {
                    *i != src_idx
                        && !g.is_closing()
                        && g.bar_rect().contains(release_point)
                })
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
            // The release may have landed on a *closing* sibling's bar
            // rect — the filter above treated it as not-there, but the
            // rect still exists geometrically, so `gap_index_for` will
            // return `None` (release is "inside a group"). Fall back
            // to the left-of-closing-group gap so the dropped tab
            // still produces a new window in approximately the right
            // spot instead of being silently dropped.
            let closing_target = self
                .groups
                .iter()
                .enumerate()
                .find(|(i, g)| {
                    *i != src_idx
                        && g.is_closing()
                        && g.bar_rect().contains(release_point)
                })
                .map(|(i, _)| i);
            if let Some(tgt_idx) = closing_target {
                if self.split_tab_into_new_group(src_idx, tab_id, tgt_idx) {
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
                continue;
            }
            // B-4: no valid drop target resolved — neither a non-closing
            // sibling bar, nor a closing sibling's bar, nor an inter-
            // group gap. The release point most commonly lands here
            // when the user drops over a *closing* sibling's content
            // area (below the bar): the closing-filter excludes it
            // from the first branch, the bar-rect check excludes it
            // from the second, and `gap_index_for` reports `None`
            // because the release is geometrically inside a group.
            // Treat as "cancel": the tab must remain in the source
            // group exactly where it started.
            //
            // The matched branches above each call `detach_tab`
            // lazily inside their own resolution path, so today an
            // unmatched release leaves the source intact already
            // (`tabs().any(...)` is true and the recovery below is
            // skipped). The recovery is a defensive guard: if a
            // future refactor ever moves the detach earlier in
            // this loop, an unmatched release would otherwise
            // silently drop the dragged tab on the floor. Sweeping
            // every other group for the orphaned id and splicing
            // it back onto source's head pins the post-condition
            // — "no valid target → tab stays in source" — at the
            // structural level rather than relying on the lazy-
            // detach pattern remaining in place.
            let tab_in_source =
                self.groups[src_idx].tabs().iter().any(|t| t.id == tab_id);
            if !tab_in_source {
                // We just confirmed source doesn't have the tab, so
                // its own `detach_tab` returns `None` and the search
                // sweeps every other group looking for the orphan.
                let recovered = self
                    .groups
                    .iter_mut()
                    .find_map(|g| g.detach_tab(tab_id));
                if let Some((desc, host)) = recovered {
                    self.groups[src_idx].insert_detached_tab(0, desc, host);
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
        //
        // Also include our own snap-on-idle animation: while a snap is
        // in flight (or a wheel is pending the idle gate), the App
        // must keep pumping redraws so each frame re-runs
        // `apply_layout` and the eased `scroll_x` propagates to every
        // child's stored rect.
        self.snap_in_flight() || self.groups.iter().any(|g| g.needs_animation_frame())
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
        // While a snap is in flight, force the App to rebuild taffy +
        // re-run `apply_layout` next frame. Without this, `apply_layout`
        // (which is where `tick_snap_at` lives) would only run on
        // events, leaving the snap curve to evaluate against stale
        // child rects between frames. Setting `structure_dirty` is
        // heavier than strictly needed (full taffy rebuild instead of
        // just a re-layout) but matches the existing pattern other
        // animations in this file use to drive frame-by-frame updates.
        let snap = self.snap_in_flight();
        own || children || snap
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

    /// A cross-group tab drop whose release point lands on a
    /// `WindowGroup` that is currently mid-close-fade must NOT be
    /// inserted into that group. `insert_detached_tab` doesn't
    /// clear `closing_at`, so the re-inserted tab would be pruned
    /// alongside the group at fade-end — the tab is silently lost.
    ///
    /// The fix filters closing groups out of the sibling bar-rect
    /// resolution, then falls through to a gap-based split: a
    /// brand-new group gets spliced in at the closing group's
    /// current slot (the gap to its LEFT), so the dragged tab
    /// ends up in a fresh adjacent window. This test seeds B as
    /// closing, drives a detach from A targeting B's bar rect,
    /// and asserts that B's tab list is unchanged and that a new
    /// group carrying A's dragged tab appears at B's position.
    #[test]
    fn drop_onto_closing_group_creates_new_group_instead() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // Two multi-tab groups so neither gets pruned by the
        // detach-empties-source rule — we want to assert specifically
        // on the drop-target side.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);

        // B is at index 1. Mark it as closing (fresh latch so the
        // prune sweep won't touch it this tick — only
        // `close_complete()` triggers prune).
        row.groups[1].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[1].is_closing());
        assert!(!row.groups[1].close_complete());
        let b_tabs_before: Vec<u64> =
            row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(b_tabs_before, vec![10, 11]);

        // Release point: inside B's bar rect. That's the exact
        // geometry the pre-fix code would resolve to B as a
        // cross-group drop target.
        let b_bar = row.groups[1].bar_rect();
        let release = Point::new(
            (b_bar.x0 + b_bar.x1) / 2.0,
            (b_bar.y0 + b_bar.y1) / 2.0,
        );
        // Publish a pending detach on A's bar — mimics what
        // `TabBar::handle_event` does when a drag release lands
        // outside A's own bar rect.
        row.groups[0].bar_mut().pending_detach = Some((1, release));

        // Drive a no-op CursorMoved so the detach sweep runs.
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

        // B's tab list must be untouched — the drop was diverted
        // away from the closing group.
        let b_idx = row
            .groups
            .iter()
            .position(|g| g.is_closing())
            .expect("B should still be present (mid-fade)");
        let b_tabs_after: Vec<u64> =
            row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_tabs_after, b_tabs_before,
            "closing group's tabs must be unchanged — the drop must not insert into it",
        );

        // The dragged tab (id 1) must have landed in a brand-new
        // group adjacent to (specifically: at the slot of) B.
        // Row now contains 3 groups: A (minus tab 1), new group
        // (tab 1), B (closing). The new group sits at what used
        // to be B's index.
        assert_eq!(
            row.groups.len(),
            3,
            "drop-onto-closing falls through to split-into-new-group",
        );
        let a_tabs: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(a_tabs, vec![2], "A lost its dragged tab (id 1)");
        // The new group carries only tab 1 and is NOT the closing
        // group. Find it positionally: closing group is at index 2.
        assert!(
            row.groups[2].is_closing(),
            "B (closing) should still be present at the right of the row",
        );
        let new_group_tabs: Vec<u64> =
            row.groups[1].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            new_group_tabs, vec![1],
            "dragged tab must end up in a fresh group, not in the closing group",
        );
        assert!(
            !row.groups[1].is_closing(),
            "the new group must NOT inherit the closing state",
        );
    }

    /// Regression for B-4: a cross-group drop whose release point lands
    /// inside a *closing* sibling's content area (below its bar, but
    /// still inside the group's outer rect) used to fall through every
    /// resolution branch silently — the non-closing-bar branch filters
    /// closing groups out, the closing-bar branch needs the release in
    /// the bar rect, and `gap_index_for` returns `None` because the
    /// release is inside a group's `get_rect`. Without an explicit
    /// "no valid target → cancel" fallback the dragged tab is at risk
    /// of being lost. This test pins the source-keeps-the-tab invariant
    /// so the post-detach loop can never quietly drop it on the floor.
    #[test]
    fn cross_group_drop_into_closing_content_area_keeps_tab_in_source() {
        use std::time::Instant;
        use winit::event::WindowEvent;

        // Two multi-tab groups. B (index 1) goes into the closing fade
        // freshly so the prune sweep won't reap it this tick — only its
        // `is_closing()` flag is set, which is the trigger that funnels
        // the drop past both `target_idx` branches.
        let (mut row, taffy, root) =
            make_row_with_groups_with_taffy(&[&[1, 2], &[10, 11]]);
        row.groups[1].set_closing_at_for_tests(Instant::now());
        assert!(row.groups[1].is_closing());
        assert!(!row.groups[1].close_complete());

        // Release point: inside B's outer rect but BELOW B's bar rect —
        // i.e. over its tile/content area. This is the gap the bug
        // report calls out: the closing-filter excludes B from the
        // first branch, the bar-rect check excludes B from the second
        // branch, and `gap_index_for` reports `None` because the
        // release is geometrically inside a group.
        let b_rect = row.groups[1].get_rect();
        let b_bar = row.groups[1].bar_rect();
        let release = Point::new(
            (b_rect.x0 + b_rect.x1) / 2.0,
            (b_bar.y1 + b_rect.y1) / 2.0,
        );
        assert!(
            b_rect.contains(release),
            "precondition: release is inside B's outer rect",
        );
        assert!(
            !b_bar.contains(release),
            "precondition: release is OUTSIDE B's bar rect (content area)",
        );
        assert_eq!(
            row.gap_index_for(release),
            None,
            "precondition: gap_index_for returns None for inside-group releases",
        );

        // Publish a pending detach on A (source) — mimics what
        // `TabBar::handle_event` does when a drag release lands
        // outside A's own bar rect. We pre-stage the detach marker
        // (instead of replaying the full press → drag-past-threshold
        // gesture) because `press_origin` is private; the existing
        // sibling test `single_tab_group_split_is_not_refused_by_handle_event_sweep`
        // uses the same shortcut.
        let a_tabs_before: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        row.groups[0].bar_mut().pending_detach = Some((1, release));

        // Drive the actual release: `MouseInput::Released` flows
        // through `GroupRow::handle_event`'s post-dispatch detach
        // loop, which is the bug site. Using the real release event
        // (not a no-op `CursorMoved`) ensures we test the same path
        // the user actually triggers.
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let release_event = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        row.handle_event(
            &release_event, 1.0, &modifiers, &mut doc, &doc_root,
            &mut handled, &mut focus, root, &taffy,
        );

        // Source group must still contain its original tab list — the
        // dragged tab id 1 is still present. No data loss, no spurious
        // new group spliced in, no tab inserted into the closing group.
        let a_tabs_after: Vec<u64> =
            row.groups[0].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            a_tabs_after, a_tabs_before,
            "source group must keep the dragged tab when no valid target resolves",
        );
        // Row layout untouched: still exactly two groups, B still closing.
        assert_eq!(
            row.groups.len(),
            2,
            "no new group should be spliced in for a cancelled drop",
        );
        let b_idx = row
            .groups
            .iter()
            .position(|g| g.is_closing())
            .expect("B should still be present mid-fade");
        let b_tabs_after: Vec<u64> =
            row.groups[b_idx].tabs().iter().map(|t| t.id).collect();
        assert_eq!(
            b_tabs_after, vec![10, 11],
            "closing group's tabs must be untouched — drop did not insert into it",
        );
        // The pending_detach marker must have been drained by the
        // sweep — a leftover would re-fire next tick.
        assert!(
            row.groups[0].bar_mut().pending_detach.is_none(),
            "post-dispatch sweep must drain pending_detach even when no target resolves",
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

    /// Parallel-vec invariant: the prune sweep in `GroupRow::handle_event`
    /// removes every fully-faded group from `self.groups`. The companion
    /// `node_ids` vector is rebuilt by the next `build_layout` pass —
    /// `apply_layout` would walk a stale `node_ids` and crash if it ever
    /// outlived the prune without a rebuild gate. Two safeguards keep
    /// this honest:
    ///
    /// 1. The prune branch sets `structure_dirty = true`, so the App's
    ///    tick handler re-runs `build_layout` before the next
    ///    `apply_layout`, which clears and repopulates `node_ids`.
    /// 2. The production code path debug-asserts that `node_ids.len()`
    ///    after the prune still equals the *pre-prune* `groups.len()`
    ///    — i.e. nobody silently popped a slot off `node_ids` outside
    ///    the rebuild path. Fail-loud in debug builds; the test below
    ///    drives that assert by pruning two groups in a single tick.
    ///
    /// Gated on `cfg(debug_assertions)` because the asserts being
    /// exercised are themselves debug-only.
    #[cfg(debug_assertions)]
    #[test]
    fn group_prune_keeps_node_ids_aligned_in_debug() {
        use std::time::{Duration, Instant};
        use winit::event::WindowEvent;

        // Three groups; mark the outer two as already-elapsed so the
        // sweep prunes them on the next tick. Using non-adjacent prune
        // indices (0 and 2) verifies the highest-index-first sort
        // doesn't shift slots out from under the loop and that the
        // pre-loop count is captured before any mutation.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[2], &[3]]);
        row.groups[0].set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(500),
        );
        row.groups[2].set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(500),
        );

        // Snapshot the pre-prune lengths. `node_ids` was populated by
        // the build_layout call inside `make_row_with_groups_with_taffy`,
        // so it must equal `groups.len()` here — that's the very
        // invariant the production debug_assert protects.
        let groups_pre = row.groups.len();
        let node_ids_pre = row.node_ids.len();
        assert_eq!(
            groups_pre, 3,
            "precondition: row was seeded with three groups",
        );
        assert_eq!(
            node_ids_pre, groups_pre,
            "precondition: node_ids must equal groups.len() after build_layout",
        );

        // Drive a no-op CursorMoved to trigger the prune sweep. If the
        // production debug_assert ever fires (e.g. because someone
        // popped a node_ids slot outside the build_layout path), this
        // call would panic and the test would fail loudly.
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

        // Two of three groups should be pruned. The remaining group is
        // the centre one (id-1 tab → groups[1] pre-prune).
        assert_eq!(
            row.groups.len(),
            1,
            "two fully-faded groups must be pruned in a single sweep",
        );

        // Either node_ids is in lockstep with groups (rebuilt-in-place
        // path) OR structure_dirty was set so the App rebuilds the
        // taffy tree before the next apply_layout pass. The plan
        // explicitly accepts either; we just refuse the third option
        // (drift with no rebuild gate) which would crash apply_layout.
        let aligned_now = row.node_ids.len() == row.groups.len();
        let dirty_for_rebuild = row.take_structure_dirty();
        assert!(
            aligned_now || dirty_for_rebuild,
            "after prune: node_ids must either match groups.len() ({}) directly \
             or structure_dirty must be set so build_layout rebuilds them \
             (node_ids.len() = {})",
            row.groups.len(),
            row.node_ids.len(),
        );
    }

    /// Helper that lays out N groups inside a *narrow* viewport so the
    /// strip's natural content width exceeds the row's rect — the
    /// precondition for horizontal scroll. Each `WindowWidth::Normal`
    /// group is 600 px so `count` × 600 + `count-1` × 8 (gap) is the
    /// natural content width; we set the viewport width so it's
    /// strictly less.
    fn make_overflow_row(count: usize, viewport_w: f64) -> (GroupRow, TaffyTree<()>, NodeId) {
        use std::collections::HashMap;
        use taffy::{AvailableSpace, Size as TSize, TaffyTree};

        let groups: Vec<WindowGroup> = (0..count)
            .map(|i| make_group(100 + i as u64 * 100, &[i as u64 + 1]))
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
                        width: Dimension::length(viewport_w as f32),
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
                    width: AvailableSpace::Definite(viewport_w as f32),
                    height: AvailableSpace::Definite(600.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .expect("taffy compute_layout");
        row.apply_layout(&taffy, root, Point::ORIGIN);
        (row, taffy, root)
    }

    #[test]
    fn content_width_exceeds_viewport_when_groups_overflow() {
        // Pre-fix invariant: with `flex_shrink: 1.0` (Taffy default),
        // children would shrink to fit and `content_w == rect.width()`.
        // After the override, content_w must equal the natural sum of
        // group widths (+ gaps) regardless of viewport size, so
        // `max_scroll_x()` is non-zero when content overflows. The
        // scroll machinery hangs off this invariant — without it,
        // wheel events are silently absorbed but pan nothing.
        let (row, _t, _r) = make_overflow_row(4, 1200.0);
        // Four 600-px Normal groups + three 8-px gaps = 2424 px.
        let expected_min = 4.0 * 600.0 + 3.0 * 8.0 - 1.0; // 1 px wiggle for fp.
        assert!(
            row.content_width() >= expected_min,
            "content_w must reflect the natural group widths; got {} expected ≥{}",
            row.content_width(),
            expected_min
        );
        assert!(
            row.max_scroll_x() > 0.0,
            "viewport 1200 < content {} → max_scroll must be positive",
            row.content_width()
        );
    }

    #[test]
    fn drop_in_visible_trailing_area_creates_new_group_at_end() {
        // After a horizontal scroll has shifted the strip, a release
        // past the last visible group's right edge must still resolve
        // to the trailing gap (gap_idx = N) so the dropped tab spawns
        // a new group at the end. This is the Niri-style "infinite
        // strip" trailing drop zone — and the user's reported flow
        // ("drag a tab past the rightmost group → new group there").
        //
        // We don't need to actually scroll: with a narrow viewport,
        // the last group already sits past `row.rect.x1` because
        // `flex_shrink: 0.0` lets children overflow.
        let (mut row, _t, _r) = make_overflow_row(3, 1000.0);
        let last_rect = row.groups.last().unwrap().get_rect();
        let release = Point::new(last_rect.x1 + 50.0, last_rect.y0 + 20.0);
        assert_eq!(
            row.gap_index_for(release),
            Some(3),
            "release past the last group's right edge must resolve to gap_idx=N"
        );
        // And the split actually fires for that gap.
        let src_tab = row.groups[0].tabs()[0].id;
        assert!(
            row.split_tab_into_new_group(0, src_tab, 3),
            "split_tab_into_new_group must succeed at the trailing gap"
        );
        // Source had only one tab, so it's pruned and the new group
        // sits at the original tail position. Net group count is 3.
        assert_eq!(row.groups.len(), 3);
        // The dropped tab must be the last group's only tab.
        let last_ids: Vec<u64> = row.groups.last().unwrap().tabs().iter().map(|t| t.id).collect();
        assert_eq!(last_ids, vec![src_tab]);
    }

    #[test]
    fn wheel_pans_strip_horizontally_when_content_overflows() {
        // End-to-end: a vertical wheel event over the row's rect, while
        // there's overflow, must (a) update `scroll_x` (b) shift each
        // group's stored `Rect` left by that amount on the next layout
        // pass, and (c) claim the event so a sibling `StripView`
        // doesn't paginate strips out from under the user.
        use winit::dpi::PhysicalPosition;

        let (mut row, taffy, root) = make_overflow_row(4, 1200.0);
        let pre_rect_0 = row.groups[0].get_rect();

        // Move the cursor to the centre of the row so the wheel
        // handler's containment check passes.
        let cy = (row.rect.y0 + row.rect.y1) / 2.0;
        let cx = (row.rect.x0 + row.rect.x1) / 2.0;
        let mut state = AutoCommit::new();
        let root_obj = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        // Synthesise CursorMoved + MouseWheel through handle_event.
        // DeviceId is opaque in winit; we use the dummy variant.
        let dev = unsafe { std::mem::zeroed() };
        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        // Wheel down (y < 0) → scroll content right (scroll_x > 0).
        row.handle_event(
            &WindowEvent::MouseWheel {
                device_id: dev,
                delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        assert!(handled, "wheel over an overflowing row must claim the event");
        assert!(
            row.scroll_x() > 0.0,
            "wheel-down must increase scroll_x; got {}",
            row.scroll_x()
        );
        // Re-run the layout pass: with a non-zero scroll, every child's
        // x must shift left by `scroll_x`.
        row.apply_layout(&taffy, root, Point::ORIGIN);
        let post_rect_0 = row.groups[0].get_rect();
        let dx = pre_rect_0.x0 - post_rect_0.x0;
        assert!(
            (dx - row.scroll_x()).abs() < 0.5,
            "group[0]'s x must shift left by exactly scroll_x; pre={} post={} dx={} scroll_x={}",
            pre_rect_0.x0,
            post_rect_0.x0,
            dx,
            row.scroll_x()
        );
    }

    #[test]
    fn wheel_does_not_claim_when_strip_fits_inside_viewport() {
        // Inverse of the overflow case: when content fits, the wheel
        // event is left for `StripView` to paginate strips. This
        // preserves the existing inter-strip nav behaviour for
        // single-strip-fits-viewport workspaces (the bootstrap default).
        use winit::dpi::PhysicalPosition;

        // Two Normal=600 groups in a wide 2000-px viewport: 1208 px
        // content < 2000 px viewport, no overflow, max_scroll = 0.
        let (mut row, taffy, root) = make_row_with_groups_with_taffy(&[&[1], &[10]]);
        assert_eq!(
            row.max_scroll_x(),
            0.0,
            "precondition: 2-group row in wide viewport must not overflow"
        );

        let cy = (row.rect.y0 + row.rect.y1) / 2.0;
        let cx = (row.rect.x0 + row.rect.x1) / 2.0;
        let mut state = AutoCommit::new();
        let root_obj = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let dev = unsafe { std::mem::zeroed() };
        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        row.handle_event(
            &WindowEvent::MouseWheel {
                device_id: dev,
                delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0,
            &modifiers,
            &mut state,
            &root_obj,
            &mut handled,
            &mut focus,
            root,
            &taffy,
        );
        assert!(
            !handled,
            "wheel over a fitting row must not claim the event so StripView \
             retains its inter-strip pagination"
        );
        assert_eq!(
            row.scroll_x(),
            0.0,
            "no overflow → scroll_x must remain at 0"
        );
    }

    #[test]
    fn scroll_snaps_to_nearest_group_after_wheel_idle() {
        // After the user wheel-pans the strip past a group boundary
        // and stops, the strip must ease `scroll_x` to the nearest
        // group's left edge with a cubic-out curve over ~200 ms.
        // Mirrors the concept's `snap-x snap-proximity` behaviour in
        // `DesktopView.tsx:396`.
        //
        // Synthesise the geometry directly (three 600-px groups, 8 px
        // gap → left edges at 0, 608, 1216) so we don't depend on
        // taffy's flex math for the precise stop positions.
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
        row.set_content_w_for_test(1816.0);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
        // Pretend we panned to mid-second-group (scroll_x = 250) and
        // stopped wheeling 250 ms ago. Nearest left-edge to 250 is 0
        // (distance 250) vs 608 (distance 358) — first group wins,
        // BUT 250 is closer to 0 by simple |edge - scroll_x|. To
        // exercise the "snap to the SECOND group's left edge" branch
        // we start from 400 (closer to 608 than to 0).
        row.set_scroll_x_for_test(400.0);
        let base = Instant::now();
        row.set_last_wheel_at_for_test(base - Duration::from_millis(250));

        // First tick after idle: snap kicks off and advances ~25 % of
        // the way (50 ms into a 200 ms cubic-out curve, so eased
        // ≈ 1 - (1 - 0.25)^3 ≈ 0.578). scroll_x must have moved
        // toward 608 but not yet arrived.
        row.tick_snap_at(base + Duration::from_millis(50));
        assert!(
            row.scroll_x() > 400.0,
            "snap must move scroll_x toward the target; got {}",
            row.scroll_x()
        );
        assert!(
            row.scroll_x() < 608.0,
            "snap must NOT have arrived after only 50 ms; got {}",
            row.scroll_x()
        );

        // After the full 200 ms, scroll_x must have settled at the
        // target group's left edge.
        row.tick_snap_at(base + Duration::from_millis(250));
        assert!(
            (row.scroll_x() - 608.0).abs() < 1.0,
            "snap must settle at 608.0 after 200 ms; got {}",
            row.scroll_x()
        );
    }

    #[test]
    fn snap_does_not_fire_during_wheel_idle_window() {
        // Before the 200 ms idle threshold elapses, the snap must
        // NOT fire — we don't want the strip pulling itself out from
        // under the user mid-wheel.
        let mut row = make_row_with_groups(&[&[1], &[2], &[3]]);
        row.set_group_left_edges_for_test(vec![0.0, 608.0, 1216.0]);
        row.set_content_w_for_test(1816.0);
        row.set_rect_for_test(Rect::new(0.0, 0.0, 1000.0, 600.0));
        row.set_scroll_x_for_test(400.0);
        let base = Instant::now();
        // Only 100 ms since the last wheel — well inside the idle
        // window.
        row.set_last_wheel_at_for_test(base - Duration::from_millis(100));

        row.tick_snap_at(base);
        assert_eq!(
            row.scroll_x(),
            400.0,
            "snap must not fire while still inside the idle window",
        );
    }

    #[test]
    fn fresh_wheel_cancels_in_flight_snap() {
        // If the user starts panning again mid-snap, the snap must
        // cancel — the new wheel event resets `last_wheel_at`, the
        // idle gate fails, and the existing snap is dropped on the
        // next tick. Without this, the strip would fight the user's
        // input mid-curve.
        use winit::dpi::PhysicalPosition;

        let (mut row, taffy, root) = make_overflow_row(4, 1200.0);
        // Stage an in-flight snap directly so we don't have to wait
        // out a real idle window.
        row.set_scroll_x_for_test(300.0);
        let base = Instant::now();
        row.set_last_wheel_at_for_test(base - Duration::from_millis(250));
        row.tick_snap_at(base);
        assert!(
            row.snap_anim.is_some(),
            "precondition: snap must be in flight",
        );

        // Drive a fresh wheel event. The cursor must be inside the
        // row's rect for the wheel handler to claim it.
        let cy = (row.rect.y0 + row.rect.y1) / 2.0;
        let cx = (row.rect.x0 + row.rect.x1) / 2.0;
        let mut state = AutoCommit::new();
        let root_obj = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let dev = unsafe { std::mem::zeroed() };
        row.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: PhysicalPosition::new(cx, cy),
            },
            1.0, &modifiers, &mut state, &root_obj,
            &mut handled, &mut focus, root, &taffy,
        );
        row.handle_event(
            &WindowEvent::MouseWheel {
                device_id: dev,
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0, &modifiers, &mut state, &root_obj,
            &mut handled, &mut focus, root, &taffy,
        );
        assert!(
            row.snap_anim.is_none(),
            "fresh wheel event must cancel the in-flight snap",
        );
    }
}
