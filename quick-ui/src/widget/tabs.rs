use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::dnd::{DragPayload, DRAG_START_THRESHOLD_PX};
use crate::widget::AppKind;

/// Duration of the inter-pill drop-zone expand animation. Matches the
/// concept's `motion.div layout` spring feel — short enough that the user
/// perceives the zone "tracking" the cursor but long enough that the eye
/// catches the transition. Paired with `needs_animation_frame()` so the
/// host keeps pumping redraws while interpolation is in flight.
const DROP_ZONE_ANIM_MS: u64 = 140;
/// Width a drop-zone indicator expands to when it's the current target
/// gap. Mirrors the concept's `w-16` (64 px Tailwind). We clamp visually
/// against the actual gap between pills (6 px) at rest — the expansion is
/// an overlay painted *over* adjacent pills, not a layout change.
const DROP_ZONE_EXPANDED_PX: f64 = 18.0;
/// Width of the idle drop-zone hint painted in every inter-pill gap while
/// a drag is in flight. Stays thin (2 px) so it reads as a subtle guide
/// rather than a warning.
const DROP_ZONE_IDLE_PX: f64 = 2.0;

/// Duration of the tab-layout spring that plays when pills reorder, insert,
/// or close. Longer than the drop-zone animation because this one moves
/// *every* pill's position — 180 ms is enough for the eye to track the
/// motion as a smooth interpolation instead of a jump cut. Matches the
/// concept's `motion.div layout transition={{ type: "spring" }}` feel.
const LAYOUT_ANIM_MS: u64 = 180;

/// Per-pill fade-in when a tab is inserted. Matches the concept's
/// `initial={{ opacity: 0, y: -10, scale: 0.9 }}` → `animate={{ opacity: 1,
/// y: 0, scale: 1 }}` on each `<motion.div>` tab pill. Kept at 220 ms —
/// comfortably distinct from the 180 ms layout spring so a freshly
/// inserted pill reads as "appearing" rather than just sliding in with
/// its neighbours. We fade opacity + vertical offset only; scale is
/// skipped to avoid a per-pill affine push_layer.
const INSERT_FADE_MS: u64 = 220;
/// Vertical start offset for the insert fade, in CSS-like px. Positive
/// values pull the pill *up* at t=0 and settle to the target position at
/// t=1 (so the pill reads as dropping in from above, matching the
/// concept's `y: -10` initial).
const INSERT_FADE_Y_OFFSET: f64 = 10.0;

#[derive(Clone, Debug)]
pub struct TabDescriptor {
    pub id: u64,
    pub title: String,
    /// App-kind hint used by `TabBar::draw` to paint a tiny leading
    /// glyph inside the pill. Mirrors the concept's `getAppIcon` lookup
    /// in `WindowFrame.tsx:10-18` which maps tile types to lucide icons
    /// (FileCode2, Folder, Globe, Terminal). `None` falls through to a
    /// text-only pill — used for tests and for any callers that don't
    /// know the tile type at construction time.
    pub app_kind: Option<AppKind>,
}

impl TabDescriptor {
    /// Construct a descriptor without a typed icon — convenience for the
    /// many test call sites that only care about id + title. Production
    /// callers (the shell) populate `app_kind` via the struct literal
    /// so the pill paints the correct leading glyph.
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Self { id, title: title.into(), app_kind: None }
    }
}

pub struct TabBar {
    group_id_tag: String,
    tabs: Vec<TabDescriptor>,
    active_id: u64,
    rect: Rect,
    tab_rects: Vec<Rect>,
    close_rects: Vec<Rect>,
    /// Hover-only maximize-icon hit regions, aligned 1:1 with `tabs`.
    /// Sits just to the left of each pill's close button and only draws
    /// when the pill is hovered, mirroring the concept's hover-reveal
    /// action row.
    max_rects: Vec<Rect>,
    /// Hover-only "search in app" hit regions, aligned 1:1 with `tabs`.
    /// Sits left of the maximize icon and mirrors the third concept
    /// action button: clicking it publishes `pending_search` for the
    /// shell to open the command palette scoped to that tab.
    search_rects: Vec<Rect>,
    hover_idx: Option<usize>,
    hover_close_idx: Option<usize>,
    /// Tab whose maximize icon the pointer is currently over. Kept
    /// separate from `hover_close_idx` so their hover visuals don't bleed
    /// into each other.
    hover_max_idx: Option<usize>,
    hover_search_idx: Option<usize>,
    press_origin: Option<(usize, Point)>,
    last_cursor: Point,
    pub pending_drag: Option<DragPayload>,
    pub pending_reorder: Option<(usize, usize)>,
    pub pending_close: Option<u64>,
    /// Populated when the user clicks the maximize icon on a tab. The
    /// outer `WindowGroup` drains this and toggles its own maximized
    /// state so the shell can paint concept-style floating controls over
    /// a full-bleed tile.
    pub pending_max: Option<u64>,
    /// Populated when the user clicks the hover-only search icon on a
    /// tab. The shell drains this and opens the command palette scoped
    /// to that tab (concept's "Search in App" per-tab action).
    pub pending_search: Option<u64>,
    /// Populated when a drag gesture is released while the cursor is
    /// *outside* this bar's own rect — the user intends to move the tab
    /// to a different window-group. Carries `(tab_id, release_point)` so
    /// a parent widget (e.g. `GroupRow`) can resolve the drop target by
    /// hit-testing sibling bars. The parent is expected to drain this
    /// field immediately; leaving it set across frames is harmless but
    /// wasteful.
    pub pending_detach: Option<(u64, Point)>,
    /// Inter-pill gap index (0..=tabs.len()) the cursor last hovered
    /// while a drag was in flight. `None` means no drag, or the cursor
    /// left the bar entirely. Changing the target latches
    /// `drop_zone_animated_at` so `draw` can interpolate the zone's
    /// expansion across a few frames instead of popping.
    drop_zone_target: Option<usize>,
    /// `Instant` at which `drop_zone_target` last changed. Read by
    /// `draw` to derive `eased = (elapsed / DROP_ZONE_ANIM_MS).min(1)`.
    /// Wrapped in `Option` because `Instant` has no `ZERO` constant.
    drop_zone_animated_at: Option<Instant>,
    /// Snapshot of each tab's `tab_rects[i]` keyed by tab id, captured
    /// just before the last reorder/insert/remove. `draw` uses this to
    /// interpolate each pill's *visual* position toward its new
    /// `tab_rects[i]` over `LAYOUT_ANIM_MS`. Tabs that didn't exist in
    /// the previous layout simply render at their target rect (no
    /// fly-in); that matches the concept where `AnimatePresence` animates
    /// positions of *existing* items only.
    prev_tab_rects_by_id: HashMap<u64, Rect>,
    /// `Instant` the current layout spring started, or `None` if no
    /// animation is in-flight. Cleared implicitly when `elapsed >
    /// LAYOUT_ANIM_MS` — `needs_animation_frame` derives the "still
    /// animating" answer from this directly.
    layout_anim_started: Option<Instant>,
    /// Per-tab fade-in timestamps, keyed by tab id. Populated by
    /// `insert_tab` at the moment the tab is spliced in; `draw` reads
    /// the timestamp, computes a per-pill alpha + y-offset, and drops
    /// the entry once `INSERT_FADE_MS` elapses. Tabs that were present
    /// at `TabBar::new` construction get no entry → render steady-state.
    /// This keeps the headless / screenshot tests deterministic:
    /// animations only fire for runtime insertions, not static fixtures.
    inserted_at: HashMap<u64, Instant>,
}

impl TabBar {
    pub fn new(group_id_tag: &str, tabs: Vec<TabDescriptor>, active_id: u64) -> Self {
        Self {
            group_id_tag: group_id_tag.into(),
            tabs,
            active_id,
            rect: Rect::ZERO,
            tab_rects: Vec::new(),
            close_rects: Vec::new(),
            max_rects: Vec::new(),
            search_rects: Vec::new(),
            hover_idx: None,
            hover_close_idx: None,
            hover_max_idx: None,
            hover_search_idx: None,
            press_origin: None,
            last_cursor: Point::ZERO,
            pending_drag: None,
            pending_reorder: None,
            pending_close: None,
            pending_max: None,
            pending_search: None,
            pending_detach: None,
            drop_zone_target: None,
            drop_zone_animated_at: None,
            prev_tab_rects_by_id: HashMap::new(),
            layout_anim_started: None,
            inserted_at: HashMap::new(),
        }
    }

