use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use taffy::style_helpers::length;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use crate::icons;
use crate::widget::{Widget, TabBar, TabDescriptor, TileContent, TileHost};
use crate::widget::tabs::CLOSE_FADE_MS;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

pub const WIDTH_MIN_PX: u32 = 300;
pub const WIDTH_MAX_PX: u32 = 1200;
pub const RESIZE_HANDLE_PX: f64 = 6.0;

/// Duration of the per-group fade-in played when a `WindowGroup` is
/// newly spawned — e.g. via split-into-new-group from a cross-group
/// drag release. Mirrors the radial-workspace concept's
/// `transition={{ delay: 0.2, duration: 0.5, ease: [0.22, 1, 0.36, 1] }}`
/// (`radial-workspace-switcher/src/App.tsx:407`). 420 ms is long enough
/// for the eased slide-in to read clearly to the user without delaying
/// the next interaction on the new group — earlier 320 ms tunings made
/// the spawn read as "instant" in shots-driver captures.
const SPAWN_ANIM_MS: u64 = 420;
/// Horizontal start offset for the spawn fade, in CSS-like px. Negative
/// values mean the group starts to the left of its target and settles
/// rightward — mirrors the concept's `x: -20` initial. Tuned to -16 so
/// the slide reads as "arriving" without escaping the row's visible
/// margin in shots-driver captures.
const SPAWN_ANIM_X_OFFSET: f64 = -16.0;
/// Initial scale for the spawn fade. The group ramps from this to 1.0
/// over `SPAWN_ANIM_MS`. 0.92 is punchy enough for the user to register
/// the arrival without making the new group feel like it's leaping in.
const SPAWN_ANIM_INITIAL_SCALE: f64 = 0.92;
/// Duration of the group-level close fade-out played when the last
/// tab is closed out of a `WindowGroup`. Matches the tab-pill
/// `CLOSE_FADE_MS` in `tabs.rs` so the group's own shrink chains
/// cleanly onto the final pill fade without a visible gap or double-
/// bounce. `GroupRow` prunes the group from its `groups` vec once
/// `close_complete()` returns true.
const GROUP_CLOSE_FADE_MS: u64 = 180;

/// Vertical reveal band (px) for the maximized floating chrome. Matches
/// the concept's `WindowFrame.tsx:108-123` `useEffect` that watches
/// `clientY <= 80` and toggles `chromeVisible`. While the cursor sits
/// at or above this y, the floating title pill + restore + close
/// cluster paint; once the cursor moves below, the chrome hides so the
/// maximized content fills the tile uninterrupted. Hit-testing math is
/// untouched — only the rendering is gated, so a click that lands on
/// the chrome rects while the cursor is still in the reveal band still
/// resolves through the existing handle_event arms.
const MAXIMIZED_CHROME_REVEAL_Y: f64 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowWidth { Narrow, Normal, Wide, Custom(u32) }

impl WindowWidth {
    pub fn pixels(&self) -> u32 {
        match self {
            WindowWidth::Narrow => 400,
            WindowWidth::Normal => 600,
            WindowWidth::Wide => 800,
            WindowWidth::Custom(p) => (*p).clamp(WIDTH_MIN_PX, WIDTH_MAX_PX),
        }
    }
}

pub struct WindowGroup {
    pub id: u64,
    bar: TabBar,
    hosts: Vec<(u64, TileHost)>,
    width: WindowWidth,
    rect: Rect,
    resize_origin: Option<(f32, u32)>,
    pub hover_resize: bool,
    /// User has requested the concept's "maximized" mode for this group.
    /// The shell paints the floating controls on top of this rect while
    /// set; click the maximize icon again to un-maximize (restore).
    pub maximized: bool,
    /// Concept parity for `WindowFrame.isSearching`: the user clicked the
    /// search glyph on one of this group's tabs. We paint a thicker indigo
    /// ring + outer glow around the card while set, matching the concept's
    /// `ring-4 ring-indigo-500/50 shadow-[0_0_60px_rgba(99,102,241,0.6)]`.
    /// Toggled off by clicking the same icon again or pressing Esc while
    /// the group is focused.
    pub search_active: bool,
    /// Hit region for the floating "restore" pill drawn while maximized.
    /// Populated each draw so clicks always map to what's on screen.
    restore_rect: Rect,
    restore_hover: bool,
    /// Hit region for the floating "close active tab" glyph drawn while
    /// maximized, sitting just right of the restore pill to mirror the
    /// concept's `[Search] [Minimize2] [X]` cluster.
    close_rect: Rect,
    close_hover: bool,
    last_cursor: Point,
    /// `Instant` at which this group's spawn-fade was latched, or
    /// `None` if no spawn animation is in-flight (the common case —
    /// groups built at `build_nirios_root` time don't animate). Set
    /// via [`WindowGroup::start_spawn_animation`] from code paths that
    /// create a brand-new group (e.g. split-tab-into-new-group in
    /// [`crate::widget::GroupRow::split_tab_into_new_group`]).
    spawned_at: Option<Instant>,
    /// Hosts whose matching tab pill is in the middle of the
    /// concept-parity close fade. Keyed by `(tab_id, Instant)` where
    /// the `Instant` is the close-start timestamp — the same stamp
    /// the bar snapshots into its `closing_tabs` so the two
    /// animations stay in lockstep. While an entry is present the
    /// `TileHost` is kept in `hosts` and still occupies its index;
    /// once `elapsed > CLOSE_FADE_MS` a sweep at the top of
    /// `handle_event` drops the entry, removes the host from
    /// `hosts`, and calls `bar.finalize_close(id)` so the pill
    /// stops rendering. Side-table rather than a field on `TileHost`
    /// to keep the `TileHost` shape stable and avoid a touch to
    /// `tile_host.rs`.
    closing_hosts: HashMap<u64, Instant>,
    /// Set when `maximized` flips, so `take_structure_dirty` requests a
    /// `build_layout` rebuild and the new flex_grow / width style takes
    /// effect on the cached taffy node.
    style_dirty: bool,
    /// Group-level close-fade timestamp. `None` while the group is
    /// alive; `Some(started)` once the user's last tab-close drains
    /// through `handle_event` and the group has no remaining tabs or
    /// in-flight closing hosts. Mirrors the per-tab `closing_tabs`
    /// pattern on the bar: we keep the group rendering at declining
    /// alpha + scale for `GROUP_CLOSE_FADE_MS`, then the parent
    /// `GroupRow` sweep prunes us from its `groups` vec once
    /// `close_complete()` returns true. Kept `Option<Instant>` (not
    /// a bool + separate timestamp) so `is_closing` and
    /// `close_elapsed` have a single source of truth.
    closing_at: Option<Instant>,
}

impl WindowGroup {
    pub fn new(id: u64, tabs_with_content: Vec<(TabDescriptor, TileContent)>, active_id: u64, width: WindowWidth) -> Self {
        let descs: Vec<TabDescriptor> = tabs_with_content.iter().map(|(d, _)| d.clone()).collect();
        let hosts: Vec<(u64, TileHost)> = tabs_with_content.into_iter().map(|(d, c)| (d.id, TileHost::new(c))).collect();
        Self {
            id,
            bar: TabBar::new(&format!("g{id}"), descs, active_id),
            hosts,
            width,
            rect: Rect::ZERO,
            resize_origin: None,
            hover_resize: false,
            maximized: false,
            search_active: false,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            closing_hosts: HashMap::new(),
            closing_at: None,
            style_dirty: false,
        }
    }
    pub fn width(&self) -> WindowWidth { self.width }

    /// Latch the concept-faithful spawn fade-in: opacity 0→1, scale
    /// 0.92→1, and a -16 px → 0 horizontal settle over `SPAWN_ANIM_MS`,
    /// eased with a cubic-out curve. Callers that create a brand-new
    /// `WindowGroup` — e.g. the split-into-new-group path in `GroupRow`
    /// — invoke this immediately after construction so the group reads
    /// as "arriving" rather than popping in opaque. A subsequent call
    /// within the animation window restarts it, which is acceptable
    /// (the group re-acquired focus; replay is the least surprising
    /// behaviour).
    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(Instant::now());
    }

    /// Eased 0..=1 progress through the spawn fade-in, or 1.0 when no
    /// spawn animation is in flight. Exposed so headless tests can
    /// assert on the curve's shape (initial near zero, midway near
    /// half, terminal at one) without poking the draw path. The value
    /// is the same `eased` factor `draw` uses to compute alpha, scale
    /// and the x-offset, so it's the single source of truth for "how
    /// far through the spawn are we".
    pub fn spawn_anim_t(&self) -> f32 {
        let Some(started) = self.spawned_at else { return 1.0 };
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_millis(SPAWN_ANIM_MS) {
            return 1.0;
        }
        let raw = (elapsed.as_secs_f64() / (SPAWN_ANIM_MS as f64 / 1000.0))
            .clamp(0.0, 1.0);
        (1.0 - (1.0 - raw).powi(3)) as f32
    }

    /// Latch the group-level close fade. Called from `handle_event`
    /// the first frame after the bar + `closing_hosts` have been
    /// fully drained with no surviving tabs. Mirrors
    /// `TabBar::start_closing` (tab-level) and
    /// `TileHost::start_spawn_animation` (tile-level) so each layer
    /// in the group composition has a symmetric open/close
    /// animation.
    ///
    /// Idempotent in spirit: a second call before
    /// `close_complete()` returns true overwrites the timestamp,
    /// which is acceptable (the stimulus is strictly "last tab
    /// went away"; if we somehow observed it twice we want the
    /// newer clock).
    pub fn start_closing(&mut self) {
        self.closing_at = Some(Instant::now());
    }

    /// True while the group-level close fade is in flight or latched
    /// but not yet complete. Used by `GroupRow` to branch between
    /// "keep painting" and "prune".
    pub fn is_closing(&self) -> bool {
        self.closing_at.is_some()
    }

    /// How long the close fade has been running, or `None` if the
    /// group is still alive. Exposed for parity tests that want to
    /// assert on the exact progress of the curve without reaching
    /// into private state.
    pub fn close_elapsed(&self) -> Option<Duration> {
        self.closing_at.map(|t| t.elapsed())
    }

    /// True once the group-level close fade has fully elapsed, i.e.
    /// the group has finished shrinking out and `GroupRow` should
    /// drop it from its `groups` vec. False while the fade is still
    /// in flight — the group keeps painting until this flips.
    pub fn close_complete(&self) -> bool {
        self.closing_at
            .is_some_and(|t| t.elapsed().as_millis() as u64 >= GROUP_CLOSE_FADE_MS)
    }
    /// Concept-parity gate for the maximized floating chrome (title
    /// pill + Restore + close cluster). The chrome paints only while
    /// the group is maximized AND the most recent cursor y sits at or
    /// above [`MAXIMIZED_CHROME_REVEAL_Y`]. Mirrors
    /// `WindowFrame.tsx:108-123` where the React effect toggles a
    /// `chromeVisible` boolean off `clientY <= 80`. Exposed as a
    /// public predicate so the parity tests can pin both branches
    /// without poking the draw path.
    ///
    /// Hit-testing is unaffected: `handle_event` keeps populating
    /// `restore_rect`/`close_rect` and resolving clicks through them,
    /// so a press that lands in the band while chrome is visible still
    /// works. Only the rendering is gated.
    pub fn should_show_maximized_chrome(&self, cursor_y: Option<f64>) -> bool {
        self.maximized && cursor_y.is_some_and(|y| y <= MAXIMIZED_CHROME_REVEAL_Y)
    }

    pub fn begin_resize(&mut self, pointer_x: f32) { self.resize_origin = Some((pointer_x, self.width.pixels())); }
    pub fn update_resize(&mut self, pointer_x: f32) {
        if let Some((origin_x, origin_w)) = self.resize_origin {
            let delta = pointer_x - origin_x;
            let new_w = ((origin_w as f32) + delta).round().max(0.0) as i64;
            let clamped = (new_w.max(WIDTH_MIN_PX as i64).min(WIDTH_MAX_PX as i64)) as u32;
            self.width = WindowWidth::Custom(clamped);
        }
    }
    pub fn end_resize(&mut self) { self.resize_origin = None; }
    pub fn active_host_mut(&mut self) -> Option<&mut TileHost> {
        let id = self.bar.active_id();
        self.hosts.iter_mut().find(|(i, _)| *i == id).map(|(_, h)| h)
    }

    // ------------------------------------------------------------------
    // Cross-group drag-drop routing (parent: `GroupRow`)
    // ------------------------------------------------------------------

    /// Drain a cross-group detach request from the underlying `TabBar`.
    /// Returns `(tab_id, release_point)` if the user released a drag
    /// outside the bar's own rect, else `None`. The parent widget hits-
    /// tests the release point against sibling bars to find a target.
    pub fn take_tab_detach(&mut self) -> Option<(u64, Point)> { self.bar.take_detach() }

    /// Remove a tab + its hosted `TileHost` from this group and return
    /// them so a sibling group can insert them. Returns `None` if no tab
    /// with that id is present. Preserves caller-side ordering by
    /// handing back the `TabDescriptor` and `TileHost` as a pair.
    pub fn detach_tab(&mut self, tab_id: u64) -> Option<(TabDescriptor, TileHost)> {
        let host_idx = self.hosts.iter().position(|(i, _)| *i == tab_id)?;
        let (_, host) = self.hosts.remove(host_idx);
        // TabBar::remove_tab snips the descriptor list and reassigns the
        // active id to the next tab in the list.
        let desc_idx = self
            .bar
            .tabs()
            .iter()
            .position(|t| t.id == tab_id)?;
        let desc = self.bar.tabs()[desc_idx].clone();
        self.bar.remove_tab(tab_id);
        // If the tab was mid-close when it was detached, its entry in
        // `closing_hosts` (and the bar's matching `closing_tabs` fade
        // entry) would otherwise outlive the host itself. The host is
        // gone after the `hosts.remove` above, so the fade has nothing
        // to paint — but `needs_animation_frame` keeps pumping redraws
        // until the timestamp ages past `CLOSE_FADE_MS`. Drop both
        // sides together so the run loop quiets immediately.
        self.closing_hosts.remove(&tab_id);
        self.bar.finalize_close(tab_id);
        Some((desc, host))
    }

    /// Insert a previously-detached tab at `idx`, becoming the new active
    /// tab. The content order is kept aligned with the bar's descriptor
    /// order so a subsequent reorder inside this group stays coherent.
    ///
    /// Fires the per-tile spawn fade on the incoming `TileHost` so a
    /// tab dragged into an existing group arrives with the concept's
    /// ease-out opacity+scale motion rather than popping in opaque.
    /// The sibling path — splitting a dragged tab into a *brand-new*
    /// group — lives in `GroupRow::split_tab_into_new_group` and plays
    /// the group-level fade instead, so we never double up.
    pub fn insert_detached_tab(&mut self, idx: usize, desc: TabDescriptor, mut host: TileHost) {
        let id = desc.id;
        self.bar.insert_tab(idx, desc);
        let clamped = idx.min(self.hosts.len());
        host.start_spawn_animation();
        self.hosts.insert(clamped, (id, host));
    }

    /// Construct a new `WindowGroup` wrapping a single pre-built
    /// `(TabDescriptor, TileHost)` pair — the output of `detach_tab`.
    ///
    /// Used by `GroupRow` when the user drops a dragged tab into the
    /// empty space between groups (or past the last group), creating a
    /// fresh group at that position with the dragged tab as its sole
    /// child. The equivalent of the concept's `onMoveTab(src, 'new',
    /// tab, idx)` path in `DesktopView.tsx:414`.
    pub fn from_detached(id: u64, desc: TabDescriptor, host: TileHost, width: WindowWidth) -> Self {
        let active_id = desc.id;
        let tab_id = desc.id;
        Self {
            id,
            bar: TabBar::new(&format!("g{id}"), vec![desc], active_id),
            hosts: vec![(tab_id, host)],
            width,
            rect: Rect::ZERO,
            resize_origin: None,
            hover_resize: false,
            maximized: false,
            search_active: false,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            closing_hosts: HashMap::new(),
            closing_at: None,
            style_dirty: false,
        }
    }

    /// Number of tabs hosted by this group. Used by `GroupRow` to decide
    /// whether a detach leaves the source group empty (and thus whether
    /// to prune it after a new-group drop).
    pub fn tab_count(&self) -> usize { self.hosts.len() }

    /// Return this group's tab-bar rect — i.e. the strip above the tile
    /// content where sibling drops should resolve. The parent `GroupRow`
    /// uses it to hit-test cross-group releases.
    pub fn bar_rect(&self) -> Rect { self.bar.bar_rect() }

    /// Per-pill rects inside the bar, in tab order. Mirrors
    /// `TabBar::tab_rects` so headless tests can read the live layout
    /// instead of guessing pill x-coords from theme constants.
    pub fn tab_rects(&self) -> &[Rect] { self.bar.tab_rects() }

    /// Delegate to the bar's per-pill drop-index resolver. Used by
    /// `GroupRow` on cross-group releases to figure out which index the
    /// dropped tab should be spliced at, based on where in the target
    /// bar the pointer landed.
    pub fn drop_index_for(&self, release_point: Point) -> usize {
        self.bar.drop_index_for(release_point)
    }

    /// Borrow the current tab descriptor list. Exposed so callers (e.g.
    /// the cross-group drag tests in `GroupRow`) can assert on the tab
    /// sequence without reaching through the `TabBar` itself.
    pub fn tabs(&self) -> &[TabDescriptor] { self.bar.tabs() }

    /// The id of the tab currently treated as active by the bar. Exposed
    /// for cross-group drag tests that need to assert on activation flow
    /// (e.g. detaching the active tab must promote a sibling to active).
    pub fn active_tab_id(&self) -> u64 { self.bar.active_id() }

    /// True while this group's `TabBar` has a drag in flight. Exposed so
    /// the parent `GroupRow` can paint inter-group drop-zone indicators
    /// while any child is dragging.
    pub fn drag_in_flight(&self) -> bool { self.bar.drag_in_flight() }

    /// Test-only mutable accessor for the underlying `TabBar`. Used by
    /// `GroupRow` tests that need to publish a synthetic `pending_detach`
    /// to exercise the post-dispatch detach sweep without staging a full
    /// press/drag/release event sequence.
    #[cfg(test)]
    pub(crate) fn bar_mut(&mut self) -> &mut crate::widget::TabBar { &mut self.bar }

    /// Test-only accessor pinning the lucide glyph drawn over the
    /// maximized floating close chip. Paired with
    /// `window_group_close_glyph_is_lucide_x` so a revert to the text
    /// `×` codepoint can't slip in unnoticed.
    #[cfg(test)]
    pub fn close_glyph_icon_for_tests() -> icons::Icon { icons::Icon::X }

    /// Test-only accessor for the glyph prepended to the Restore pill.
    /// Lucide's `Minimize2` — the "shrink out of fullscreen" bracket
    /// pair — paired with `window_group_restore_pill_uses_minimize2`.
    #[cfg(test)]
    pub fn restore_glyph_icon_for_tests() -> icons::Icon { icons::Icon::Minimize2 }

    /// Test-only: number of hosts currently in the bar's host list.
    /// Used by the close-fade tests to verify a host stays until the
    /// fade elapses.
    #[cfg(test)]
    pub fn host_count(&self) -> usize { self.hosts.len() }

    /// Test-only: whether a host with `id` is still present. Paired
    /// with `host_count` for the "closing host stays until fade
    /// complete" assertion.
    #[cfg(test)]
    pub fn has_host(&self, id: u64) -> bool {
        self.hosts.iter().any(|(i, _)| *i == id)
    }

    /// Test-only: force the closing-fade start timestamp for `id`
    /// to an already-expired `Instant` so the next `handle_event`
    /// sweep finalises the host removal. Lets tests drive the close
    /// flow deterministically without sleeping past `CLOSE_FADE_MS`.
    #[cfg(test)]
    pub fn force_expire_closing(&mut self, id: u64) {
        if self.closing_hosts.contains_key(&id) {
            self.closing_hosts
                .insert(id, Instant::now() - Duration::from_millis(CLOSE_FADE_MS as u64 + 50));
        }
    }

    /// Test-only accessor: is there at least one host currently in
    /// the closing-fade window? Mirrors the predicate used inside
    /// `needs_animation_frame`.
    #[cfg(test)]
    pub fn has_closing_host(&self, id: u64) -> bool {
        self.closing_hosts.contains_key(&id)
    }

    /// Test-only: directly set `closing_at` without waiting for the
    /// full "last tab was closed" drain sweep. Used by `GroupRow`'s
    /// prune tests to seed a group with an already-elapsed close
    /// timestamp (or a fresh one) and assert on the prune vs.
    /// retain branch deterministically.
    #[cfg(test)]
    pub(crate) fn set_closing_at_for_tests(&mut self, t: Instant) {
        self.closing_at = Some(t);
    }

    /// Test-only: directly set `close_hover` for the maximized close
    /// chip without routing a CursorMoved event. Lets the parity tests
    /// exercise the red-500/80 hover branch even when layout hasn't
    /// populated `close_rect`.
    #[cfg(test)]
    pub(crate) fn set_close_hover_for_tests(&mut self, hover: bool) {
        self.close_hover = hover;
    }

    /// Test-only: current fill color the maximized close chip paints
    /// under this theme + hover state. Mirrors the branches inside
    /// `draw` so the visual-parity tests catch drift without a GPU.
    #[cfg(test)]
    pub(crate) fn maximized_close_chip_color_for_tests(
        &self,
        theme: &Theme,
    ) -> vello::peniko::Color {
        if self.close_hover {
            theme.tokens.colors.semantic.danger.with_alpha_factor(0.8)
        } else {
            vello::peniko::Color::rgba8(255, 255, 255, 38)
        }
    }

    /// Test-only: current glyph color the maximized close chip paints.
    /// Pairs with `maximized_close_chip_color_for_tests` — on hover the
    /// concept promotes the `×` to full white (`hover:text-white`).
    #[cfg(test)]
    pub(crate) fn maximized_close_glyph_color_for_tests(
        &self,
        theme: &Theme,
    ) -> vello::peniko::Color {
        if self.close_hover {
            vello::peniko::Color::rgba8(255, 255, 255, 255)
        } else {
            theme.tokens.colors.text_primary
        }
    }
}