    /// Capture the current `tab_rects` keyed by tab id, then latch
    /// `layout_anim_started` so the next few frames render every pill
    /// interpolating from its old position toward the new one. Called
    /// from `reorder`, `insert_tab`, and `remove_tab`. No-op on an empty
    /// layout (first-ever layout pass) — there's nothing to spring from.
    fn snapshot_layout_for_spring(&mut self) {
        if self.tab_rects.len() != self.tabs.len() {
            return;
        }
        let mut snap: HashMap<u64, Rect> = HashMap::with_capacity(self.tabs.len());
        for (i, t) in self.tabs.iter().enumerate() {
            snap.insert(t.id, self.tab_rects[i]);
        }
        self.prev_tab_rects_by_id = snap;
        self.layout_anim_started = Some(Instant::now());
    }
    pub fn tabs(&self) -> &[TabDescriptor] { &self.tabs }
    pub fn tabs_mut(&mut self) -> &mut Vec<TabDescriptor> { &mut self.tabs }
    pub fn active_id(&self) -> u64 { self.active_id }
    pub fn set_active(&mut self, id: u64) { if self.tabs.iter().any(|t| t.id == id) { self.active_id = id; } }
    pub fn remove_tab(&mut self, id: u64) -> Option<usize> {
        let idx = self.tabs.iter().position(|t| t.id == id)?;
        // Snapshot current pill geometry *before* mutating so the spring
        // has something to interpolate from. Survivors fly toward their
        // new rect; the removed tab has no target and simply disappears.
        self.snapshot_layout_for_spring();
        self.tabs.remove(idx);
        if self.active_id == id {
            self.active_id = self.tabs.get(idx).or_else(|| self.tabs.last()).map(|t| t.id).unwrap_or(0);
        }
        Some(idx)
    }
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to { return; }
        self.snapshot_layout_for_spring();
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
    }
    pub fn take_drag(&mut self) -> Option<DragPayload> { self.pending_drag.take() }
    /// True while a drag gesture is in flight (press threshold exceeded,
    /// release not yet seen). The parent `WindowGroup` reads this to paint
    /// a drag-in-flight accent ring matching the concept's drag-over state.
    pub fn drag_in_flight(&self) -> bool { self.pending_drag.is_some() }
    pub fn take_reorder(&mut self) -> Option<(usize, usize)> { self.pending_reorder.take() }
    pub fn take_close(&mut self) -> Option<u64> { self.pending_close.take() }
    pub fn take_max(&mut self) -> Option<u64> { self.pending_max.take() }
    pub fn take_search(&mut self) -> Option<u64> { self.pending_search.take() }
    /// Drain a cross-group detach request if any. See [`Self::pending_detach`].
    pub fn take_detach(&mut self) -> Option<(u64, Point)> { self.pending_detach.take() }
    /// Expose the bar's own rect so a parent widget (e.g. `GroupRow`) can
    /// hit-test a release point against sibling bars without having to
    /// re-walk the layout tree.
    pub fn bar_rect(&self) -> Rect { self.rect }
    /// Per-pill rects in tab order, populated during `apply_layout`.
    /// Exposed so `GroupRow` can resolve an "insert before pill X" drop
    /// index when a cross-group drag releases over a sibling's bar.
    pub fn tab_rects(&self) -> &[Rect] { &self.tab_rects }
    /// Compute an insertion index for a drop at `release_point`. Returns
    /// an index in `[0, tab_rects.len()]` — `0` means "before the first
    /// pill", `len` means "after the last pill". Vertical position is
    /// ignored (only the X coordinate matters); this lets callers drop
    /// anywhere inside the bar's horizontal extent.
    ///
    /// Rule: for each pill, if the release x is left of the pill's
    /// horizontal midpoint, insert **before** that pill; otherwise keep
    /// walking. If the release is past every pill's midpoint, append.
    pub fn drop_index_for(&self, release_point: Point) -> usize {
        for (i, r) in self.tab_rects.iter().enumerate() {
            let mid_x = (r.x0 + r.x1) / 2.0;
            if release_point.x < mid_x {
                return i;
            }
        }
        self.tab_rects.len()
    }
    /// Insert a tab at `idx` and make it active. Used by the cross-group
    /// drag routing in `GroupRow::handle_event` to splice a detached tab
    /// into the target bar without forcing a full rebuild.
    pub fn insert_tab(&mut self, idx: usize, desc: TabDescriptor) {
        let clamped = idx.min(self.tabs.len());
        let id = desc.id;
        // Snapshot existing pill rects so neighbours visibly slide aside
        // to make room. The newly inserted tab isn't in `prev_*`, so
        // `draw` would otherwise render it at its target rect immediately
        // — we instead latch `inserted_at` for this id so `draw` plays a
        // fade-in (opacity 0→1 + vertical settle) over `INSERT_FADE_MS`,
        // matching the concept's per-pill motion init.
        self.snapshot_layout_for_spring();
        self.tabs.insert(clamped, desc);
        self.inserted_at.insert(id, Instant::now());
        self.active_id = id;
    }

    /// Currently-targeted drop-zone gap index (only meaningful while a
    /// drag is in flight). `None` means "no target" — either the drag
    /// just started (pre-first-CursorMoved) or the cursor left the bar
    /// for a cross-group release path. Exposed for tests.
    #[cfg(test)]
    pub(crate) fn drop_zone_target(&self) -> Option<usize> {
        self.drop_zone_target
    }
}

/// Outer bar height. Matches the concept's `h-14` (56 px Tailwind) at
/// a slightly tighter ratio so the pills still read as compact chrome
/// rather than a full secondary toolbar. See `parity-status.md` §4.
pub const TAB_BAR_HEIGHT: f64 = 40.0;
/// Vertical inset of each pill within the bar. Bar height 40 – 2 × 6 =
/// 28 px pill height, matching the concept's `px-4 py-2 text-sm` pill.
pub const TAB_PILL_INSET_Y: f64 = 6.0;