impl Widget for WindowGroup {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let Some(h) = self.active_host_mut() { h.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool { self.active_host_mut().map(|h| h.update(s, p)).unwrap_or(false) }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let bar_node = self.bar.build_layout(taffy, theme, node_map);
        let active_id = self.bar.active_id();
        let tile_node = if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.build_layout(taffy, theme, node_map)
        } else {
            taffy.new_leaf(Style::default()).unwrap()
        };
        // Maximized groups absorb all remaining row width via a huge
        // `flex_grow` + zero `flex_basis`, so siblings squeeze down to
        // min-content. Mirrors the concept's `width: 100vw` on the
        // fixed-position maximized window (`WindowFrame.tsx:188-200`)
        // and gives the per-tab Maximize2 click a visible result.
        let mut style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size { width: Dimension::length(self.width.pixels() as f32), height: Dimension::percent(1.0) },
            gap: taffy::Size { width: length(0.0), height: length(0.0) },
            ..Default::default()
        };
        if self.maximized {
            style.size.width = Dimension::auto();
            style.flex_grow = 1.0e6;
            style.flex_basis = Dimension::length(0.0);
        }
        let node = taffy.new_with_children(style, &[bar_node, tile_node]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        let children = taffy.children(node).unwrap();
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        self.bar.apply_layout(taffy, children[0], new_origin);
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.apply_layout(taffy, children[1], new_origin);
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        // Per-group spawn / close fades are mutually exclusive in
        // practice — a group can't be both "just arrived" and "last
        // tab just left" on the same frame. We check close first so
        // a latent `spawned_at` from a historically-rare reuse never
        // clobbers the shutdown curve the user is watching.
        //
        // Spawn (`start_spawn_animation` on a freshly-created group,
        // typically via split-into-new-group) runs the radial-workspace
        // concept's `initial={{ opacity: 0, scale: 0.92, x: -16 }}` →
        // `animate={{ opacity: 1, scale: 1, x: 0 }}` curve over
        // `SPAWN_ANIM_MS` with a cubic-out ease.
        //
        // Close (`start_closing` latched after the last tab's host
        // fade sweeps through) runs the mirror curve over
        // `GROUP_CLOSE_FADE_MS`: α 1 → 0, scale 1 → 0.95.
        //
        // Either way, the whole paint block (bar + tile + chrome
        // rings) composes under a single alpha layer + translate +
        // scale so pill hover targets and resize-handle hit-tests
        // stay aligned with what the user sees on screen mid-fade.
        let (fade_alpha, fade_x, fade_y, fade_scale): (f32, f64, f64, f64) = if let Some(started) =
            self.closing_at
        {
            // Close curve: α 1 → 0, scale 1 → 0.95, no offset (the
            // per-host curve already carries the y-drift; the group
            // shell just fades + shrinks about its centre).
            let raw = (started.elapsed().as_millis() as f32
                / GROUP_CLOSE_FADE_MS as f32)
                .clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - raw).powi(3);
            let alpha = (1.0 - eased).max(0.0);
            let scale = (1.0 - 0.05 * eased) as f64;
            (alpha, 0.0, 0.0, scale)
        } else if let Some(started) = self.spawned_at {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(SPAWN_ANIM_MS) {
                (1.0, 0.0, 0.0, 1.0)
            } else {
                let raw = (elapsed.as_secs_f64()
                    / (SPAWN_ANIM_MS as f64 / 1000.0))
                    .clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - raw).powi(3);
                let scale = SPAWN_ANIM_INITIAL_SCALE
                    + (1.0 - SPAWN_ANIM_INITIAL_SCALE) * eased;
                let x_off = SPAWN_ANIM_X_OFFSET * (1.0 - eased);
                (eased as f32, x_off, 0.0, scale)
            }
        } else {
            (1.0, 0.0, 0.0, 1.0)
        };
        let spawn_active = fade_alpha < 1.0;
        if spawn_active {
            // Clip with a generous margin so the concept-faithful glow
            // rings (search_active, maximized outline) aren't clipped
            // mid-fade. 16 px covers the widest outer stroke currently.
            let clip = Rect::new(
                self.rect.x0 - 16.0,
                self.rect.y0 - 16.0,
                self.rect.x1 + 16.0,
                self.rect.y1 + 16.0,
            );
            scene.push_layer(
                vello::peniko::BlendMode::default(),
                fade_alpha,
                transform,
                &clip,
            );
        }
        // All subsequent paint uses `transform` shifted by the fade's
        // x/y-offset and scaled about the group's center point so the
        // group reads as "sliding into place". `fade_x` carries the
        // spawn slide-in (-16 px → 0); `fade_y` is reserved for the
        // close curve's centre-shrink (currently 0 — kept in the
        // tuple so a future tweak can re-introduce a subtle settle).
        let transform = if fade_x != 0.0 || fade_y != 0.0 || fade_scale != 1.0 {
            let cx = self.rect.center().x;
            let cy = self.rect.center().y;
            transform
                * Affine::translate((cx + fade_x, cy + fade_y))
                * Affine::scale(fade_scale)
                * Affine::translate((-cx, -cy))
        } else {
            transform
        };
        self.bar.draw(scene, transform, tr, theme, presence);
        let active_id = self.bar.active_id();
        // Mirror `search_active` onto the active host so its card paints
        // the concept's focused indigo bloom
        // (`shadow-[0_0_60px_rgba(99,102,241,0.6)]`, WindowFrame.tsx:359).
        let focused = self.search_active;
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.set_focused(focused);
            h.draw(scene, transform, tr, theme, presence);
        }
        // Concept-parity close fade-out pass on hosts. Mirrors the
        // per-tile spawn curve in `tile_host.rs` — opacity 1 → 0,
        // scale 1 → 0.95, y 0 → -20 over `CLOSE_FADE_MS`. Painted
        // over the active host so the closing tile visibly settles
        // away instead of popping. Snapshotted id list so we can
        // borrow `self.hosts` mutably inside the loop without
        // fighting the borrow checker against `self.closing_hosts`.
        let close_window = Duration::from_millis(CLOSE_FADE_MS as u64);
        let closing_ids: Vec<(u64, Instant)> = self
            .closing_hosts
            .iter()
            .filter(|(_, t)| t.elapsed() < close_window)
            .map(|(id, t)| (*id, *t))
            .collect();
        for (id, started) in closing_ids {
            let elapsed = started.elapsed().as_secs_f64();
            let raw = (elapsed / (CLOSE_FADE_MS as f64 / 1000.0)).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - raw).powi(3);
            let alpha = (1.0 - eased) as f32;
            let scale = 1.0 + (-0.05) * eased; // 1.0 → 0.95
            let y_offset = -20.0 * eased;
            if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == id) {
                let rect = h.get_rect();
                // Generous clip: match the tile-spawn clip so shadow
                // sprites aren't chopped as the host shrinks.
                let clip = Rect::new(
                    rect.x0 - 160.0,
                    rect.y0 - 160.0,
                    rect.x1 + 160.0,
                    rect.y1 + 160.0,
                );
                scene.push_layer(
                    vello::peniko::BlendMode::default(),
                    alpha.max(0.0),
                    transform,
                    &clip,
                );
                let centre = rect.center();
                let close_transform = transform
                    * Affine::translate((0.0, y_offset))
                    * Affine::translate((centre.x, centre.y))
                    * Affine::scale(scale)
                    * Affine::translate((-centre.x, -centre.y));
                h.draw(scene, close_transform, tr, theme, presence);
                scene.pop_layer();
            }
        }
        // resize handle visualization
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        let col = if self.hover_resize || self.resize_origin.is_some() { theme.tokens.colors.border_focus } else { theme.tokens.colors.border_standard };
        scene.fill(vello::peniko::Fill::NonZero, transform, col, None, &handle_rect);

        // Drag-in-flight visual: subtle accent ring around the whole group
        // so the user has a spatial signal the drag is live. Matches the
        // concept's "drag-over" border state for the source group. Skipped
        // while maximized — the 2 px maximized outline takes priority.
        if self.bar.drag_in_flight() && !self.maximized {
            let ring = Rect::new(
                self.rect.x0 - 1.0,
                self.rect.y0 - 1.0,
                self.rect.x1 + 1.0,
                self.rect.y1 + 1.0,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                theme.tokens.colors.accent[5].with_alpha_factor(0.6),
                None,
                &ring,
            );
        }

        // Search-active visual: concept's `ring-4 ring-indigo-500/50` +
        // `shadow-[0_0_60px_rgba(99,102,241,0.6)]`. We paint two concentric
        // indigo strokes (inner 4 px at 50% alpha, outer 8 px at 24% alpha)
        // to approximate the ring + shadow pair without introducing a blur
        // primitive. Draws behind the maximized outline so a group can be
        // simultaneously maximized and searching (as the concept allows).
        if self.search_active {
            let ring_outer = Rect::new(
                self.rect.x0 - 6.0,
                self.rect.y0 - 6.0,
                self.rect.x1 + 6.0,
                self.rect.y1 + 6.0,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(8.0),
                transform,
                theme.tokens.colors.accent[5].with_alpha_factor(0.24),
                None,
                &ring_outer,
            );
            let ring_inner = Rect::new(
                self.rect.x0 - 2.0,
                self.rect.y0 - 2.0,
                self.rect.x1 + 2.0,
                self.rect.y1 + 2.0,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(4.0),
                transform,
                theme.tokens.colors.accent[5].with_alpha_factor(0.5),
                None,
                &ring_inner,
            );
        }

        // Maximized visual: 2 px accent outline around the whole group so
        // the user gets immediate feedback from the tab's maximize click
        // even before the shell paints concept-style floating controls.
        if self.maximized {
            let outline = Rect::new(
                self.rect.x0 - 1.0,
                self.rect.y0 - 1.0,
                self.rect.x1 + 1.0,
                self.rect.y1 + 1.0,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &outline,
            );

            // Concept-parity auto-hide: only paint the floating chrome
            // (title pill + Restore + close) while the cursor sits in
            // the reveal band (y <= MAXIMIZED_CHROME_REVEAL_Y). Mirrors
            // `WindowFrame.tsx:108-123` where `clientY <= 80` toggles
            // `chromeVisible`; outside the band the chrome fades out so
            // the maximized content reads cleanly. Hit-test math
            // (restore_rect/close_rect) keeps running below — just gate
            // the visual paint here so a click that lands on the chrome
            // while it's visible still resolves through the existing
            // handle_event arms.
            if self.should_show_maximized_chrome(Some(self.last_cursor.y)) {

            // Floating controls at the top of the maximized group, matching
            // the concept's `[app title]  …  [Restore][×]` layout.
            let pad = 6.0_f64;
            let pill_h = 22.0_f64;
            let y0 = self.rect.y0 + pad;

            // Left: app-title pill. Shows the active tab's title so the
            // maximized tile still tells you which app owns the surface.
            let active_title = self
                .bar
                .tabs()
                .iter()
                .find(|t| t.id == self.bar.active_id())
                .map(|t| t.title.clone())
                .unwrap_or_default();
            let title_size = theme.tokens.typography.scale.xs;
            let (tw, th) = tr.measure(&active_title, title_size, None);
            let title_pill_w = (tw as f64 + 24.0).max(80.0);
            let title_pill = Rect::new(
                self.rect.x0 + pad,
                y0,
                self.rect.x0 + pad + title_pill_w,
                y0 + pill_h,
            );
            let title_rr =
                vello::kurbo::RoundedRect::from_rect(title_pill, pill_h / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                vello::peniko::Color::rgba8(255, 255, 255, 38),
                None,
                &title_rr,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &title_rr,
            );
            let title_tx = title_pill.center().x - tw as f64 / 2.0;
            let title_ty = title_pill.center().y - th as f64 / 2.0;
            tr.draw_text(
                scene,
                &active_title,
                title_size,
                theme.tokens.colors.text_primary,
                transform * vello::kurbo::Affine::translate((title_tx, title_ty)),
                None,
            );

            // Right-action cluster: Restore pill + close glyph.
            let close_w = 22.0_f64;
            let close_x0 = self.rect.x1 - close_w - pad;
            self.close_rect = Rect::new(close_x0, y0, close_x0 + close_w, y0 + pill_h);
            let restore_w = 76.0_f64;
            let restore_x0 = close_x0 - restore_w - 4.0;
            self.restore_rect =
                Rect::new(restore_x0, y0, restore_x0 + restore_w, y0 + pill_h);

            // Restore pill.
            let pill = vello::kurbo::RoundedRect::from_rect(
                self.restore_rect,
                pill_h / 2.0,
            );
            let fill_alpha: u8 = if self.restore_hover { 64 } else { 38 };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                vello::peniko::Color::rgba8(255, 255, 255, fill_alpha),
                None,
                &pill,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &pill,
            );
            // Prepend a lucide `Minimize2` glyph to the Restore pill so
            // the action reads at a glance — matches the concept's
            // maximized floating controls in WindowFrame.tsx which
            // render a `<Minimize2 className="w-4 h-4" />` alongside
            // the restore affordance. Accessibility: the text label is
            // preserved (screen readers and i18n still see "Restore");
            // we just offset it horizontally to leave a 14 px icon
            // slot at the pill's leading edge. `window_group_restore_pill_uses_minimize2`
            // pins this variant.
            let icon_slot = Rect::new(
                self.restore_rect.x0 + 8.0,
                self.restore_rect.y0 + 4.0,
                self.restore_rect.x0 + 22.0,
                self.restore_rect.y1 - 4.0,
            );
            icons::draw_icon(
                scene,
                transform,
                icon_slot,
                icons::Icon::Minimize2,
                theme.tokens.colors.text_primary,
                1.5,
            );
            let label = "Restore";
            let (_lw, lh) = tr.measure(label, title_size, None);
            // Shift the label past the icon slot (x0 + 28.0) instead of
            // recentring, so the glyph + label cluster sits together on
            // the left of the pill.
            let lx = self.restore_rect.x0 + 28.0;
            let ly = self.restore_rect.center().y - lh as f64 / 2.0;
            tr.draw_text(
                scene,
                label,
                title_size,
                theme.tokens.colors.text_primary,
                transform * vello::kurbo::Affine::translate((lx, ly)),
                None,
            );

            // Close glyph: matches the concept's `X` button, tinted red on
            // hover so the destructive intent is obvious. Hover fill is
            // `hover:bg-red-500/80` (WindowFrame.tsx:249) — red-500 at
            // 80% alpha — so we multiply `semantic.danger` by 0.8 rather
            // than painting it fully opaque.
            let close_rr =
                vello::kurbo::RoundedRect::from_rect(self.close_rect, pill_h / 2.0);
            let close_fill = if self.close_hover {
                theme.tokens.colors.semantic.danger.with_alpha_factor(0.8)
            } else {
                vello::peniko::Color::rgba8(255, 255, 255, 38)
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                close_fill,
                None,
                &close_rr,
            );
            // Close glyph: lucide `X` painted over the (already-filled)
            // red-on-hover close chip. Replaces the previous literal
            // `×` codepoint + measure/draw_text pair so the maximized
            // floating controls read identically to the concept's
            // `<X className="w-4 h-4" />` in WindowFrame.tsx. The
            // filled chip underneath is unchanged; the icon paints on
            // top via `icons::draw_icon`. On hover the concept promotes
            // the glyph to full white (`hover:text-white`) so the × reads
            // crisp against the red fill.
            let glyph_color = if self.close_hover {
                vello::peniko::Color::rgba8(255, 255, 255, 255)
            } else {
                theme.tokens.colors.text_primary
            };
            icons::draw_icon(
                scene,
                transform,
                self.close_rect,
                icons::Icon::X,
                glyph_color,
                1.5,
            );
            }
        } else {
            self.restore_rect = Rect::ZERO;
            self.close_rect = Rect::ZERO;
        }

        // Close out the spawn-fade layer if one was pushed at the top
        // of `draw`. Mirrors the `spawn_active` branch exactly so every
        // `push_layer` has a matching pop.
        if spawn_active {
            scene.pop_layer();
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        let children = taffy.children(node_id).unwrap();
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                self.hover_resize = handle_rect.contains(pt);
                self.restore_hover = self.maximized && self.restore_rect.contains(pt);
                self.close_hover = self.maximized && self.close_rect.contains(pt);
                if self.resize_origin.is_some() { self.update_resize(pt.x as f32); *handled = true; return; }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
                if self.maximized && self.close_rect.contains(self.last_cursor) =>
            {
                // Close-glyph click on a maximized group: route through
                // the same deferred fade flow the per-tab × click uses
                // (`bar.start_closing` → `closing_hosts` → sweep). A
                // previous version mutated `self.hosts` + `self.bar`
                // synchronously here, which meant the 180 ms tab pill
                // fade and the follow-on group-close fade were both
                // skipped: the entire group just popped out of
                // existence. Now we only latch the close; the sweep at
                // the bottom of `handle_event` runs on the next tick
                // and removes the host + finalises the pill + (when
                // tabs empty) starts the group-level close fade, so
                // the user sees a coherent shrink whether they close
                // from maximised or the tab bar.
                //
                // We still exit maximize immediately so the floating
                // restore / close chrome stops painting — it's latched
                // to the active tab and that tab is on its way out;
                // keeping the chrome up for one extra frame would
                // render it over a fading tile that no longer owns
                // the interaction.
                let active_id = self.bar.active_id();
                self.bar.start_closing(active_id);
                self.closing_hosts
                    .entry(active_id)
                    .or_insert_with(Instant::now);
                self.maximized = false;
                *handled = true;
                return;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
                if self.maximized && self.restore_rect.contains(self.last_cursor) =>
            {
                // Restore-pill click: exit maximize. Handled here (not via
                // bar.pending_max) so the click never races with a tab
                // selection under the pill's geometry.
                self.maximized = false;
                *handled = true;
                return;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } if self.hover_resize => {
                // the origin x is captured at the current pointer via the current rect
                self.begin_resize(self.rect.x1 as f32);
                *handled = true; return;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } if self.resize_origin.is_some() => {
                self.end_resize(); *handled = true; return;
            }
            _ => {}
        }
        self.bar.handle_event(event, scale_factor, m, s, p, handled, focus_target, children[0], taffy);

        // Sweep any hosts whose close fade-out has fully elapsed.
        // `self.bar.start_closing` runs on the press frame (inside
        // `self.bar.handle_event` above) and simultaneously publishes
        // `pending_close` for us to drain below. Actual host removal
        // is deferred so the fade curve plays out with the tile
        // still in place. Mirrors the tile-level spawn-in pattern in
        // `tile_host.rs` — both sides of the animation live in the
        // widget layer, not the shell.
        let close_window = Duration::from_millis(CLOSE_FADE_MS as u64);
        let expired: Vec<u64> = self
            .closing_hosts
            .iter()
            .filter(|(_, started)| started.elapsed() >= close_window)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.closing_hosts.remove(&id);
            if let Some(idx) = self.hosts.iter().position(|(i, _)| *i == id) {
                self.hosts.remove(idx);
            }
            self.bar.finalize_close(id);
        }

        // If the final tab's host-level close fade just expired and
        // dropped the last occupant, latch the group-level close
        // fade so `GroupRow` paints us shrinking out over the next
        // `GROUP_CLOSE_FADE_MS` before pruning. Gated on `is_closing`
        // so a subsequent tick inside the fade window doesn't
        // restart the curve. We intentionally check
        // `bar.tabs().is_empty()` instead of just `hosts.is_empty()`
        // — the two track each other one-for-one, but a future
        // refactor could introduce tab placeholders without hosts,
        // and the user-visible "group is empty" signal is the bar.
        if !self.is_closing()
            && self.bar.tabs().is_empty()
            && self.hosts.is_empty()
            && self.closing_hosts.is_empty()
        {
            self.start_closing();
        }

        // Close a tab: drain the bar's one-shot `pending_close` and
        // latch the host-level fade-out. The bar has already shifted
        // active-id to the neighbour and snapshotted its pill into
        // `closing_tabs` so the pill keeps painting while the host
        // fades out. We keep the `TileHost` in `self.hosts` for the
        // duration of `CLOSE_FADE_MS`, then the sweep above drops it.
        if let Some(close_id) = self.bar.take_close() {
            self.closing_hosts
                .entry(close_id)
                .or_insert_with(Instant::now);
            *handled = true;
        }

        // Maximize toggle from the tab bar's hover-action icon. Flagging
        // `style_dirty` makes `take_structure_dirty` request a
        // `build_layout` rebuild so the new flex_grow / width style
        // takes effect — without it the cached taffy node keeps the
        // pre-toggle width and the click looks like a no-op.
        if let Some(_id) = self.bar.take_max() {
            self.maximized = !self.maximized;
            self.style_dirty = true;
            *handled = true;
        }

        // Search toggle from the tab bar's hover-action icon. Same pattern
        // as maximize — the bar publishes `pending_search` once per click,
        // we drain it here and flip the search-active ring on the group.
        if let Some(_id) = self.bar.take_search() {
            self.search_active = !self.search_active;
            *handled = true;
        }

        // Drag-reorder within the bar: apply the same permutation to
        // both the bar's tab list AND the hosts vec so the visible
        // pill order matches the content order. Previously only `hosts`
        // got reordered while `self.bar.tabs` stayed in the original
        // order — the user saw drop zones but no visible reorder.
        //
        // Parallel-vec hardening (Wave 2 / 2-D): `bar.tabs` and `hosts`
        // are the two halves of a strictly-aligned 1:1 mapping. A
        // future refactor that mutates one without the other would
        // silently desync the visible pill at index `i` from the
        // content rendered at host index `i`. The `debug_assert_eq!`
        // below trips loudly in debug builds the moment the lengths
        // diverge — index-based bounds checks alone (the prior
        // `if from < self.hosts.len() && to < self.hosts.len()` guard)
        // would mask the asymmetry: the bar would be permuted while
        // the hosts vec quietly stayed put. Now we permute
        // unconditionally, with the assert documenting the invariant
        // that's expected to hold every time we get here.
        if let Some((from, to)) = self.bar.take_reorder() {
            debug_assert_eq!(
                self.bar.tabs().len(),
                self.hosts.len(),
                "WindowGroup parallel-vec invariant violated: bar.tabs.len() \
                 ({}) != hosts.len() ({}). The bar's pill list and the host \
                 content list must stay 1:1 — every mutation of one must be \
                 mirrored on the other.",
                self.bar.tabs().len(),
                self.hosts.len(),
            );
            self.bar.reorder(from, to);
            let item = self.hosts.remove(from);
            self.hosts.insert(to, item);
            *handled = true;
        }

        if *handled { return; }
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.handle_event(event, scale_factor, m, s, p, handled, focus_target, children[1], taffy);
        }
    }
    fn name(&self) -> &str { "WindowGroup" }
    fn get_rect(&self) -> Rect { self.rect }
    fn wants_pointer_capture(&self) -> bool { self.resize_origin.is_some() }
    fn take_structure_dirty(&mut self) -> bool {
        // Bubble both the TabBar's and every host's dirty flag. Ordered
        // with `|` over a fold so each child still gets its flag cleared
        // even if an earlier one already reported dirty.
        let bar = self.bar.take_structure_dirty();
        let hosts = self
            .hosts
            .iter_mut()
            .map(|(_, h)| h.take_structure_dirty())
            .fold(false, |acc, d| acc || d);
        let style = std::mem::take(&mut self.style_dirty);
        bar || hosts || style
    }
    fn needs_animation_frame(&self) -> bool {
        // The tab bar runs a frame-driven drop-zone expand animation
        // during drag; its signal would otherwise be invisible to ancestors
        // because `WindowGroup::children()` uses the default (empty) impl.
        // Bubble it up plus any active host's signal so nested app
        // animations (e.g. caret blink in future revisions) keep pumping.
        // Also honour the per-group spawn fade while it's in flight so
        // the motion plays out without visible stutter.
        let spawn_in_flight = self
            .spawned_at
            .map(|t| t.elapsed() < Duration::from_millis(SPAWN_ANIM_MS))
            .unwrap_or(false);
        // Per-host close fade (180 ms). Same idea as `spawn_in_flight`
        // but for the concept's `exit={{ opacity: 0, scale: 0.95, y: -20 }}`
        // tab-close curve: while any host is within its fade window
        // we need to keep pumping redraws so the pill + host shrink
        // + fade visibly plays out instead of popping.
        let close_window = Duration::from_millis(CLOSE_FADE_MS as u64);
        let close_in_flight = self
            .closing_hosts
            .values()
            .any(|t| t.elapsed() < close_window);
        // Group-level close fade (180 ms). While `closing_at` is
        // latched and still inside the fade window the `App` run
        // loop needs to keep pumping frames so the group visibly
        // shrinks out instead of popping away the instant the last
        // tab's fade expires.
        let group_close_in_flight = self.closing_at.is_some_and(|t| {
            t.elapsed() < Duration::from_millis(GROUP_CLOSE_FADE_MS)
        });
        spawn_in_flight
            || close_in_flight
            || group_close_in_flight
            || self.bar.needs_animation_frame()
            || self
                .hosts
                .iter()
                .any(|(_, h)| h.needs_animation_frame())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{AppKind, TabDescriptor, TileContent};

    fn make_group() -> WindowGroup {
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "A"),
                TileContent::Stub { kind: AppKind::Settings, label: "a".into() },
            ),
            (
                TabDescriptor::new(2, "B"),
                TileContent::Stub { kind: AppKind::Settings, label: "b".into() },
            ),
        ];
        WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
    }

    /// Pressing the per-tab Maximize2 hit rect flips
    /// `WindowGroup::maximized` end-to-end (bar publishes `pending_max`
    /// → WindowGroup drains it → flag toggles), and a second click
    /// flips it back. Regression for the live-binary "doesn't work"
    /// report.
    #[test]
    fn maximize_click_on_tab_pill_toggles_window_group_maximized() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use crate::theme::Theme;
        use winit::event::{ElementState, MouseButton};

        let mut g = make_group();
        assert!(!g.maximized, "fresh group must start non-maximized");

        // Full layout pass so the bar's `max_rects` are authoritative —
        // the press arm in `TabBar::handle_event` does a fresh hit-test
        // against `max_rects[i].contains(last_cursor)`, so the rect must
        // be populated by `apply_layout` before we drive the click.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // Aim the synthetic cursor at the centre of tab 0's maximize rect.
        let max_rect = g.bar.max_rects()[0];
        let click_pt = max_rect.center();

        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(click_pt.x, click_pt.y),
        };
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut handled = false;
        let mut focus_target = None;
        // Seed last_cursor on the icon — the bar's press arm uses the
        // cached cursor for its fresh hit-test.
        g.handle_event(
            &moved, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );
        let mut handled = false;
        g.handle_event(
            &press, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );

        assert!(handled, "press on the maximize icon must be claimed");
        assert!(
            g.maximized,
            "maximize click on a tab pill must drive WindowGroup::maximized true",
        );

        // Second click flips it back to non-maximized — concept's
        // `setIsMaximized(!isMaximized)` is a click-to-toggle, not a
        // one-way latch.
        let mut handled = false;
        g.handle_event(
            &moved, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );
        let mut handled = false;
        g.handle_event(
            &press, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );
        assert!(
            !g.maximized,
            "second maximize click must restore (toggle off)",
        );
    }

    /// Toggling `maximized` must report `take_structure_dirty()` so
    /// `App` re-runs `build_layout` and the cached taffy node picks
    /// up the new width style — otherwise the click visibly does
    /// nothing.
    #[test]
    fn maximize_toggle_marks_structure_dirty_for_layout_rebuild() {
        let mut g = make_group();
        // Fresh group is clean.
        assert!(!g.take_structure_dirty(), "fresh group is not dirty");

        // Simulate the bar publishing a maximize request — same code
        // path the per-tab click hits, just without driving the full
        // event pump.
        g.bar.pending_max = Some(1);
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        // A no-op cursor move drives the WindowGroup drain block, which
        // is what reads `pending_max` → flips `maximized` → sets
        // `style_dirty`.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> = std::collections::HashMap::new();
        let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.new_with_children(Style::default(), &[group_node]).unwrap();
        use taffy::prelude::TaffyMaxContent;
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        g.handle_event(
            &moved, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );

        assert!(g.maximized, "maximize drain must flip the flag");
        assert!(
            g.take_structure_dirty(),
            "maximize toggle must report structure dirty so App re-runs build_layout",
        );
        // Drained — second poll within the same frame is clean.
        assert!(!g.take_structure_dirty(), "structure_dirty drains on poll");
    }

    #[test]
    fn search_active_toggles_when_bar_publishes_pending_search() {
        // Fresh group starts not-searching. Seeding `bar.pending_search`
        // and re-running the drain block flips the flag, and a second
        // drain flips it back — matching the concept's click-to-toggle
        // affordance on the tab pill's search glyph.
        let mut g = make_group();
        assert!(!g.search_active, "fresh group must not be searching");
        g.bar.pending_search = Some(1);
        if let Some(_id) = g.bar.take_search() {
            g.search_active = !g.search_active;
        }
        assert!(g.search_active, "drain should flip search_active on");
        g.bar.pending_search = Some(1);
        if let Some(_id) = g.bar.take_search() {
            g.search_active = !g.search_active;
        }
        assert!(!g.search_active, "second drain should flip search_active off");
    }

    #[test]
    fn start_spawn_animation_records_timestamp_and_pumps_frames() {
        let mut g = make_group();
        assert!(g.spawned_at.is_none(), "fresh group has no spawn timestamp");
        g.start_spawn_animation();
        assert!(g.spawned_at.is_some(), "start_spawn_animation records the start");
        assert!(
            g.needs_animation_frame(),
            "in-flight spawn fade must pump the run loop",
        );
    }

    #[test]
    fn spawn_fade_window_clears_after_duration() {
        let mut g = make_group();
        g.spawned_at = Some(Instant::now() - Duration::from_millis(SPAWN_ANIM_MS + 16));
        assert!(
            !g.needs_animation_frame(),
            "after the spawn window elapses, the fade no longer demands frames",
        );
    }

    /// The spawn animation must be visibly progressive — at the very
    /// first frame the eased progress is near zero (group reads as
    /// "just arriving"), around the midpoint it's clearly past half
    /// (cubic-out ramps fast early), and once the duration elapses it
    /// pins to 1.0. Without this guard a regression that shortened the
    /// duration to <50 ms or skipped the easing would re-introduce the
    /// "spawn invisible / instant" report from the wheel-rebuild
    /// worker.
    #[test]
    fn spawn_animation_advances_eased_progress_over_time() {
        let mut g = make_group();
        // No spawn in flight — accessor pins to 1.0.
        assert_eq!(g.spawn_anim_t(), 1.0, "idle group reports finished progress");

        // First frame: started "now", elapsed ~0. Eased progress is
        // near zero — the group should be barely visible.
        g.start_spawn_animation();
        let t_start = g.spawn_anim_t();
        assert!(
            t_start < 0.05,
            "first-frame spawn progress must be near zero, got {t_start}",
        );

        // Roughly midway: cubic-out at raw=0.5 yields 1 - 0.5^3 = 0.875,
        // so we expect well past 0.5. Backdate the timestamp instead of
        // sleeping so the test doesn't waste real time.
        g.spawned_at = Some(Instant::now() - Duration::from_millis(SPAWN_ANIM_MS / 2));
        let t_mid = g.spawn_anim_t();
        assert!(
            t_mid > 0.5 && t_mid < 1.0,
            "mid-spawn progress must be past half but not yet complete, got {t_mid}",
        );

        // Past the duration: pins to 1.0.
        g.spawned_at = Some(Instant::now() - Duration::from_millis(SPAWN_ANIM_MS + 16));
        assert_eq!(
            g.spawn_anim_t(),
            1.0,
            "after the spawn window elapses, progress pins to 1.0",
        );
    }

    /// The spawn duration must stay long enough that the user can see
    /// the slide-in. Lock it to the "punchy but readable" 350-450 ms
    /// band so a future pass can't silently shorten it back to the
    /// 320 ms tuning that read as "instant" in shots-driver captures.
    #[test]
    fn spawn_animation_duration_in_visible_band() {
        assert!(
            (350..=450).contains(&SPAWN_ANIM_MS),
            "spawn duration must stay in the 350-450 ms readability band, got {SPAWN_ANIM_MS}",
        );
    }

    #[test]
    fn window_group_close_glyph_is_lucide_x() {
        // Pins the maximized floating close chip to lucide's `X`
        // glyph. Mirrors the per-tab close pin in `tabs.rs` — both
        // exist to make a future revert to the literal `×` codepoint
        // or any hand-rolled primitive fail closed at `cargo test`.
        assert_eq!(
            WindowGroup::close_glyph_icon_for_tests(),
            icons::Icon::X,
        );
    }

    #[test]
    fn window_group_restore_pill_uses_minimize2() {
        // Pins the leading glyph of the Restore pill to lucide's
        // `Minimize2`. Paired with a horizontal offset on the
        // "Restore" text label in `draw` so the icon + text cluster
        // reads as one action.
        assert_eq!(
            WindowGroup::restore_glyph_icon_for_tests(),
            icons::Icon::Minimize2,
        );
    }

    #[test]
    fn insert_detached_tab_plays_tile_spawn_animation() {
        // A tab dragged into an existing `WindowGroup` must fire the
        // per-tile spawn fade so the new pill's content reads as
        // "arriving" rather than popping in opaque. The sibling
        // split-into-new-group path plays the group-level fade
        // instead; we never double up.
        let mut group = WindowGroup::new(0, vec![], 0, WindowWidth::Normal);
        let host = TileHost::new(TileContent::Stub {
            kind: AppKind::Settings,
            label: "stub".to_string(),
        });
        let desc = TabDescriptor::new(42, "new");
        group.insert_detached_tab(0, desc, host);
        let inserted = group
            .hosts
            .iter()
            .find(|(i, _)| *i == 42)
            .expect("inserted host present");
        assert!(
            inserted.1.spawn_in_flight(),
            "inserting a detached tab must fire the tile-level spawn fade",
        );
    }

    /// Detaching a tab whose id is mid-close (it sits in
    /// `closing_hosts` because of the leak path described in B-8 —
    /// a stale entry whose timer never expires after the tab's host
    /// has been moved) must drop the lingering close-fade state.
    /// Otherwise `needs_animation_frame` keeps pumping redraws for
    /// the remainder of the `CLOSE_FADE_MS` window even though the
    /// host itself is gone. (B-8)
    #[test]
    fn detach_tab_clears_closing_state_for_moved_tab() {
        let mut g = make_group();
        // Seed `closing_hosts` directly with tab id 1 — this is the
        // leak shape the plan calls out. The tab is still present in
        // `bar.tabs` (so `detach_tab` can find it) and in `hosts`,
        // but the side-table erroneously carries a fade timestamp.
        g.closing_hosts.insert(1, Instant::now());
        assert!(
            g.closing_hosts.contains_key(&1),
            "precondition: closing_hosts carries the moved tab id",
        );

        let detached = g.detach_tab(1);
        assert!(detached.is_some(), "detach_tab returns the moved tab");

        assert!(
            !g.closing_hosts.contains_key(&1),
            "detach_tab must clear closing_hosts for the moved tab so \
             needs_animation_frame stops pumping for a host that's gone",
        );
        assert!(
            !g.bar.closing_tab_ids().contains(&1),
            "detach_tab must call bar.finalize_close so any pill-side \
             fade state for the moved id is also dropped",
        );
    }

    /// Drive the close flow end-to-end: the host must survive until
    /// the fade window elapses, then the sweep at the top of
    /// `handle_event` drops it from `self.hosts`. Uses
    /// `force_expire_closing` to move the timestamp into the past so
    /// the test doesn't need a real `sleep(CLOSE_FADE_MS)`.
    #[test]
    fn closing_host_stays_until_fade_complete() {
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use crate::theme::Theme;
        use std::collections::HashMap;

        let mut g = make_group();
        // Run one layout pass so the bar has authoritative
        // pill / close rects — `start_closing` snapshots those into
        // its `closing_tabs` vec.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // Latch the close via the bar API. `start_closing` both
        // moves tab 2 into `closing_tabs` and publishes
        // `pending_close = Some(2)` so the `WindowGroup::handle_event`
        // drain below can seed `closing_hosts` without us having to
        // simulate a full press/click sequence through the bar.
        g.bar_mut().start_closing(2);

        // Run a handle_event so the WindowGroup drain code path
        // seeds `closing_hosts`. We use a `CursorMoved` way off the
        // rect so no hover handlers fire and the event reaches the
        // close-drain block at the bottom of `handle_event`.
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
        };
        g.handle_event(
            &moved,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            group_node,
            &taffy,
        );

        assert!(
            g.has_closing_host(2),
            "drain must latch the host-level close fade",
        );
        assert!(
            g.needs_animation_frame(),
            "in-flight close fade must pump redraws",
        );
        assert!(
            g.has_host(2),
            "host stays in `hosts` for the fade duration",
        );
        let host_count_during = g.host_count();

        // Force the close fade to be expired, then run another
        // `handle_event` so the sweep at the top runs. The host
        // should drop.
        g.force_expire_closing(2);
        let moved2 = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
        };
        let mut handled2 = false;
        g.handle_event(
            &moved2,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled2,
            &mut focus_target,
            group_node,
            &taffy,
        );
        assert!(
            !g.has_host(2),
            "sweep drops the host after the fade expires",
        );
        assert_eq!(
            g.host_count(),
            host_count_during - 1,
            "exactly one host removed",
        );
        assert!(
            !g.has_closing_host(2),
            "sweep clears the closing_hosts entry",
        );
    }

    /// Driving a full "close the sole tab" cycle must end with the
    /// group-level close fade latched — `GroupRow` relies on
    /// `is_closing()` → `close_complete()` to sequence the prune so
    /// if the tick logic never flips `closing_at` the group stays
    /// around as an empty shell forever (the pre-feature bug this
    /// commit fixes). We fast-forward the host-level close via
    /// `force_expire_closing` so the test doesn't have to sleep past
    /// `CLOSE_FADE_MS`.
    #[test]
    fn closing_last_tab_kicks_off_group_close() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use crate::theme::Theme;

        // Single-tab group so "close this tab" truly empties the group.
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
            TabDescriptor::new(1, "solo"),
            TileContent::Stub { kind: AppKind::Settings, label: "a".into() },
        )];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

        // Full layout pass so the bar has authoritative pill rects.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // Kick the close flow via the bar: this simulates the
        // user pressing the pill's × on the sole tab.
        g.bar_mut().start_closing(1);

        // First handle_event drains `pending_close` into
        // `closing_hosts`. The group is NOT yet closing because a
        // closing host is still in flight.
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
        };
        g.handle_event(
            &moved, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );
        assert!(
            g.has_closing_host(1),
            "precondition: host-level close latched",
        );
        assert!(
            !g.is_closing(),
            "group close must NOT fire while a host-level close is still in flight",
        );

        // Expire the host-level close; next handle_event sweeps the
        // host away and then discovers the group is empty, which is
        // when the group-level close should latch.
        g.force_expire_closing(1);
        let moved2 = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
        };
        let mut handled2 = false;
        g.handle_event(
            &moved2, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled2, &mut focus_target,
            group_node, &taffy,
        );
        assert!(
            g.is_closing(),
            "after the sole tab's fade sweeps out, the group must latch its own close fade",
        );
    }

    /// Pinned directly against the public API so a regression that
    /// breaks only the elapsed-check (without touching
    /// `start_closing` or `is_closing`) still fails here.
    #[test]
    fn group_close_complete_true_after_fade_duration() {
        let mut g = make_group();
        g.set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(GROUP_CLOSE_FADE_MS + 20),
        );
        assert!(
            g.close_complete(),
            "close_complete must flip true after the fade window elapses",
        );
        // And a fresh close timestamp must NOT be complete yet.
        g.set_closing_at_for_tests(Instant::now());
        assert!(
            !g.close_complete(),
            "a just-latched close must not report complete",
        );
    }

    /// `GroupRow` calls `draw` every frame regardless of close
    /// status — the whole point of the fade is that the group keeps
    /// painting while shrinking out. This test guards against a
    /// panic or a collapsed rect mid-fade.
    #[test]
    fn group_still_paints_during_close_fade() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use crate::theme::Theme;

        let mut g = make_group();
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);
        let pre_rect = g.get_rect();

        g.start_closing();

        // Paint into a throwaway Scene — the assertion is "no panic"
        // and "rect still reports prior bounds for the duration of
        // the fade". The fade scales about the centre via an
        // Affine; the logical `self.rect` used for hit-tests stays
        // put so drop-targets and focus bookkeeping don't shift
        // under the user mid-animation.
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        // TextRenderer is constructed fresh per crate test; we use
        // the same factory the other `draw` tests in this file rely
        // on.
        let mut tr = TextRenderer::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert_eq!(
            g.get_rect(),
            pre_rect,
            "close fade paints over the existing rect; hit-tests must stay stable",
        );
        assert!(
            g.is_closing(),
            "close flag stays latched through the draw pass",
        );
    }

    /// Symmetric to the host-level `window_group_needs_animation_frame_during_close`
    /// above but for the group-level `closing_at` path. Regression
    /// guard for a change that breaks only this pump signal.
    #[test]
    fn group_needs_animation_frame_while_closing() {
        let mut g = make_group();
        assert!(!g.needs_animation_frame(), "precondition: idle group is quiet");
        g.start_closing();
        assert!(
            g.needs_animation_frame(),
            "group-level close fade must pump frames until close_complete",
        );
        // Expired: the run loop can idle again (the prune sweep
        // drops us on the next tick regardless).
        g.set_closing_at_for_tests(
            Instant::now() - Duration::from_millis(GROUP_CLOSE_FADE_MS + 20),
        );
        assert!(
            !g.needs_animation_frame(),
            "after the fade window elapses, frames are no longer required",
        );
    }

    /// A maximized-mode close press must route through the same
    /// deferred tab-fade + group-close-fade flow as the normal
    /// per-tab × click. Previously this path mutated `self.hosts`
    /// and called `bar.remove_tab` synchronously, so closing the
    /// last tab from maximised mode popped the entire group out of
    /// existence with zero animation. The fix calls
    /// `bar.start_closing` + inserts into `closing_hosts`, drops
    /// maximize, and lets the sweep finish the teardown on the
    /// next tick(s). End-to-end this test drives the close press,
    /// asserts the deferred state, then runs a second tick with
    /// the close fade forced-expired and asserts the host +
    /// bar-tab are gone and the group-level close fade is latched.
    #[test]
    fn maximized_close_uses_deferred_flow() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use crate::theme::Theme;
        use winit::event::{ElementState, MouseButton};

        // Single-tab group so the sweep has a clear "last tab just
        // left" trigger for the group-level close fade.
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
            TabDescriptor::new(1, "solo"),
            TileContent::Stub { kind: AppKind::Settings, label: "a".into() },
        )];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style::default(), &[group_node])
            .unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // Put the group into maximized mode and seed `close_rect`
        // + `last_cursor` so the close-press arm matches. The
        // close_rect normally gets populated in `draw` while
        // maximized; tests don't paint so we seed it directly.
        g.maximized = true;
        g.close_rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        g.last_cursor = Point::new(5.0, 5.0);

        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        g.handle_event(
            &press, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled, &mut focus_target,
            group_node, &taffy,
        );

        // Deferred flow: maximize dropped, host still present
        // (fade in-flight), bar has latched the pill-close, and
        // our own closing_hosts carries the active id.
        assert!(handled, "press must be consumed by the close arm");
        assert!(!g.maximized, "maximize must drop immediately");
        assert!(
            g.has_host(1),
            "host stays in `hosts` for the fade duration — no synchronous removal",
        );
        assert!(
            g.bar.closing_tab_ids().contains(&1),
            "bar.start_closing must have latched the tab pill fade",
        );
        assert!(
            g.has_closing_host(1),
            "closing_hosts must carry the active id so the host fade runs",
        );
        // And the group itself is NOT closing yet — a host fade is
        // still in flight; the group-level close only latches once
        // the sweep drains the last host.
        assert!(
            !g.is_closing(),
            "group close must NOT fire while a host-level close is still in flight",
        );

        // Now force-expire the host close and drive another tick.
        // The sweep drops the host + finalises the pill, then the
        // "empty group" check latches the group-level close fade.
        g.force_expire_closing(1);
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        let mut handled2 = false;
        g.handle_event(
            &moved, 1.0, &winit::event::Modifiers::default(),
            &mut state, &root_id, &mut handled2, &mut focus_target,
            group_node, &taffy,
        );

        assert!(!g.has_host(1), "sweep drops the host once the fade expires");
        assert!(
            !g.bar.closing_tab_ids().contains(&1),
            "bar.finalize_close clears the closing pill entry",
        );
        assert!(g.bar.tabs().is_empty(), "bar has no live tabs left");
        assert!(
            g.is_closing(),
            "group-level close fade must latch once the last host has swept out",
        );
    }

    /// `needs_animation_frame` must report true during the close
    /// fade so the `App` run loop keeps pumping redraws until the
    /// curve settles. Pinned independently of the full
    /// close-host-stays test so a regression that breaks only the
    /// frame-pump signal is caught in isolation.
    #[test]
    fn window_group_needs_animation_frame_during_close() {
        let mut g = make_group();
        assert!(
            !g.needs_animation_frame(),
            "precondition: idle group doesn't demand frames",
        );
        // Directly seed `closing_hosts` — bypasses the full event
        // drive so this test stays narrow. Simulates what happens
        // after `handle_event` drains a `pending_close`.
        g.closing_hosts.insert(1, Instant::now());
        assert!(
            g.needs_animation_frame(),
            "a fresh closing host must pump the run loop",
        );
        // Expired: no more frames needed.
        g.closing_hosts
            .insert(1, Instant::now() - Duration::from_millis(CLOSE_FADE_MS as u64 + 20));
        assert!(
            !g.needs_animation_frame(),
            "after the fade window elapses the group goes idle again",
        );
    }

    /// Concept parity: the maximized floating close chip
    /// (WindowFrame.tsx:249) uses `hover:bg-red-500/80`. Mirrors the
    /// per-tab close pill pinned by `tabs::close_hover_uses_red_500_fill`.
    /// On hover the chip fills with the destructive red at 80% alpha
    /// and the glyph promotes to full white; off-hover reverts to the
    /// subtle white chrome tint + `text_primary` glyph.
    #[test]
    fn maximized_close_chip_uses_red_500_on_hover() {
        let mut g = make_group();
        g.maximized = true;
        let theme = Theme::dark();

        // Idle (maximized but close not hovered): subtle white tint,
        // glyph reads as theme's text_primary.
        g.set_close_hover_for_tests(false);
        let idle_chip = g.maximized_close_chip_color_for_tests(&theme);
        assert_eq!(
            (idle_chip.r, idle_chip.g, idle_chip.b, idle_chip.a),
            (255, 255, 255, 38),
            "idle maximized close chip stays as the chrome white tint",
        );
        let idle_glyph = g.maximized_close_glyph_color_for_tests(&theme);
        let expected = theme.tokens.colors.text_primary;
        assert_eq!(
            (idle_glyph.r, idle_glyph.g, idle_glyph.b, idle_glyph.a),
            (expected.r, expected.g, expected.b, expected.a),
            "idle glyph uses theme text_primary",
        );

        // Hover: red-500/80 fill, full-white glyph.
        g.set_close_hover_for_tests(true);
        let hover_chip = g.maximized_close_chip_color_for_tests(&theme);
        assert_eq!(
            (hover_chip.r, hover_chip.g, hover_chip.b, hover_chip.a),
            (239, 68, 68, 204),
            "hover maximized close chip paints red-500 at 80% alpha",
        );
        let hover_glyph = g.maximized_close_glyph_color_for_tests(&theme);
        assert_eq!(
            (hover_glyph.r, hover_glyph.g, hover_glyph.b, hover_glyph.a),
            (255, 255, 255, 255),
            "hover glyph promotes to opaque white (concept: hover:text-white)",
        );
    }

    /// Concept parity: `WindowFrame.tsx:108-123` only shows the
    /// floating chrome (title pill + restore + close cluster) when
    /// `clientY <= 80`. Outside that band the chrome is hidden so the
    /// maximized content fills the tile cleanly. Pin both branches of
    /// `should_show_maximized_chrome` so a regression that drops the
    /// y-gate or shifts the threshold fails closed at `cargo test`.
    #[test]
    fn maximized_chrome_visible_when_cursor_y_le_80() {
        let mut g = make_group();
        g.maximized = true;
        assert!(g.should_show_maximized_chrome(Some(40.0)));
        assert!(g.should_show_maximized_chrome(Some(80.0)));
        assert!(!g.should_show_maximized_chrome(Some(81.0)));
        assert!(!g.should_show_maximized_chrome(Some(400.0)));
        // Cursor not yet observed — concept treats the absence of a
        // cursor signal as "below the band", so the chrome stays
        // hidden until the user moves the pointer into the reveal
        // strip.
        assert!(!g.should_show_maximized_chrome(None));
    }

    /// Belt-and-braces: even if the gate predicate is asked about a
    /// y inside the reveal band, an un-maximized group must never
    /// claim its chrome is visible (there is no chrome to paint when
    /// the tile is tiled).
    #[test]
    fn maximized_chrome_always_hidden_when_not_maximized() {
        let g = make_group();
        assert!(!g.maximized, "fresh group is not maximized");
        assert!(!g.should_show_maximized_chrome(Some(20.0)));
        assert!(!g.should_show_maximized_chrome(Some(80.0)));
        assert!(!g.should_show_maximized_chrome(None));
    }

    /// Parallel-vec invariant: after the bar publishes a `pending_reorder`
    /// and the `WindowGroup::handle_event` drain block applies it, the
    /// resulting `bar.tabs()` order MUST match the resulting `hosts`
    /// order one-for-one (`bar.tabs()[i].id == hosts[i].0` for every i).
    /// Two parallel vectors that drift out of sync would cause tab pill
    /// `i` to render content from a different host on the next frame —
    /// the exact bug the live `bar.reorder(from, to)` call was added to
    /// fix in Wave 1. This test exercises several from/to combinations
    /// (forward, backward, no-op, adjacent) so a future refactor that
    /// permutes only one side trips loudly. The production code's
    /// `debug_assert_eq!(self.bar.tabs().len(), self.hosts.len())`
    /// inside the reorder branch would also panic here if any case
    /// drifted, providing belt-and-braces coverage in debug builds.
    #[test]
    fn reorder_keeps_hosts_and_bar_tabs_aligned() {
        use crate::widget::{AppKind, TabDescriptor, TileContent};
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        // Helper: build a fresh 4-tab WindowGroup so we have enough
        // pills to exercise non-trivial permutations.
        fn make_group_4() -> WindowGroup {
            let tabs: Vec<(TabDescriptor, TileContent)> = (1..=4u64)
                .map(|i| {
                    (
                        TabDescriptor::new(i, format!("T{i}")),
                        TileContent::Stub {
                            kind: AppKind::Settings,
                            label: format!("s{i}"),
                        },
                    )
                })
                .collect();
            WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
        }

        // (from, to) cases:
        //  - forward small step (0 → 2)
        //  - backward step (3 → 1)
        //  - adjacent swap (1 → 2)
        //  - no-op (2 → 2)
        //  - extreme (0 → 3)
        let cases: &[(usize, usize)] = &[(0, 2), (3, 1), (1, 2), (2, 2), (0, 3)];

        for &(from, to) in cases {
            let mut g = make_group_4();

            // Build a layout pass — `handle_event` walks the cached
            // taffy children, and an empty children slice would short-
            // circuit before the drain block. Mirrors the maximize tests.
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(Style::default(), &[group_node])
                .unwrap();
            taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            // Precondition: bar.tabs and hosts are aligned 1:1 in id order.
            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "precondition: tab[{i}] id must equal hosts[{i}].0",
                );
            }

            // Publish the reorder request the same way `TabBar::handle_event`
            // does on a drag release within its own bar.
            g.bar.pending_reorder = Some((from, to));

            // Drive the WindowGroup drain block via a no-op cursor move.
            let moved = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
            };
            let mut state = automerge::AutoCommit::new();
            let root_id = automerge::ROOT;
            let mut handled = false;
            let mut focus_target = None;
            g.handle_event(
                &moved, 1.0, &winit::event::Modifiers::default(),
                &mut state, &root_id, &mut handled, &mut focus_target,
                group_node, &taffy,
            );

            // Postcondition: bar.tabs and hosts must still be aligned
            // one-for-one after the permutation. Both vectors must
            // have identical lengths and the i-th tab's id must equal
            // the i-th host's id for every i.
            assert_eq!(
                g.bar.tabs().len(),
                g.hosts.len(),
                "case ({from}, {to}): bar.tabs().len() must equal hosts.len() after reorder",
            );
            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "case ({from}, {to}): bar.tabs()[{i}].id must equal hosts[{i}].0 after reorder",
                );
            }
        }
    }
}