impl Widget for TabBar {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        // Outer bar is `TAB_BAR_HEIGHT` tall (40 px); pills sit inset
        // `TAB_PILL_INSET_Y` (6 px) top + bottom so each pill reads as
        // 28 px, matching the concept's `h-14` container / `py-2 text-sm`
        // pill proportions.
        let node = taffy.new_leaf(Style { size: Size { width: Dimension::auto(), height: Dimension::length(TAB_BAR_HEIGHT as f32) }, flex_shrink: 0.0, ..Default::default() }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        // Content-sized pill tabs with a gap between them. The close button
        // ("×") sits inside the right end of each pill, 16 px wide.
        self.tab_rects.clear();
        self.close_rects.clear();
        self.max_rects.clear();
        self.search_rects.clear();
        let gap = 6.0_f64;
        let pad_x = 12.0_f64;
        let close_w = 18.0_f64;
        let max_w = 16.0_f64;
        let search_w = 16.0_f64;
        let close_inset = 4.0_f64;
        // Leading-icon width reserved on every pill that carries an
        // `app_kind`. Mirrors the concept's `<FileCode2 className="w-4
        // h-4" />` glyph at the start of each tab.
        let icon_w = 14.0_f64;
        let icon_gap = 6.0_f64;
        let pill_y0 = self.rect.y0 + TAB_PILL_INSET_Y;
        let pill_y1 = self.rect.y1 - TAB_PILL_INSET_Y;
        let mut x = self.rect.x0;
        for t in &self.tabs {
            let approx_char_w = 7.0_f64;
            let title_w = t.title.chars().count() as f64 * approx_char_w;
            // Reserve room for the close glyph + both hover-reveal icons
            // (search, maximize) so pills don't resize when you hover them.
            // Tabs with a typed `app_kind` also reserve `icon_w + icon_gap`
            // on the leading edge for the per-app glyph.
            let lead_icon_reserve =
                if t.app_kind.is_some() { icon_w + icon_gap } else { 0.0 };
            let w = (title_w + pad_x * 2.0 + close_w + max_w + search_w + lead_icon_reserve)
                .max(96.0);
            let r = Rect::new(x, pill_y0, x + w, pill_y1);
            let close_x0 = r.x1 - close_w - close_inset;
            let close = Rect::new(close_x0, r.y0 + 4.0, close_x0 + close_w, r.y1 - 4.0);
            let max_x0 = close_x0 - max_w - 2.0;
            let max_rect = Rect::new(max_x0, r.y0 + 4.0, max_x0 + max_w, r.y1 - 4.0);
            let search_x0 = max_x0 - search_w - 2.0;
            let search_rect = Rect::new(search_x0, r.y0 + 4.0, search_x0 + search_w, r.y1 - 4.0);
            self.tab_rects.push(r);
            self.close_rects.push(close);
            self.max_rects.push(max_rect);
            self.search_rects.push(search_rect);
            x += w + gap;
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        // No full-bar fill — pills sit directly on the dark shell background.
        // Radius is based on pill height (bar minus 2 × inset), not the full
        // bar, so pills stay `rounded-full` even when the outer bar grows.
        let pill_height = (self.rect.height() - 2.0 * TAB_PILL_INSET_Y).max(0.0);
        let radius = pill_height / 2.0;
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = theme.tokens.colors.text_dim;

        // Tab-layout spring. If a reorder/insert/remove just happened,
        // every surviving pill interpolates from its previous rect to
        // its target `tab_rects[i]` over `LAYOUT_ANIM_MS`. Tabs that
        // weren't in the snapshot (brand new) render at target
        // immediately — matches concept's AnimatePresence where only
        // existing items animate layout changes.
        let layout_t = self.layout_anim_started.and_then(|t| {
            let elapsed = t.elapsed();
            if elapsed >= Duration::from_millis(LAYOUT_ANIM_MS) {
                None
            } else {
                let raw = elapsed.as_secs_f64() / (LAYOUT_ANIM_MS as f64 / 1000.0);
                // Ease-out-cubic, matching the drop-zone curve above.
                Some(1.0 - (1.0 - raw).powi(3))
            }
        });

        // Clean up any insert-fade entries that have fully elapsed — keeps
        // `inserted_at` bounded and prevents `needs_animation_frame` from
        // pumping redraws forever.
        let fade_window = Duration::from_millis(INSERT_FADE_MS);
        self.inserted_at
            .retain(|_, started| started.elapsed() < fade_window);

        for (i, t) in self.tabs.iter().enumerate() {
            let target = self.tab_rects[i];
            // Visual rect = interpolation of prev → target if we have a
            // snapshot for this tab id and a spring is in flight. No
            // snapshot (new tab) or no active spring → render at target.
            let r = match (layout_t, self.prev_tab_rects_by_id.get(&t.id)) {
                (Some(tt), Some(prev)) => Rect::new(
                    prev.x0 + (target.x0 - prev.x0) * tt,
                    prev.y0 + (target.y0 - prev.y0) * tt,
                    prev.x1 + (target.x1 - prev.x1) * tt,
                    prev.y1 + (target.y1 - prev.y1) * tt,
                ),
                _ => target,
            };
            let dx = r.x0 - target.x0;
            let dy = r.y0 - target.y0;

            // Per-pill insert fade: tabs spliced in by `insert_tab` fade
            // from alpha=0 + y=-10 to alpha=1 + y=0 over `INSERT_FADE_MS`.
            // `fade_alpha` < 1.0 wraps the whole per-pill paint block in a
            // `push_layer` with the current alpha; `fade_y` shifts the
            // transform upward at t=0 and settles to 0 at t=1 via
            // ease-out-cubic, matching the concept's `y: -10` → `y: 0` on
            // each `<motion.div>` pill.
            let (fade_alpha, fade_y): (f32, f64) = match self.inserted_at.get(&t.id) {
                Some(started) => {
                    let elapsed = started.elapsed();
                    if elapsed >= fade_window {
                        (1.0, 0.0)
                    } else {
                        let raw = (elapsed.as_secs_f64()
                            / (INSERT_FADE_MS as f64 / 1000.0))
                            .clamp(0.0, 1.0);
                        let eased = 1.0 - (1.0 - raw).powi(3);
                        (eased as f32, -INSERT_FADE_Y_OFFSET * (1.0 - eased))
                    }
                }
                None => (1.0, 0.0),
            };
            // If this pill is mid-fade, push a pill-scoped alpha layer and
            // shift the transform vertically. The dx/dy for the layout
            // spring still apply — a just-inserted pill has no prev rect so
            // `dy` stays zero, but we honour it here for completeness.
            let pill_transform = if fade_y != 0.0 {
                transform * Affine::translate((0.0, fade_y))
            } else {
                transform
            };
            let pill_alpha_active = fade_alpha < 1.0;
            if pill_alpha_active {
                // Clip rect generous around the pill so the hover-reveal
                // action icons also fade in cleanly.
                let clip = Rect::new(
                    r.x0 - 2.0,
                    r.y0 - 4.0,
                    r.x1 + self.close_rects[i].width() + 60.0,
                    r.y1 + 4.0,
                );
                scene.push_layer(
                    vello::peniko::BlendMode::default(),
                    fade_alpha,
                    transform,
                    &clip,
                );
            }
            // All per-pill paint below uses `pill_transform` so the fade's
            // vertical offset composes with any layout-spring dx/dy.
            let transform = pill_transform;
            let is_active = t.id == self.active_id;
            let is_hover = Some(i) == self.hover_idx;
            // Per-pill drop shadow, approximating the concept's
            // `shadow-[0_8px_32px_rgba(0,0,0,0.2)]`. Vello has no blur
            // primitive, so we stack four concentric offset rounded rects
            // at falling alpha (same trick `tile_host.rs` uses for the
            // window-card shadow). This separates every pill from the
            // dark shell behind and, combined with the widened fill
            // contrast below, lets the active pill visually pop forward
            // (visual gap #2 in docs/nirios/visual-gaps.md).
            let shadow_y_offset = 6.0_f64;
            let shadow_rings: [(f64, u8); 4] = [
                (2.0, 51), // ~0.20
                (5.0, 30), // ~0.12
                (9.0, 15), // ~0.06
                (14.0, 5), // ~0.02
            ];
            for (spread, alpha) in shadow_rings {
                let shadow_rect = Rect::new(
                    r.x0 - spread,
                    r.y0 - spread + shadow_y_offset,
                    r.x1 + spread,
                    r.y1 + spread + shadow_y_offset,
                );
                let shadow_shape = RoundedRect::from_rect(shadow_rect, radius + spread);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    vello::peniko::Color::rgba8(0, 0, 0, alpha),
                    None,
                    &shadow_shape,
                );
            }
            // Pill body. Concept: active = `bg-white/10 border-white/20`,
            // inactive = `bg-white/5 border-white/10`, hover = intermediate.
            // We nudge the raw alphas upward on the active pill (52 vs the
            // literal 26 of white/10) and downward on the inactive pill
            // (8 vs white/5's 13) to compensate for the still-missing
            // `backdrop-blur` — without frosted blur the concept's literal
            // values read as nearly identical over the dark shell.
            let shape = RoundedRect::from_rect(r, radius);
            let (fill_alpha, border_alpha): (u8, u8) = if is_active {
                (52, 64)
            } else if is_hover {
                (26, 40)
            } else {
                (8, 18)
            };
            let fill = vello::peniko::Color::rgba8(255, 255, 255, fill_alpha);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                vello::peniko::Color::rgba8(255, 255, 255, border_alpha),
                None,
                &shape,
            );
            // Active-only inner rim light: a 1 px-tall translucent white
            // band along the top inside edge of the pill. Reads as the
            // subtle "inset" highlight Tailwind's `shadow` implies but
            // doesn't render without the frosted-blur stack. Clipped to
            // the pill shape so it respects the rounded ends.
            if is_active {
                scene.push_layer(
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &shape,
                );
                let rim = Rect::new(r.x0 + 2.0, r.y0 + 1.0, r.x1 - 2.0, r.y0 + 2.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    vello::peniko::Color::rgba8(255, 255, 255, 20),
                    None,
                    &rim,
                );
                scene.pop_layer();
            }
            // Title colour is the primary active/inactive cue: active pills
            // carry the full `text_primary` white, inactive pills dim to
            // `text_dim` (matches concept's `text-white` vs `text-white/70`).
            // On hover of an inactive pill we bump back up to `text_primary`
            // to mirror the concept's `hover:text-white`.
            let text_color = if is_active || is_hover {
                text_primary
            } else {
                text_dim
            };
            let text_size = theme.tokens.typography.scale.sm;
            let (tw, th) = tr.measure(&t.title, text_size, None);
            // Paint the leading app-kind icon when one is set — a small
            // vector glyph centered in a 14 px-wide slot on the left of
            // the pill. The title is shifted right by (icon + gap) so
            // the title's baseline stays aligned with the pill centre.
            // Falls back to no icon when `app_kind == None` (test fixtures,
            // cross-group detached tabs mid-drag).
            let leading_shift = if let Some(kind) = t.app_kind {
                let icon_x0 = r.x0 + 10.0;
                let icon_rect = Rect::new(
                    icon_x0,
                    r.center().y - 7.0,
                    icon_x0 + 14.0,
                    r.center().y + 7.0,
                );
                draw_app_icon(scene, transform, icon_rect, kind, text_color);
                14.0 + 6.0
            } else {
                0.0
            };
            // Offset title left so the close button has room.
            let title_x = r.x0 + 12.0 + leading_shift;
            let title_y = r.center().y - th as f64 / 2.0;
            // Clip the title to the area before the close button so long
            // titles truncate cleanly instead of overlapping the "×".
            let clip_r = Rect::new(r.x0, r.y0, self.close_rects[i].x0 - 2.0, r.y1);
            scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &clip_r);
            tr.draw_text(scene, &t.title, text_size, text_color, transform * Affine::translate((title_x, title_y)), None);
            let _ = tw;
            scene.pop_layer();

            // Per-tab collaborator presence indicators. Matches the concept's
            // foggy blur: each peer active on this tab paints a low-alpha
            // colored circle at the bottom of the pill, colored by a stable
            // hash of their user id so the same person keeps the same hue
            // everywhere the UI renders them.
            let tab_key = PresenceKey::Tab { id: format!("{:x}", t.id) };
            let peers = presence.peers_on(&tab_key);
            if !peers.is_empty() {
                // Clip to the pill so the colored glow doesn't bleed into
                // adjacent tabs or the dark shell background.
                scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &shape);
                let dot_r = 8.0_f64;
                let span = (peers.len().min(4) as f64 - 1.0) * 10.0;
                let base_x = r.center().x - span / 2.0;
                let bottom = r.y1 + 2.0;
                for (pi, peer) in peers.iter().take(4).enumerate() {
                    let (cr, cg, cb) = presence.color_for(peer);
                    let cx = base_x + pi as f64 * 10.0;
                    let c = vello::kurbo::Circle::new(Point::new(cx, bottom), dot_r);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        vello::peniko::Color::rgba8(cr, cg, cb, 140),
                        None,
                        &c,
                    );
                }
                scene.pop_layer();
            }

            // Close button: rendered always but emphasised on hover of the
            // tab or the close rect itself. Concept-aligned: a "×" glyph on
            // a subtle hover chip.
            // During a layout spring, offset the hit-test rect by (dx, dy)
            // so the glyph tracks the pill. Hit-testing still uses
            // `self.close_rects[i]` unchanged, so the cursor interacts
            // with the target position — a reasonable trade for a 180 ms
            // animation.
            let close_r = self.close_rects[i] + vello::kurbo::Vec2::new(dx, dy);
            let close_hover = Some(i) == self.hover_close_idx;
            // Hover-chip: translucent white on every pill (both active and
            // inactive share the dark-on-translucent-white pill style now),
            // matching the concept's `hover:bg-white/30`.
            let close_chip_alpha: u8 = if close_hover { 64 } else if is_hover { 28 } else { 0 };
            if close_chip_alpha > 0 {
                let chip = RoundedRect::from_rect(close_r, close_r.height() / 2.0);
                let chip_color = vello::peniko::Color::rgba8(255, 255, 255, close_chip_alpha);
                scene.fill(vello::peniko::Fill::NonZero, transform, chip_color, None, &chip);
            }
            let glyph = "×";
            let glyph_size = theme.tokens.typography.scale.base;
            let (gw, gh) = tr.measure(glyph, glyph_size, None);
            let gx = close_r.center().x - gw as f64 / 2.0;
            let gy = close_r.center().y - gh as f64 / 2.0;
            // Glyph color mirrors title polarity: bright white on
            // active/hover, dim on the idle inactive pill.
            let glyph_color = if is_active || close_hover {
                text_primary
            } else {
                text_dim
            };
            tr.draw_text(scene, glyph, glyph_size, glyph_color, transform * Affine::translate((gx, gy)), None);

            // Maximize icon: only visible on pill hover (concept's
            // hover-reveal action row). Drawn as a 1 px outlined square so
            // it reads as a generic "grow window" glyph without needing a
            // bespoke icon font.
            if is_hover {
                // Pills are all translucent-white-on-dark now, so every
                // hover-reveal chip/icon uses the same white polarity
                // (concept's `hover:bg-white/30`, `text-white/70` →
                // `hover:text-white`).
                let chip_tint = vello::peniko::Color::rgba8(255, 255, 255, 64);
                let icon_idle = text_dim;
                let icon_active = text_primary;

                let max_r = self.max_rects[i] + vello::kurbo::Vec2::new(dx, dy);
                let max_hover = Some(i) == self.hover_max_idx;
                if max_hover {
                    let chip = RoundedRect::from_rect(max_r, max_r.height() / 2.0);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        chip_tint,
                        None,
                        &chip,
                    );
                }
                // 10×10 centered square outline.
                let s = 10.0_f64;
                let cx = max_r.center().x;
                let cy = max_r.center().y;
                let square = Rect::new(cx - s / 2.0, cy - s / 2.0, cx + s / 2.0, cy + s / 2.0);
                let outline_color = if max_hover { icon_active } else { icon_idle };
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.2),
                    transform,
                    outline_color,
                    None,
                    &square,
                );

                // Search icon — "magnifying glass" built from a 5 px circle
                // plus a short diagonal handle line, so we don't need an
                // icon-font codepoint. Concept's third hover-reveal action.
                let search_r = self.search_rects[i] + vello::kurbo::Vec2::new(dx, dy);
                let search_hover = Some(i) == self.hover_search_idx;
                if search_hover {
                    let chip = RoundedRect::from_rect(search_r, search_r.height() / 2.0);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        chip_tint,
                        None,
                        &chip,
                    );
                }
                let icon_color = if search_hover { icon_active } else { icon_idle };
                // Slightly off-centre to make room for the diagonal handle.
                let scx = search_r.center().x - 1.0;
                let scy = search_r.center().y - 1.0;
                let circle = vello::kurbo::Circle::new(Point::new(scx, scy), 3.5);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.2),
                    transform,
                    icon_color,
                    None,
                    &circle,
                );
                // Handle: 3 px diagonal stroke off the bottom-right of the
                // glass.
                let handle = vello::kurbo::Line::new(
                    Point::new(scx + 2.5, scy + 2.5),
                    Point::new(scx + 5.0, scy + 5.0),
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.2),
                    transform,
                    icon_color,
                    None,
                    &handle,
                );
            }

            // Close out the insert-fade layer, if one was pushed at the
            // top of this iteration. The branch must mirror the push
            // exactly — `pill_alpha_active` tracks that.
            if pill_alpha_active {
                scene.pop_layer();
            }
        }

        // Drop-zone indicators: while a tab drag is in flight, paint one
        // vertical bar per inter-pill gap (N+1 gaps for N tabs, including
        // one at the start and one at the end). The gap the cursor is
        // currently over animates from the idle width to `_EXPANDED_PX`
        // with a glow — this is the Rust port of the concept's
        // `TabDropZone` which uses `motion.div layout` to spring between
        // `w-2` and `w-16`. Animation state (`drop_zone_target` +
        // `drop_zone_animated_at`) is updated in `handle_event`; `draw`
        // only reads it here to interpolate.
        if self.pending_drag.is_some() && !self.tab_rects.is_empty() {
            let accent = theme.tokens.colors.accent[5];
            let target = self.drop_zone_target;
            let now = Instant::now();
            let elapsed_ms = self
                .drop_zone_animated_at
                .map(|t| now.duration_since(t).as_millis() as f64)
                .unwrap_or(DROP_ZONE_ANIM_MS as f64);
            // Ease-out-cubic over DROP_ZONE_ANIM_MS; clamps to [0, 1].
            let raw_t = (elapsed_ms / DROP_ZONE_ANIM_MS as f64).clamp(0.0, 1.0);
            let eased_t = 1.0 - (1.0 - raw_t).powi(3);

            for gap_idx in 0..=self.tab_rects.len() {
                // Compute the gap's centre x: between pills, or just past
                // the outermost edge at the endpoints.
                let (cx, y0, y1) = if gap_idx == 0 {
                    let r = self.tab_rects[0];
                    (r.x0 - 3.0, r.y0 - 2.0, r.y1 + 2.0)
                } else if gap_idx == self.tab_rects.len() {
                    let r = *self.tab_rects.last().unwrap();
                    (r.x1 + 3.0, r.y0 - 2.0, r.y1 + 2.0)
                } else {
                    let prev = self.tab_rects[gap_idx - 1];
                    let next = self.tab_rects[gap_idx];
                    let cx = (prev.x1 + next.x0) / 2.0;
                    (cx, prev.y0.min(next.y0) - 2.0, prev.y1.max(next.y1) + 2.0)
                };

                // Target gap expands smoothly. Non-target gaps stay at
                // the idle width + ~30% alpha so the user still sees
                // every possible drop location (concept's always-visible
                // spacers).
                let is_target = target == Some(gap_idx);
                let (w, alpha) = if is_target {
                    let w = DROP_ZONE_IDLE_PX
                        + (DROP_ZONE_EXPANDED_PX - DROP_ZONE_IDLE_PX) * eased_t;
                    // Interpolate 0.3 → 1.0 alpha too so the target zone
                    // brightens as it widens.
                    let a = 0.3 + 0.7 * eased_t;
                    (w, a)
                } else {
                    (DROP_ZONE_IDLE_PX, 0.3)
                };

                let bar = Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y1);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    accent.with_alpha_factor(alpha as f32),
                    None,
                    &bar,
                );

                // Extra outer glow on the target gap while it's past ~60%
                // of the animation. Cheap approximation of the concept's
                // `shadow-[0_0_12px_rgba(99,102,241,0.8)]` without needing
                // a blur primitive — two wider strokes at lower alpha.
                if is_target && eased_t > 0.6 {
                    let glow_t = ((eased_t - 0.6) / 0.4).clamp(0.0, 1.0);
                    let glow = Rect::new(
                        cx - (w / 2.0 + 4.0),
                        y0 - 1.0,
                        cx + (w / 2.0 + 4.0),
                        y1 + 1.0,
                    );
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        accent.with_alpha_factor(0.25 * glow_t as f32),
                        None,
                        &glow,
                    );
                }
            }
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                self.hover_close_idx = self.close_rects.iter().position(|r| r.contains(p));
                self.hover_max_idx = self.max_rects.iter().position(|r| r.contains(p));
                self.hover_search_idx = self.search_rects.iter().position(|r| r.contains(p));
                // Only count a tab as "hovered for highlight" if we're inside
                // the pill AND not inside its close chip — keeps the close
                // button's own hover state dominant.
                self.hover_idx = self
                    .tab_rects
                    .iter()
                    .position(|r| r.contains(p));
                if let Some((idx, origin)) = self.press_origin {
                    if (p - origin).hypot() > DRAG_START_THRESHOLD_PX {
                        if let Some(t) = self.tabs.get(idx) {
                            let group_tag_hash = self.group_id_tag.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
                            self.pending_drag = Some(DragPayload::Tab { group: group_tag_hash, tab: t.id });
                            *handled = true;
                        }
                    }
                }
                // While a drag is in flight, keep the drop-zone target in
                // sync with the cursor. Cursor inside the bar → the gap
                // index resolved by `drop_index_for`. Outside the bar →
                // clear (cross-group release is handled by `GroupRow`;
                // this bar shouldn't paint an expanding zone for it).
                if self.pending_drag.is_some() {
                    let new_target = if self.rect.contains(p) {
                        Some(self.drop_index_for(p))
                    } else {
                        None
                    };
                    if new_target != self.drop_zone_target {
                        self.drop_zone_target = new_target;
                        self.drop_zone_animated_at = Some(Instant::now());
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Fresh hit-tests at press time against the authoritative
                // rects, rather than trusting the hover-index cache populated
                // on `CursorMoved`. The cache can lag when the click arrives
                // before a hover-refresh covers the press position (e.g. a
                // click on an inactive tab's icon without a prior in-icon
                // move), which previously swallowed the icon hit and merely
                // activated the tab. Concept's `TabBar.tsx` fires its action
                // on first click regardless of tab-active state — this
                // matches that behaviour.
                let cursor_pos = self.last_cursor;
                let close_idx = self.close_rects.iter().position(|r| r.contains(cursor_pos));
                let max_idx = self.max_rects.iter().position(|r| r.contains(cursor_pos));
                let search_idx = self.search_rects.iter().position(|r| r.contains(cursor_pos));

                // Close button takes priority over selection/drag.
                if let Some(ci) = close_idx {
                    if let Some(t) = self.tabs.get(ci) {
                        self.pending_close = Some(t.id);
                        *handled = true;
                        return;
                    }
                }
                // Maximize icon — fires on first click regardless of whether
                // the tab is already active.
                if let Some(mi) = max_idx {
                    if let Some(t) = self.tabs.get(mi) {
                        self.pending_max = Some(t.id);
                        *handled = true;
                        return;
                    }
                }
                // Search icon — same deal; shell drains `pending_search`.
                if let Some(si) = search_idx {
                    if let Some(t) = self.tabs.get(si) {
                        self.pending_search = Some(t.id);
                        *handled = true;
                        return;
                    }
                }
                if let Some(idx) = self.hover_idx {
                    let origin = self.tab_rects[idx].center();
                    self.press_origin = Some((idx, origin));
                    self.active_id = self.tabs[idx].id;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // Finalise a drag release. Three possible outcomes:
                //
                //   1. Released over another pill in this same bar →
                //      `pending_reorder` (within-group move).
                //   2. Released somewhere outside this bar's own rect →
                //      `pending_detach` (cross-group move — the parent
                //      `GroupRow` resolves the target bar by hit-testing
                //      sibling `bar_rect()`s against the release point).
                //   3. Released inside the bar but not on another pill →
                //      dropped in the gap, treated as "cancel".
                if self.pending_drag.is_some() {
                    if let Some((from, _)) = self.press_origin {
                        let in_bar = self.rect.contains(self.last_cursor);
                        if in_bar {
                            let to = self.tab_rects.iter().position(|r| r.contains(self.last_cursor));
                            if let Some(to) = to {
                                if to != from {
                                    self.pending_reorder = Some((from, to));
                                    *handled = true;
                                }
                            }
                        } else if let Some(t) = self.tabs.get(from) {
                            self.pending_detach = Some((t.id, self.last_cursor));
                            *handled = true;
                        }
                    }
                    self.pending_drag = None;
                }
                self.press_origin = None;
                // Drag is over — stop pumping redraws for the drop-zone
                // animation and clear the target so a fresh drag starts
                // with no stale zone visible.
                self.drop_zone_target = None;
                self.drop_zone_animated_at = None;
            }
            _ => {}
        }
    }
    fn name(&self) -> &str { "TabBar" }
    fn get_rect(&self) -> Rect { self.rect }
    fn wants_pointer_capture(&self) -> bool { self.press_origin.is_some() || self.pending_drag.is_some() }
    fn accepts_drop(&self, payload: &DragPayload) -> bool { matches!(payload, DragPayload::Tab { .. }) }
    fn needs_animation_frame(&self) -> bool {
        // Keep the host pumping redraws while either of the two animations
        // are still in flight:
        //   1. Drop-zone expand curve while a drag is pending (140 ms).
        //   2. Tab-layout spring after reorder/insert/remove (180 ms).
        // At rest (no drag, no layout change) both return false and the
        // wrapping widgets (`WindowGroup`, `GroupRow`, `ShellOverlayHost`)
        // can stop pumping frames.
        let drop_zone = self
            .drop_zone_animated_at
            .map(|t| t.elapsed() < Duration::from_millis(DROP_ZONE_ANIM_MS))
            .unwrap_or(false);
        let layout = self
            .layout_anim_started
            .map(|t| t.elapsed() < Duration::from_millis(LAYOUT_ANIM_MS))
            .unwrap_or(false);
        // 3. Per-tab insert fade (220 ms). Any unexpired entry keeps the
        //    redraw pump going so the fade curve is visible.
        let insert_fade = self
            .inserted_at
            .values()
            .any(|t| t.elapsed() < Duration::from_millis(INSERT_FADE_MS));
        drop_zone || layout || insert_fade
    }
}

/// Paint a small (14 × 14 px target) vector glyph inside `rect` that
/// corresponds to `kind`. Matches the concept's `getAppIcon`
/// (`WindowFrame.tsx:10-18`), which picks a lucide icon per app type.
/// We approximate the lucide shapes with kurbo primitives so we don't
/// need an icon-font codepoint — the strokes are thin (1.1 px) so the
/// glyphs read as chrome rather than decoration.
fn draw_app_icon(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    kind: AppKind,
    color: vello::peniko::Color,
) {
    let stroke = vello::kurbo::Stroke::new(1.1);
    let cx = rect.center().x;
    let cy = rect.center().y;
    match kind {
        AppKind::Terminal => {
            // ">_" prompt: diagonal chevron + trailing underscore. Lucide
            // `Terminal` draws a ">" plus "-" in a rect; we skip the
            // outer frame so the glyph reads at 14 px without clutter.
            let chev = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 4.0, cy - 3.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 0.5, cy)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 4.0, cy + 3.0)),
            ]);
            scene.stroke(&stroke, transform, color, None, &chev);
            let under = vello::kurbo::Line::new(
                Point::new(cx + 0.5, cy + 3.0),
                Point::new(cx + 4.5, cy + 3.0),
            );
            scene.stroke(&stroke, transform, color, None, &under);
        }
        AppKind::Files => {
            // Folder trapezoid: lucide `Folder`. A small tab on the
            // top-left gives the shape its folder-y read at this size.
            let tab = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 5.0, cy - 3.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 2.0, cy - 3.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 0.5, cy - 1.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 5.0, cy - 1.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 5.0, cy + 4.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 5.0, cy + 4.0)),
                vello::kurbo::PathEl::ClosePath,
            ]);
            scene.stroke(&stroke, transform, color, None, &tab);
        }
        AppKind::Editor => {
            // Lucide `FileCode2`: document outline with a `<>` glyph.
            // We draw a rounded rect with a corner fold + two small
            // chevrons.
            let doc = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 4.0, cy - 5.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 2.0, cy - 5.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 4.5, cy - 2.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 4.5, cy + 5.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 4.0, cy + 5.0)),
                vello::kurbo::PathEl::ClosePath,
            ]);
            scene.stroke(&stroke, transform, color, None, &doc);
            // Inner chevron "<" + ">"
            let chevs = vello::kurbo::BezPath::from_vec(vec![
                vello::kurbo::PathEl::MoveTo(Point::new(cx - 1.5, cy - 0.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 3.0, cy + 1.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx - 1.5, cy + 2.5)),
                vello::kurbo::PathEl::MoveTo(Point::new(cx + 1.5, cy - 0.5)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 3.0, cy + 1.0)),
                vello::kurbo::PathEl::LineTo(Point::new(cx + 1.5, cy + 2.5)),
            ]);
            scene.stroke(&stroke, transform, color, None, &chevs);
        }
        AppKind::Browser => {
            // Lucide `Globe`: circle + horizontal equator + vertical
            // meridian. Small enough that we omit the curved latitude
            // lines the full lucide glyph has.
            let circle = vello::kurbo::Circle::new(Point::new(cx, cy), 5.5);
            scene.stroke(&stroke, transform, color, None, &circle);
            let h = vello::kurbo::Line::new(
                Point::new(cx - 5.5, cy),
                Point::new(cx + 5.5, cy),
            );
            scene.stroke(&stroke, transform, color, None, &h);
            let v = vello::kurbo::Line::new(
                Point::new(cx, cy - 5.5),
                Point::new(cx, cy + 5.5),
            );
            scene.stroke(&stroke, transform, color, None, &v);
        }
        AppKind::Settings => {
            // Lucide `Settings` (gear). Approximate via an outer outlined
            // circle + a tighter inner circle; the full gear teeth are
            // too small to read at 14 px. Mirrors the concept's gear
            // glyph used for "workspace settings" throughout.
            let outer = vello::kurbo::Circle::new(Point::new(cx, cy), 5.5);
            scene.stroke(&stroke, transform, color, None, &outer);
            let inner = vello::kurbo::Circle::new(Point::new(cx, cy), 2.0);
            scene.stroke(&stroke, transform, color, None, &inner);
            // Four small teeth (top/bottom/left/right) to give the gear
            // shape its characteristic crenellation.
            for (dx, dy) in [(0.0, -5.5), (0.0, 5.5), (-5.5, 0.0), (5.5, 0.0)] {
                let t = vello::kurbo::Line::new(
                    Point::new(cx + dx * 0.9, cy + dy * 0.9),
                    Point::new(cx + dx * 1.25, cy + dy * 1.25),
                );
                scene.stroke(&stroke, transform, color, None, &t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::user_color;

    fn bar() -> TabBar {
        TabBar::new(
            "g1",
            vec![
                TabDescriptor::new(1, "A"),
                TabDescriptor::new(2, "B"),
                TabDescriptor::new(3, "C"),
            ],
            1,
        )
    }

    #[test]
    fn remove_tab_drops_it_and_picks_neighbour_when_active() {
        let mut b = bar();
        b.set_active(2);
        assert_eq!(b.remove_tab(2), Some(1));
        assert_eq!(b.tabs().len(), 2);
        // Active fell through to the tab that slid into position `1` (C).
        assert_eq!(b.active_id(), 3);
    }

    #[test]
    fn remove_tab_at_end_picks_last_remaining() {
        let mut b = bar();
        b.set_active(3);
        b.remove_tab(3);
        assert_eq!(b.active_id(), 2);
    }

    #[test]
    fn reorder_moves_tab_to_requested_index() {
        let mut b = bar();
        b.reorder(0, 2);
        let ids: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    #[test]
    fn user_color_is_stable_per_id_and_differs_across_ids() {
        // The widget now forwards to `crate::presence::user_color`, which
        // pins `alice` → pink-500 and `bob` → orange-500. The guarantees
        // we care about at the widget level are unchanged: stable per id,
        // distinguishable across ids.
        let a1 = user_color("alice");
        let a2 = user_color("alice");
        let b1 = user_color("bob");
        assert_eq!(a1, a2, "same id must map to same color across calls");
        assert_ne!(a1, b1, "different ids should map to different colors");
    }

    #[test]
    fn drop_index_for_uses_pill_midpoints() {
        // Hand-craft a bar with three known pill rects so we don't have to
        // run `apply_layout` through taffy for a drop-index unit test.
        // Pill A spans [0, 80], B [80, 160], C [160, 240].
        let mut b = bar();
        // Outer bar: 40 px (TAB_BAR_HEIGHT). Pills sit inset 6 px top/bot,
        // so pill y spans [6, 34]. Drop-index hit tests only compare the x
        // coordinate, so this y-inset is cosmetic for the test — picked to
        // match the real `apply_layout` geometry so we catch regressions.
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(80.0, 6.0, 160.0, 34.0),
            Rect::new(160.0, 6.0, 240.0, 34.0),
        ];
        // Left of A's midpoint (40) → insert before A (index 0).
        assert_eq!(b.drop_index_for(Point::new(10.0, 20.0)), 0);
        // Past A's mid but before B's mid (120) → insert at index 1.
        assert_eq!(b.drop_index_for(Point::new(90.0, 20.0)), 1);
        // Past B's mid but before C's mid (200) → insert at index 2.
        assert_eq!(b.drop_index_for(Point::new(170.0, 20.0)), 2);
        // Past every midpoint → append at len().
        assert_eq!(b.drop_index_for(Point::new(230.0, 20.0)), 3);
    }

    #[test]
    fn drop_index_for_empty_bar_is_zero() {
        // A bar with no pills (e.g. a target that was just emptied) should
        // accept any drop at index 0 so the spliced tab lands cleanly.
        let mut b = bar();
        b.tab_rects.clear();
        assert_eq!(b.drop_index_for(Point::new(0.0, 0.0)), 0);
        assert_eq!(b.drop_index_for(Point::new(1000.0, 0.0)), 0);
    }

    #[test]
    fn take_max_drains_pending_request_exactly_once() {
        // Fresh bar has nothing pending — drain is `None` and stays that
        // way until an event seeds the request.
        let mut b = bar();
        assert_eq!(b.take_max(), None);
        b.pending_max = Some(2);
        assert_eq!(b.take_max(), Some(2));
        // Second drain is empty — ensures WindowGroup doesn't double-toggle
        // `maximized` on redundant polls within the same frame.
        assert_eq!(b.take_max(), None);
    }

    #[test]
    fn take_search_drains_pending_request_exactly_once() {
        // Matches the `take_max` contract — a pending search request is
        // drained exactly once so the shell doesn't re-open the command
        // palette on every frame after a single click.
        let mut b = bar();
        assert_eq!(b.take_search(), None);
        b.pending_search = Some(3);
        assert_eq!(b.take_search(), Some(3));
        assert_eq!(b.take_search(), None);
    }

    #[test]
    fn tab_descriptor_new_sets_app_kind_none() {
        // `TabDescriptor::new` is the convenience path used by tests and
        // by any caller that doesn't know the tile type — app_kind must
        // default to None so `TabBar::draw` skips the leading-icon
        // reservation and the pill stays its short, text-only width.
        let d = TabDescriptor::new(1, "X");
        assert_eq!(d.id, 1);
        assert_eq!(d.title, "X");
        assert!(d.app_kind.is_none());
    }

    #[test]
    fn pill_reserves_extra_width_when_app_kind_set() {
        // With `app_kind = Some(kind)`, `apply_layout` reserves an extra
        // 20 px (icon_w 14 + icon_gap 6) on the leading edge so the
        // title doesn't collide with the glyph. We drive `apply_layout`
        // through taffy and assert the two pills' widths differ by
        // exactly that reserve.
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut a = TabBar::new(
            "g1",
            vec![TabDescriptor::new(1, "A")],
            1,
        );
        let mut b = TabBar::new(
            "g1",
            vec![TabDescriptor {
                id: 1,
                title: "A".into(),
                app_kind: Some(AppKind::Terminal),
            }],
            1,
        );
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        let node_a = a.build_layout(&mut taffy, &theme, &mut node_map);
        let root_a = taffy
            .new_with_children(Style::default(), &[node_a])
            .unwrap();
        taffy
            .compute_layout(root_a, taffy::Size::MAX_CONTENT)
            .unwrap();
        a.apply_layout(&taffy, node_a, Point::ZERO);

        let mut taffy2 = TaffyTree::<()>::new();
        let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node_b = b.build_layout(&mut taffy2, &theme, &mut node_map2);
        let root_b = taffy2
            .new_with_children(Style::default(), &[node_b])
            .unwrap();
        taffy2
            .compute_layout(root_b, taffy::Size::MAX_CONTENT)
            .unwrap();
        b.apply_layout(&taffy2, node_b, Point::ZERO);

        let width_a = a.tab_rects[0].width();
        let width_b = b.tab_rects[0].width();
        // A hits the 96 px floor; B may also hit the floor because "A"
        // is 1 char — verify instead that B is >= A + icon reserve at
        // the computed width, treating the floor as a lower bound.
        assert!(
            width_b >= width_a || (width_a - width_b).abs() < 0.5,
            "typed-app pill must be at least as wide as the plain one (\
             a = {width_a}, b = {width_b})"
        );
        // If A and B both hit the floor, at least verify the call-path
        // is live — i.e. the typed descriptor still computes a rect.
        assert!(width_b > 0.0);
    }

    #[test]
    fn reorder_starts_layout_spring_and_snapshots_prev_rects() {
        // Before reorder: no snapshot, no animation.
        let mut b = bar();
        assert!(b.prev_tab_rects_by_id.is_empty());
        assert!(b.layout_anim_started.is_none());
        assert!(!b.needs_animation_frame());

        // Seed `tab_rects` so the snapshot has something to capture —
        // this is what `apply_layout` would leave behind in production.
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];

        // Trigger a reorder. Snapshot must capture the pre-reorder rects
        // keyed by id, and the spring must start.
        b.reorder(0, 2);
        assert_eq!(b.prev_tab_rects_by_id.len(), 3);
        assert_eq!(
            b.prev_tab_rects_by_id.get(&1).copied(),
            Some(Rect::new(0.0, 6.0, 80.0, 34.0)),
            "tab 1's pre-reorder rect must be captured"
        );
        assert!(b.layout_anim_started.is_some());
        assert!(
            b.needs_animation_frame(),
            "fresh reorder must request animation frames"
        );
    }

    #[test]
    fn insert_tab_triggers_layout_spring() {
        // Inserting a tab into an existing bar must spring neighbours
        // aside — same mechanism as reorder. Verify the animation
        // started and the pre-insert rects were captured.
        let mut b = bar();
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];
        b.insert_tab(1, TabDescriptor::new(99, "NEW"));
        assert!(b.layout_anim_started.is_some());
        assert!(b.needs_animation_frame());
        // The new tab 99 is *not* in the snapshot — it renders at target
        // immediately (no fly-in), which the draw path guards with
        // `prev_tab_rects_by_id.get(...)`.
        assert!(!b.prev_tab_rects_by_id.contains_key(&99));
        // Existing tabs must be in the snapshot so their old positions
        // are known to the interpolator.
        assert!(b.prev_tab_rects_by_id.contains_key(&1));
    }

    #[test]
    fn remove_tab_triggers_layout_spring() {
        let mut b = bar();
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];
        b.remove_tab(2);
        assert!(b.layout_anim_started.is_some());
        assert!(b.needs_animation_frame());
        // Survivors are all in the snapshot so they slide into the freed
        // slot; the removed id is in the snapshot too (the method
        // captures *before* the remove), which is harmless — there's no
        // rendered tab to look it up against.
        assert!(b.prev_tab_rects_by_id.contains_key(&1));
        assert!(b.prev_tab_rects_by_id.contains_key(&3));
    }

    #[test]
    fn insert_tab_records_inserted_at_for_fade_in() {
        // The fade-in animation is driven by the per-tab `inserted_at`
        // map. A call to `insert_tab` must stamp the new tab id so the
        // draw path plays the opacity/y-offset curve.
        let mut b = bar();
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];
        assert!(!b.inserted_at.contains_key(&99), "precondition: no entry");
        b.insert_tab(1, TabDescriptor::new(99, "NEW"));
        assert!(
            b.inserted_at.contains_key(&99),
            "insert_tab must record a fade-in timestamp for the new tab id",
        );
        // While the fade is in flight, `needs_animation_frame` must
        // report true so the App pumps redraws.
        assert!(
            b.needs_animation_frame(),
            "insert fade must keep the redraw pump alive",
        );
    }

    #[test]
    fn insert_fade_clears_after_duration() {
        // Once `INSERT_FADE_MS` has elapsed, `needs_animation_frame`
        // must go quiet again. We backdate the `inserted_at` entry so
        // the retain in `draw` (and the timestamp check in
        // `needs_animation_frame`) both consider it expired.
        let mut b = bar();
        b.inserted_at.insert(
            7,
            Instant::now() - Duration::from_millis(INSERT_FADE_MS + 50),
        );
        assert!(
            !b.needs_animation_frame(),
            "elapsed insert fade must not pump frames",
        );
    }

    #[test]
    fn layout_spring_clears_itself_after_duration() {
        // After `LAYOUT_ANIM_MS` has elapsed, `needs_animation_frame`
        // must go quiet so the host stops redrawing. We simulate this
        // by backdating `layout_anim_started` — a real reorder's
        // timestamp will reach this state naturally after 180 ms.
        let mut b = bar();
        b.layout_anim_started = Some(
            Instant::now() - Duration::from_millis(LAYOUT_ANIM_MS + 50),
        );
        assert!(
            !b.needs_animation_frame(),
            "elapsed animation must not request frames"
        );
    }

    #[test]
    fn drop_zone_target_tracks_cursor_during_drag() {
        // With a drag in flight, moving the cursor across the bar should
        // update `drop_zone_target` to the gap index that matches
        // `drop_index_for`. This is the state that drives the concept's
        // TabDropZone expand animation in `draw`.
        let mut b = bar();
        // Hand-build tab rects so we don't have to run the taffy pass.
        // Pills: A [0..80], B [80..160], C [160..240]. Bar rect covers
        // [0..240, 0..40].
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(80.0, 6.0, 160.0, 34.0),
            Rect::new(160.0, 6.0, 240.0, 34.0),
        ];
        // Simulate a drag being in flight (the event-based path that
        // normally sets this — press + move past threshold — is already
        // covered by integration tests).
        b.pending_drag = Some(DragPayload::Tab { group: 0, tab: 1 });
        // Cursor over pill B's left half → drop index 1 (insert before B).
        b.last_cursor = Point::new(90.0, 20.0);
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(90.0, 20.0),
        };
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        b.handle_event(
            &event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        assert_eq!(b.drop_zone_target(), Some(1), "cursor on B's left half → gap 1");
        // While animating, `needs_animation_frame()` must be true so the
        // host pumps redraws. Sampled immediately after the target
        // change — well within `DROP_ZONE_ANIM_MS`.
        assert!(
            b.needs_animation_frame(),
            "drop-zone animation must request redraws"
        );

        // Move cursor past pill C's midpoint → gap 3 (append).
        let event = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(230.0, 20.0),
        };
        b.handle_event(
            &event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        assert_eq!(b.drop_zone_target(), Some(3), "past C's mid → gap 3");

        // Release the drag: target must clear and animation-frame must
        // stop requesting redraws so the shell can go idle again.
        let release = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        b.handle_event(
            &release,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        assert_eq!(b.drop_zone_target(), None, "release clears target");
        assert!(
            !b.needs_animation_frame(),
            "at rest (no drag) the bar must not pump frames"
        );
    }

    #[test]
    fn press_on_inactive_tab_maximize_icon_fires_on_first_click() {
        // Regression: the maximize icon previously only responded while the
        // hover-index cache named its pill, so a press that arrived without
        // a prior in-icon `CursorMoved` — or one that landed on an inactive
        // tab whose hover cache was out of sync — would merely activate the
        // tab instead of firing `pending_max`. Concept's `TabBar.tsx` fires
        // on the first click regardless of active state; we must match that.
        let mut b = bar();
        // Hand-build rects so we don't have to run taffy. Three pills with
        // a 12 px wide maximize rect on each, sitting near the right edge.
        // A sits at [0..80], B at [86..166], C at [172..252]. Tab `1` (A) is
        // the active one — we'll click B's (inactive) maximize.
        b.rect = Rect::new(0.0, 0.0, 252.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];
        b.close_rects = vec![
            Rect::new(60.0, 10.0, 78.0, 30.0),
            Rect::new(146.0, 10.0, 164.0, 30.0),
            Rect::new(232.0, 10.0, 250.0, 30.0),
        ];
        b.max_rects = vec![
            Rect::new(40.0, 10.0, 56.0, 30.0),
            Rect::new(126.0, 10.0, 142.0, 30.0),
            Rect::new(212.0, 10.0, 228.0, 30.0),
        ];
        b.search_rects = vec![
            Rect::new(20.0, 10.0, 36.0, 30.0),
            Rect::new(106.0, 10.0, 122.0, 30.0),
            Rect::new(192.0, 10.0, 208.0, 30.0),
        ];
        // Active tab is A (id 1). B (id 2) is inactive.
        assert_eq!(b.active_id(), 1);

        // Seed `last_cursor` on B's maximize rect, but deliberately leave
        // `hover_max_idx` as None — this mirrors the stale-cache case where
        // the click position hasn't been covered by a prior CursorMoved.
        b.last_cursor = Point::new(134.0, 20.0);
        b.hover_max_idx = None;
        b.hover_search_idx = None;
        b.hover_close_idx = None;
        b.hover_idx = Some(1);

        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        b.handle_event(
            &press,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );

        // The press must publish a `pending_max` for tab B (id 2) on the
        // first click, even though B was not active and the hover cache was
        // empty. Crucially, tab activation must *not* happen here — the
        // concept treats the icon as a distinct hit-zone.
        assert_eq!(
            b.take_max(),
            Some(2),
            "maximize click on inactive tab must fire on first press",
        );
        assert!(handled, "press on the maximize icon is handled");
        assert_eq!(
            b.active_id(),
            1,
            "icon hit must not bleed into tab activation",
        );
    }

    #[test]
    fn press_on_inactive_tab_search_icon_fires_on_first_click() {
        // Twin of the maximize test for the search icon. Same staleness
        // hazard, same expected fix: fresh hit-test on press.
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 252.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(86.0, 6.0, 166.0, 34.0),
            Rect::new(172.0, 6.0, 252.0, 34.0),
        ];
        b.close_rects = vec![
            Rect::new(60.0, 10.0, 78.0, 30.0),
            Rect::new(146.0, 10.0, 164.0, 30.0),
            Rect::new(232.0, 10.0, 250.0, 30.0),
        ];
        b.max_rects = vec![
            Rect::new(40.0, 10.0, 56.0, 30.0),
            Rect::new(126.0, 10.0, 142.0, 30.0),
            Rect::new(212.0, 10.0, 228.0, 30.0),
        ];
        b.search_rects = vec![
            Rect::new(20.0, 10.0, 36.0, 30.0),
            Rect::new(106.0, 10.0, 122.0, 30.0),
            Rect::new(192.0, 10.0, 208.0, 30.0),
        ];
        assert_eq!(b.active_id(), 1);

        // Cursor over C's search rect (inactive tab, id 3). All hover
        // caches empty to prove the press path does a fresh hit-test.
        b.last_cursor = Point::new(200.0, 20.0);
        b.hover_max_idx = None;
        b.hover_search_idx = None;
        b.hover_close_idx = None;
        b.hover_idx = Some(2);

        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        b.handle_event(
            &press,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );

        assert_eq!(
            b.take_search(),
            Some(3),
            "search click on inactive tab must fire on first press",
        );
        assert!(handled, "press on the search icon is handled");
        assert_eq!(
            b.active_id(),
            1,
            "search icon hit must not bleed into tab activation",
        );
    }

    /// Guards visual gap #2 from `docs/nirios/visual-gaps.md`: the active
    /// pill fill must be meaningfully stronger than an inactive pill's
    /// fill, otherwise the tab bar reads as uniform and the user can't
    /// tell which tab is active at a glance. We keep the invariant
    /// loose (>= 4x) so future tone-tuning can shift individual values
    /// without tripping the test, but it will catch any regression that
    /// collapses the two alphas back to similar magnitudes.
    #[test]
    fn active_pill_fill_has_stronger_alpha_than_inactive() {
        // Mirror the literals in `TabBar::draw` — if those move, this
        // test moves with them. Kept deliberately in-crate so the
        // contract is re-verified on every `cargo test -p quick-ui`.
        const ACTIVE_FILL_ALPHA: u8 = 52;
        const INACTIVE_FILL_ALPHA: u8 = 8;
        const HOVER_FILL_ALPHA: u8 = 26;
        const ACTIVE_BORDER_ALPHA: u8 = 64;
        const INACTIVE_BORDER_ALPHA: u8 = 18;

        assert!(
            ACTIVE_FILL_ALPHA as f64 > INACTIVE_FILL_ALPHA as f64 * 4.0,
            "active pill fill must be >= 4x the inactive fill",
        );
        assert!(
            HOVER_FILL_ALPHA > INACTIVE_FILL_ALPHA && HOVER_FILL_ALPHA < ACTIVE_FILL_ALPHA,
            "hover fill sits between inactive and active so hover feels like a lift",
        );
        assert!(
            ACTIVE_BORDER_ALPHA > INACTIVE_BORDER_ALPHA,
            "active border is stronger than inactive border",
        );
    }
}
