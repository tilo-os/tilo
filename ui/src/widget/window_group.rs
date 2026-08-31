use crate::color_compat::ColorCompat;
use crate::icons;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::tabs::CLOSE_FADE_MS;
use crate::widget::{TabBar, TabDescriptor, TileContent, TileHost, Widget};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::Key;

/// Per-mille (not px) so a tier means the same fraction on every monitor.
/// Thirds tile the strip cleanly; `Wide` is the full usable width.
pub const WIDTH_NARROW_PERMILLE: u16 = 333;
pub const WIDTH_NORMAL_PERMILLE: u16 = 667;
pub const WIDTH_WIDE_PERMILLE: u16 = 1000;

/// [`WIDTH_FLOOR_PX`] backs this up on outputs too small for 15% to be
/// usable pixels.
pub const WIDTH_MIN_PERMILLE: u16 = 150;
pub const WIDTH_MAX_PERMILLE: u16 = 1000;

/// A pure fraction has no floor — 15% of an 800px strip is 120px, too
/// narrow for a tab pill or scrollbar — so resolved width clamps up to
/// this (and back down to the strip width if that's narrower still).
pub const WIDTH_FLOOR_PX: f64 = 240.0;

/// Only reached from a bare `ui` host with no output spans; the shell
/// always pushes real geometry first.
pub const DEFAULT_STRIP_USABLE_PX: f64 = 1280.0;

pub const RESIZE_HANDLE_PX: f64 = 6.0;

pub fn min_group_px(strip_usable_px: f64) -> f64 {
    let usable = strip_usable_px.max(1.0);
    (usable * f64::from(WIDTH_MIN_PERMILLE) / 1000.0)
        .max(WIDTH_FLOOR_PX)
        .min(usable)
}

pub fn max_group_px(strip_usable_px: f64) -> f64 {
    strip_usable_px.max(1.0)
}

/// Inverse of [`WindowWidth::resolve`]: how a pointer drag (pixels)
/// becomes a stored resolution-independent width.
pub fn permille_for_px(px: f64, strip_usable_px: f64) -> u16 {
    let usable = strip_usable_px.max(1.0);
    let raw = (px.max(0.0) / usable * 1000.0).round();
    let clamped = raw.clamp(f64::from(WIDTH_MIN_PERMILLE), f64::from(WIDTH_MAX_PERMILLE));
    // Safe: `clamped` is bounded to [WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE]
    // (150..=1000) above, well within u16 range and never negative.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [150.0, 1000.0] above, which fits losslessly in u16"
    )]
    {
        clamped as u16
    }
}

/// 420ms: mirrors the reference concept's spring timing
/// (`radial-workspace-switcher/src/App.tsx:407`).
const SPAWN_ANIM_MS: u64 = 420;
/// Mirrors the concept's `x: -20` initial, tuned to -16 to stay inside
/// the row's visible margin.
const SPAWN_ANIM_X_OFFSET: f64 = -16.0;
const SPAWN_ANIM_INITIAL_SCALE: f64 = 0.92;
/// Matches the tab-pill `CLOSE_FADE_MS` in `tabs.rs` so the group's
/// shrink chains onto the final pill fade.
const GROUP_CLOSE_FADE_MS: u64 = 180;

/// [`SPAWN_ANIM_MS`] in seconds, as an `f64` divisor for the eased-progress
/// calculation.
#[allow(
    clippy::cast_precision_loss,
    reason = "SPAWN_ANIM_MS is a small compile-time millisecond constant (420), far below f64's precision limit"
)]
fn spawn_anim_secs() -> f64 {
    SPAWN_ANIM_MS as f64 / 1000.0
}

/// Test-only helper: an [`Instant`] `ms` milliseconds in the past, for
/// backdating a timestamp instead of sleeping. Falls back to `Instant::now`
/// on the practically-unreachable case that the subtraction would underflow
/// the clock's epoch.
#[cfg(test)]
fn instant_ago_ms(ms: u64) -> Instant {
    Instant::now()
        .checked_sub(Duration::from_millis(ms))
        .unwrap_or_else(Instant::now)
}

/// Narrows a screen-space coordinate, length or eased animation factor
/// (`f64`, from taffy layout or CRDT storage) to the `f32` vello/taffy
/// consume. Real output geometry never approaches `f32`'s range (or an
/// eased factor its precision), so this never truncates in practice.
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen coordinates/lengths and eased [0,1] factors are always far below f32's range"
)]
fn px_f32(v: f64) -> f32 {
    v as f32
}

/// Mirrors the concept's `clientY <= 80` check (`WindowFrame.tsx:108-123`):
/// above this y the floating title/restore/close cluster paints, below it
/// they hide. Hit-testing is unaffected — only the paint is gated.
const MAXIMIZED_CHROME_REVEAL_Y: f64 = 80.0;

/// A width only becomes pixels through [`WindowWidth::resolve`], against
/// the strip's usable width, so the same stored value means the same
/// proportion of the screen regardless of output resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowWidth {
    Narrow,
    Normal,
    Wide,
    /// What a manual resize produces: measured in pixels, converted back
    /// to a fraction so it survives a restart and a differently-sized
    /// output.
    Fraction(u16),
}

impl WindowWidth {
    pub fn permille(&self) -> u16 {
        match self {
            WindowWidth::Narrow => WIDTH_NARROW_PERMILLE,
            WindowWidth::Normal => WIDTH_NORMAL_PERMILLE,
            WindowWidth::Wide => WIDTH_WIDE_PERMILLE,
            WindowWidth::Fraction(p) => (*p).clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE),
        }
    }

    pub fn fraction(&self) -> f64 {
        f64::from(self.permille()) / 1000.0
    }

    /// Clamped into `[min_group_px, max_group_px]` so a fraction can
    /// neither collapse a group below usability nor push it past the
    /// output's right edge.
    pub fn resolve(&self, strip_usable_px: f64) -> f64 {
        let usable = strip_usable_px.max(1.0);
        // Rounded: a Wayland client's geometry is integers, and a
        // fractional width would leave sub-pixel seams between groups.
        (usable * self.fraction())
            .round()
            .clamp(min_group_px(usable), max_group_px(usable))
    }

    /// Always a [`WindowWidth::Fraction`] — records the proportion the
    /// user landed on rather than a coincident tier, so a later tier
    /// retune doesn't silently move their window.
    pub fn from_px(px: f64, strip_usable_px: f64) -> Self {
        WindowWidth::Fraction(permille_for_px(px, strip_usable_px))
    }
}

// Each bool below is an independent UI flag (resize-hover, maximized,
// search-open, focus, ...), not a bitset encoding related states, and the
// struct is a `pub` field surface read throughout `shell` — folding them
// into a bitflags/enum would be an API-breaking redesign, not a mechanical
// lint fix.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent UI flags on a widely-shared pub struct; not a state bitset"
)]
pub struct WindowGroup {
    pub id: u64,
    bar: TabBar,
    hosts: Vec<(u64, TileHost)>,
    width: WindowWidth,
    /// Pushed down by the parent [`crate::widget::GroupRow`] before every
    /// `build_layout`; seeded to [`DEFAULT_STRIP_USABLE_PX`].
    strip_usable_px: f64,
    rect: Rect,
    /// `(press-time cursor x, press-time resolved width in px)`.
    resize_origin: Option<(f32, f64)>,
    pub hover_resize: bool,
    pub maximized: bool,
    pub search_active: bool,
    /// The active group of the active strip, set by the shell each frame.
    /// Consumed by the active [`TileHost`], which paints the signal on
    /// the window's own edge/shadow rather than the group drawing a ring
    /// around both the tab row and the card.
    pub focused: bool,
    restore_rect: Rect,
    restore_hover: bool,
    close_rect: Rect,
    close_hover: bool,
    last_cursor: Point,
    spawned_at: Option<Instant>,
    /// Set on any `draw` this group sat out because nothing in it had
    /// presented yet; drained on the first `draw` that does paint, which
    /// latches the arrival fade to the moment the window turns up rather
    /// than the moment its tab was created.
    held_back: bool,
    /// Tab-pill close fades, keyed by close-start `Instant` — the same
    /// stamp the bar snapshots into `closing_tabs` so both stay in
    /// lockstep. A side table (not a `TileHost` field) to keep
    /// `TileHost`'s shape stable. Swept once `elapsed > CLOSE_FADE_MS`.
    closing_hosts: HashMap<u64, Instant>,
    style_dirty: bool,
    /// The tab id the current taffy tile-node was built for.
    /// `build_layout` only builds the *active* tab's host, so on an
    /// active-tab switch the cached node still belongs to the old one —
    /// without a rebuild the new tab inherits a childless leaf and
    /// renders at a stale top-left rect. `take_structure_dirty` diffs
    /// this against the live active id to force that rebuild.
    built_active: Option<u64>,
    pending_maximize_change: Option<bool>,
    /// Per-mille, not px — a peer's output may be a different size.
    pending_width_change: Option<u16>,
    pending_search_open: Option<u64>,
    pending_new_tab: Option<u64>,
    pending_tab_reorder: Option<(usize, usize)>,
    /// `None` while alive; `Some` once the last tab-close drains with no
    /// tabs or in-flight closing hosts left. `GroupRow` prunes the group
    /// once `close_complete()` is true.
    closing_at: Option<Instant>,
}

/// Shared layout numbers for the maximized-chrome pill row, computed once in
/// [`WindowGroup::draw_maximized_chrome`] and threaded through its two child
/// painters — keeps both well under clippy's argument-count limit.
#[derive(Clone, Copy)]
struct ChromePillLayout {
    pad: f64,
    pill_h: f64,
    y0: f64,
}

impl WindowGroup {
    pub fn new(
        id: u64,
        tabs_with_content: Vec<(TabDescriptor, TileContent)>,
        active_id: u64,
        width: WindowWidth,
    ) -> Self {
        let descs: Vec<TabDescriptor> = tabs_with_content.iter().map(|(d, _)| d.clone()).collect();
        let hosts: Vec<(u64, TileHost)> = tabs_with_content
            .into_iter()
            .map(|(d, c)| (d.id, TileHost::new(c)))
            .collect();
        Self {
            id,
            bar: TabBar::new(&format!("g{id}"), descs, active_id),
            hosts,
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            rect: Rect::ZERO,
            resize_origin: None,
            hover_resize: false,
            maximized: false,
            search_active: false,
            focused: false,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            held_back: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            style_dirty: false,
            built_active: Some(active_id),
            pending_maximize_change: None,
            pending_width_change: None,
            pending_tab_reorder: None,
            pending_search_open: None,
            pending_new_tab: None,
        }
    }
    pub fn width(&self) -> WindowWidth {
        self.width
    }

    #[must_use]
    pub fn with_maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    pub fn set_focused(&mut self, focused: bool) -> bool {
        if self.focused == focused {
            return false;
        }
        self.focused = focused;
        true
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(Instant::now());
    }

    /// The same eased factor `draw` uses for alpha, scale and offset.
    pub fn spawn_anim_t(&self) -> f32 {
        let Some(started) = self.spawned_at else {
            return 1.0;
        };
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_millis(SPAWN_ANIM_MS) {
            return 1.0;
        }
        let raw = (elapsed.as_secs_f64() / spawn_anim_secs()).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - raw).powi(3);
        px_f32(eased)
    }

    pub fn start_closing(&mut self) {
        self.closing_at = Some(Instant::now());
    }

    pub fn is_closing(&self) -> bool {
        self.closing_at.is_some()
    }

    pub fn close_elapsed(&self) -> Option<Duration> {
        self.closing_at.map(|t| t.elapsed())
    }

    pub fn close_complete(&self) -> bool {
        self.closing_at
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(GROUP_CLOSE_FADE_MS))
    }

    pub fn should_show_maximized_chrome(&self, cursor_y: Option<f64>) -> bool {
        self.maximized && cursor_y.is_some_and(|y| y <= MAXIMIZED_CHROME_REVEAL_Y)
    }

    pub fn strip_usable_px(&self) -> f64 {
        self.strip_usable_px
    }

    /// Called by [`crate::widget::GroupRow::build_layout`] before each
    /// child builds its own node.
    pub fn set_strip_usable_px(&mut self, px: f64) {
        let px = px.max(1.0);
        if (self.strip_usable_px - px).abs() > 0.5 {
            self.strip_usable_px = px;
            self.style_dirty = true;
        }
    }

    pub fn resolved_width_px(&self) -> f64 {
        self.width.resolve(self.strip_usable_px)
    }

    pub fn begin_resize(&mut self, pointer_x: f32) {
        self.resize_origin = Some((pointer_x, self.resolved_width_px()));
    }
    pub fn update_resize(&mut self, pointer_x: f32) {
        if let Some((origin_x, origin_w)) = self.resize_origin {
            let delta = f64::from(pointer_x - origin_x);
            let usable = self.strip_usable_px;
            let clamped = (origin_w + delta).clamp(min_group_px(usable), max_group_px(usable));
            let next = WindowWidth::from_px(clamped, usable);
            if next != self.width {
                self.width = next;
                // Taffy bakes width as a fixed length; without this flag
                // the drag updates the field but nothing moves on screen.
                self.style_dirty = true;
            }
        }
    }
    pub fn end_resize(&mut self) {
        self.resize_origin = None;
        self.pending_width_change = Some(self.width.permille());
    }

    /// Does NOT touch `resize_origin` — the inter-group handle owns its
    /// own drag state; follow up with [`Self::stamp_pending_width_change`]
    /// on release.
    pub fn set_width(&mut self, w: WindowWidth) {
        let before = self.width;
        match w {
            WindowWidth::Fraction(p) => {
                self.width = WindowWidth::Fraction(p.clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE));
            }
            other => self.width = other,
        }
        if self.width != before {
            self.style_dirty = true;
        }
    }

    /// Converts to a fraction immediately so nothing downstream ever
    /// stores pixels.
    pub fn set_width_px(&mut self, px: f64) {
        self.set_width(WindowWidth::from_px(px, self.strip_usable_px));
    }

    pub fn stamp_pending_width_change(&mut self) {
        self.pending_width_change = Some(self.width.permille());
    }
    pub fn take_pending_width_change(&mut self) -> Option<u16> {
        self.pending_width_change.take()
    }

    pub fn take_pending_tab_reorder(&mut self) -> Option<(usize, usize)> {
        self.pending_tab_reorder.take()
    }

    pub fn take_pending_search_open(&mut self) -> Option<u64> {
        self.pending_search_open.take()
    }

    pub fn take_pending_new_tab(&mut self) -> Option<u64> {
        self.pending_new_tab.take()
    }

    pub fn new_tab_rect(&self) -> Rect {
        self.bar.new_tab_rect()
    }
    /// Not done inside [`Self::new`]: a group is also built by a bare `ui`
    /// host and by tests with no records at all, where an unbound tile
    /// keeps editing whatever document the parent pass carries.
    pub fn bind_tab_records(
        &mut self,
        store: &crate::record::RecordStore,
        record_for: impl Fn(u64) -> crate::record::RecordCheckout,
    ) {
        for (tab_id, host) in &mut self.hosts {
            host.bind_record(store.clone(), record_for(*tab_id));
        }
    }
    /// Deliberately not [`Widget::opening`], which is false as soon as
    /// one tab has drawn even if a tab behind it is still starting (and
    /// still needs animation frames).
    pub fn any_tile_opening(&self) -> bool {
        self.hosts.iter().any(|(_, h)| Widget::opening(h))
    }

    pub fn active_host_mut(&mut self) -> Option<&mut TileHost> {
        let id = self.bar.active_id();
        self.hosts
            .iter_mut()
            .find(|(i, _)| *i == id)
            .map(|(_, h)| h)
    }

    /// Read-only twin of [`Self::active_host_mut`] — avoids a `&mut`
    /// borrow that would conflict with the frame builder's own borrows.
    pub fn active_host(&self) -> Option<&TileHost> {
        let id = self.bar.active_id();
        self.hosts.iter().find(|(i, _)| *i == id).map(|(_, h)| h)
    }

    /// Returns `(tab_id, release_point)` if the user released a drag
    /// outside the bar's own rect. The parent widget hit-tests the
    /// release point against sibling bars to find a target.
    pub fn take_tab_detach(&mut self) -> Option<(u64, Point)> {
        self.bar.take_detach()
    }

    pub fn take_pending_maximize_change(&mut self) -> Option<bool> {
        self.pending_maximize_change.take()
    }

    /// Not gated by `cfg(any(test, debug_assertions))`: `ui`'s `cfg(test)`
    /// only applies when `ui` itself is under test, so the downstream
    /// shell's `--release` test build (`debug_assertions` off) would lose
    /// this method.
    pub fn publish_maximize_intent_for_tests(&mut self) {
        let active = self.bar.active_id();
        self.bar.pending_max = Some(active);
    }

    pub fn detach_tab(&mut self, tab_id: u64) -> Option<(TabDescriptor, TileHost)> {
        let host_idx = self.hosts.iter().position(|(i, _)| *i == tab_id)?;
        let (_, host) = self.hosts.remove(host_idx);
        let desc = self.bar.tabs().iter().find(|t| t.id == tab_id)?.clone();
        self.bar.remove_tab(tab_id);
        // Drop lingering close-fade state for the moved id, or a stale
        // entry keeps `needs_animation_frame` pumping until it ages out.
        self.closing_hosts.remove(&tab_id);
        self.bar.finalize_close(tab_id);
        Some((desc, host))
    }

    /// The sibling path — splitting into a *brand-new* group — plays the
    /// group-level fade instead, in `GroupRow::split_tab_into_new_group`,
    /// so the two never double up.
    pub fn insert_detached_tab(&mut self, idx: usize, desc: TabDescriptor, mut host: TileHost) {
        let id = desc.id;
        self.bar.insert_tab(idx, desc);
        let clamped = idx.min(self.hosts.len());
        host.start_spawn_animation();
        self.hosts.insert(clamped, (id, host));
    }

    pub fn from_detached(id: u64, desc: TabDescriptor, host: TileHost, width: WindowWidth) -> Self {
        let active_id = desc.id;
        let tab_id = desc.id;
        Self {
            id,
            bar: TabBar::new(&format!("g{id}"), vec![desc], active_id),
            hosts: vec![(tab_id, host)],
            width,
            strip_usable_px: DEFAULT_STRIP_USABLE_PX,
            rect: Rect::ZERO,
            resize_origin: None,
            hover_resize: false,
            maximized: false,
            search_active: false,
            focused: false,
            restore_rect: Rect::ZERO,
            restore_hover: false,
            close_rect: Rect::ZERO,
            close_hover: false,
            last_cursor: Point::ZERO,
            spawned_at: None,
            held_back: false,
            closing_hosts: HashMap::new(),
            closing_at: None,
            style_dirty: false,
            built_active: Some(active_id),
            pending_maximize_change: None,
            pending_width_change: None,
            pending_tab_reorder: None,
            pending_search_open: None,
            pending_new_tab: None,
        }
    }

    pub fn tab_count(&self) -> usize {
        self.hosts.len()
    }

    pub fn bar_rect(&self) -> Rect {
        self.bar.bar_rect()
    }

    pub fn tab_rects(&self) -> &[Rect] {
        self.bar.tab_rects()
    }

    /// True inside the bar rect but not on a tab pill and not in the
    /// right-edge resize band. Used by [`GroupRow`] to arbitrate a press
    /// into a group-reorder drag vs. a tab/resize gesture.
    pub fn bar_grab_hit(&self, p: Point) -> bool {
        if !self.bar_rect().contains(p) {
            return false;
        }
        if p.x >= self.rect.x1 - RESIZE_HANDLE_PX {
            return false;
        }
        let plus = self.bar.new_tab_rect();
        if plus.area() > 0.0 && plus.contains(p) {
            return false;
        }
        !self.tab_rects().iter().any(|r| r.contains(p))
    }

    pub fn drop_index_for(&self, release_point: Point) -> usize {
        self.bar.drop_index_for(release_point)
    }

    pub fn tabs(&self) -> &[TabDescriptor] {
        self.bar.tabs()
    }

    pub fn active_tab_id(&self) -> u64 {
        self.bar.active_id()
    }

    /// The bar's `active_id` is the authoritative copy (the shell's
    /// per-frame harvest mirrors it into the persisted tree), so a
    /// programmatic focus jump must land here, not only in the tree.
    pub fn set_active_tab(&mut self, id: u64) -> bool {
        if self.bar.active_id() == id || !self.bar.tabs().iter().any(|t| t.id == id) {
            return false;
        }
        self.bar.set_active(id);
        true
    }

    pub fn cycle_active_tab(&mut self, dir: i32) -> bool {
        let n = self.bar.tabs().len();
        if n < 2 || dir.signum() == 0 {
            return false;
        }
        let cur = self.bar.active_id();
        let Some(idx) = self.bar.tabs().iter().position(|t| t.id == cur) else {
            return false;
        };
        let new_idx = if dir.signum() > 0 {
            // Wrap forward: past the last tab goes to the first.
            idx.checked_add(1).filter(|&i| i < n).unwrap_or(0)
        } else {
            // Wrap backward: before the first tab goes to the last.
            idx.checked_sub(1).unwrap_or_else(|| n.saturating_sub(1))
        };
        let Some(new_id) = self.bar.tabs().get(new_idx).map(|t| t.id) else {
            return false;
        };
        self.bar.set_active(new_id);
        true
    }

    /// Exists because close was pointer-only: the `✕` only paints on an
    /// active/hovered/pressed pill, so there was no route for someone
    /// driving by keyboard.
    pub fn request_close_active_tab(&mut self) -> bool {
        let id = self.bar.active_id();
        self.request_close_tab(id)
    }

    pub fn take_tab_context_menu(&mut self) -> Option<(u64, Point)> {
        self.bar.take_context_menu()
    }

    /// Drains the bar's `pending_close` immediately rather than waiting
    /// for the next `handle_event`, since a menu pick arrives outside
    /// pointer dispatch and would otherwise leave the host un-faded
    /// until an unrelated event landed.
    pub fn request_close_tab(&mut self, tab_id: u64) -> bool {
        if !self.bar.start_closing(tab_id) {
            return false;
        }
        if let Some(close_id) = self.bar.take_close() {
            self.closing_hosts
                .entry(close_id)
                .or_insert_with(Instant::now);
        }
        true
    }

    pub fn drag_in_flight(&self) -> bool {
        self.bar.drag_in_flight()
    }

    /// A no-op while the bar has its own `pending_drag` in flight, so the
    /// source bar's own indicator stays put.
    pub fn set_external_drag_hover(&mut self, cursor: Option<Point>) {
        self.bar.set_external_drag_hover(cursor);
    }

    #[cfg(test)]
    pub(crate) fn bar_mut(&mut self) -> &mut crate::widget::TabBar {
        &mut self.bar
    }

    /// `shell/tests/` integration tests need this from outside the crate.
    #[cfg(any(test, debug_assertions))]
    pub fn bar_mut_for_tests(&mut self) -> &mut crate::widget::TabBar {
        &mut self.bar
    }

    #[cfg(any(test, debug_assertions))]
    pub fn tile_kind_labels_for_tests(&self) -> Vec<String> {
        self.hosts.iter().map(|(_, h)| h.kind_string()).collect()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn bar_for_tests(&self) -> &crate::widget::TabBar {
        &self.bar
    }

    #[cfg(test)]
    pub fn close_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::X
    }

    #[cfg(test)]
    pub fn restore_glyph_icon_for_tests() -> icons::Icon {
        icons::Icon::Minimize2
    }

    #[cfg(test)]
    pub fn host_count(&self) -> usize {
        self.hosts.len()
    }

    /// Detaching and re-inserting would do the same job plus three things
    /// nobody asked for: activate the tab, play the arrival fade, and
    /// move it to the caller's index.
    pub fn host_for_tab_mut(&mut self, tab_id: u64) -> Option<&mut TileHost> {
        self.hosts
            .iter_mut()
            .find(|(id, _)| *id == tab_id)
            .map(|(_, h)| h)
    }

    #[cfg(test)]
    pub fn has_host(&self, id: u64) -> bool {
        self.hosts.iter().any(|(i, _)| *i == id)
    }

    #[cfg(test)]
    pub fn force_expire_closing(&mut self, id: u64) {
        if self.closing_hosts.contains_key(&id) {
            self.closing_hosts
                .insert(id, instant_ago_ms(GROUP_CLOSE_FADE_MS.saturating_add(50)));
        }
    }

    #[cfg(test)]
    pub fn has_closing_host(&self, id: u64) -> bool {
        self.closing_hosts.contains_key(&id)
    }

    #[cfg(test)]
    pub(crate) fn set_closing_at_for_tests(&mut self, t: Instant) {
        self.closing_at = Some(t);
    }

    #[cfg(test)]
    pub(crate) fn set_close_hover_for_tests(&mut self, hover: bool) {
        self.close_hover = hover;
    }

    #[cfg(test)]
    pub(crate) fn maximized_close_chip_color_for_tests(
        &self,
        theme: &Theme,
    ) -> vello::peniko::Color {
        if self.close_hover {
            theme.tokens.colors.semantic.danger.with_alpha_factor(0.8)
        } else {
            vello::peniko::Color::from_rgba8(255, 255, 255, 38)
        }
    }

    /// On hover promoted to full white (concept: `hover:text-white`).
    #[cfg(test)]
    pub(crate) fn maximized_close_glyph_color_for_tests(
        &self,
        theme: &Theme,
    ) -> vello::peniko::Color {
        if self.close_hover {
            vello::peniko::Color::from_rgba8(255, 255, 255, 255)
        } else {
            theme.tokens.colors.text_primary
        }
    }

    /// The event kinds a group's own resize handle, maximized-chrome
    /// close/restore chips, and pin shortcut own outright — decided before
    /// the tab bar or active tile ever see the event. Returns whether the
    /// event was fully consumed (the caller should mark `handled` and stop
    /// dispatching further).
    fn handle_direct_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        m: winit::event::Modifiers,
        handle_rect: Rect,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                self.hover_resize = handle_rect.contains(pt);
                self.restore_hover = self.maximized && self.restore_rect.contains(pt);
                self.close_hover = self.maximized && self.close_rect.contains(pt);
                if self.resize_origin.is_some() {
                    self.update_resize(px_f32(pt.x));
                    return true;
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.maximized && self.close_rect.contains(self.last_cursor) => {
                // Route through the same deferred fade flow as the
                // per-tab × click (`bar.start_closing` → `closing_hosts`
                // → sweep) so the group shrinks coherently instead of
                // popping away. Exit maximize immediately so the floating
                // chrome stops painting over a tile it no longer owns.
                let active_id = self.bar.active_id();
                self.bar.start_closing(active_id);
                self.closing_hosts
                    .entry(active_id)
                    .or_insert_with(Instant::now);
                self.maximized = false;
                self.pending_maximize_change = Some(false);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.maximized && self.restore_rect.contains(self.last_cursor) => {
                // Handled here (not via bar.pending_max) so the click
                // never races with a tab selection under the pill's geometry.
                self.maximized = false;
                self.pending_maximize_change = Some(false);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.hover_resize => {
                // Anchor on the actual press point, not the group's right
                // edge — the hit zone is `RESIZE_HANDLE_PX` wide, so the
                // edge would jump the window on grab.
                self.begin_resize(px_f32(self.last_cursor.x));
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.resize_origin.is_some() => {
                self.end_resize();
                true
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } if m.state().control_key()
                && m.state().shift_key()
                && matches!(logical_key, Key::Character(s) if s.eq_ignore_ascii_case("p")) =>
            {
                // Routed here (not the bar's own handler) because keyboard
                // events arrive at the focused widget and the bar isn't
                // focusable.
                let active = self.bar.active_id();
                self.bar.toggle_pin(active);
                true
            }
            _ => false,
        }
    }

    /// Drops hosts whose close fade has fully elapsed (removal is deferred
    /// so the fade plays out with the tile still in place), then starts
    /// closing the whole group once nothing is left in it.
    fn sweep_expired_closing_hosts(&mut self) {
        // GROUP_CLOSE_FADE_MS mirrors CLOSE_FADE_MS (tabs.rs) as a whole
        // millisecond u64, avoiding an f32->u64 cast at every use site.
        let close_window = Duration::from_millis(GROUP_CLOSE_FADE_MS);
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

        // `bar.tabs().is_empty()` rather than just `hosts.is_empty()` —
        // the bar is the user-visible "empty" signal.
        if !self.is_closing()
            && self.bar.tabs().is_empty()
            && self.hosts.is_empty()
            && self.closing_hosts.is_empty()
        {
            self.start_closing();
        }
    }

    /// Drains the tab bar's one-shot signals (close/maximize/search/
    /// new-tab/reorder) queued by `TabBar::handle_event` and applies each
    /// to group state. Returns whether any signal fired, so the caller can
    /// mark the event handled and skip forwarding it to the active tile.
    fn drain_bar_signals(&mut self) -> bool {
        let mut handled = false;

        // Host stays in `self.hosts` for `CLOSE_FADE_MS`; the sweep above drops it.
        if let Some(close_id) = self.bar.take_close() {
            self.closing_hosts
                .entry(close_id)
                .or_insert_with(Instant::now);
            handled = true;
        }

        // Without `style_dirty` the cached taffy node keeps the
        // pre-toggle width and the click looks like a no-op.
        if let Some(_id) = self.bar.take_max() {
            self.maximized = !self.maximized;
            self.style_dirty = true;
            self.pending_maximize_change = Some(self.maximized);
            handled = true;
        }

        if let Some(id) = self.bar.take_search() {
            self.search_active = !self.search_active;
            self.pending_search_open = Some(id);
            handled = true;
        }

        // The group knows its own id; the shell knows what a new tab
        // should contain, so republish with the id and let it decide.
        if self.bar.take_new_tab().is_some() {
            self.pending_new_tab = Some(self.id);
            handled = true;
        }

        // `bar.tabs` and `hosts` are a strictly-aligned 1:1 mapping; a
        // refactor that permutes only one would desync pill `i` from
        // content `i` — the debug_assert documents that invariant.
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
            self.pending_tab_reorder = Some((from, to));
            handled = true;
        }

        handled
    }

    /// The alpha/x/y/scale the group's spawn-in or close-out fade applies
    /// this frame, matching whichever animation (if any) is in flight.
    /// `fade_y` is currently always 0, kept in the tuple in case a future
    /// close curve wants a settle.
    fn fade_transform_params(&self) -> (f32, f64, f64, f64) {
        if let Some(started) = self.closing_at {
            // No offset: the per-host curve already carries the y-drift.
            // Ratio of two `Duration`s taken via `as_secs_f64` rather than
            // an integer->float `as` cast on the millisecond counts.
            let raw = px_f32(
                (started.elapsed().as_secs_f64()
                    / Duration::from_millis(GROUP_CLOSE_FADE_MS).as_secs_f64())
                .clamp(0.0, 1.0),
            );
            let eased = 1.0 - (1.0 - raw).powi(3);
            let alpha = (1.0 - eased).max(0.0);
            let scale = f64::from(1.0 - 0.05 * eased);
            (alpha, 0.0, 0.0, scale)
        } else if let Some(started) = self.spawned_at {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(SPAWN_ANIM_MS) {
                (1.0, 0.0, 0.0, 1.0)
            } else {
                let raw = (elapsed.as_secs_f64() / spawn_anim_secs()).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - raw).powi(3);
                let scale = SPAWN_ANIM_INITIAL_SCALE + (1.0 - SPAWN_ANIM_INITIAL_SCALE) * eased;
                let x_off = SPAWN_ANIM_X_OFFSET * (1.0 - eased);
                (px_f32(eased), x_off, 0.0, scale)
            }
        } else {
            (1.0, 0.0, 0.0, 1.0)
        }
    }

    /// Paints each still-fading closed tab (opacity 1→0, scale 1→0.95,
    /// y 0→-20 over `CLOSE_FADE_MS`). The id list is snapshotted first to
    /// borrow `self.hosts` mutably without fighting the borrow checker
    /// against `self.closing_hosts`.
    fn draw_closing_hosts(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // GROUP_CLOSE_FADE_MS mirrors CLOSE_FADE_MS (tabs.rs) as a whole
        // millisecond u64, avoiding an f32->u64 cast at every use site.
        let close_window = Duration::from_millis(GROUP_CLOSE_FADE_MS);
        let closing_ids: Vec<(u64, Instant)> = self
            .closing_hosts
            .iter()
            .filter(|(_, t)| t.elapsed() < close_window)
            .map(|(id, t)| (*id, *t))
            .collect();
        for (id, started) in closing_ids {
            let elapsed = started.elapsed().as_secs_f64();
            let raw = (elapsed / (f64::from(CLOSE_FADE_MS) / 1000.0)).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - raw).powi(3);
            let alpha = px_f32(1.0 - eased);
            let scale = 1.0 + (-0.05) * eased; // 1.0 → 0.95
            let y_offset = -20.0 * eased;
            if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == id) {
                let rect = h.get_rect();
                // Matches the tile-spawn clip so shadows aren't chopped.
                let clip = Rect::new(
                    rect.x0 - 160.0,
                    rect.y0 - 160.0,
                    rect.x1 + 160.0,
                    rect.y1 + 160.0,
                );
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha.max(0.0),
                    transform,
                    &clip,
                );
                let centre = rect.center();
                // Affine composition is floating-point matrix multiply —
                // it cannot overflow or panic the way integer arithmetic
                // can.
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
                )]
                let close_transform = transform
                    * Affine::translate((0.0, y_offset))
                    * Affine::translate((centre.x, centre.y))
                    * Affine::scale(scale)
                    * Affine::translate((-centre.x, -centre.y));
                h.draw(scene, close_transform, tr, theme, presence);
                scene.pop_layer();
            }
        }
    }

    /// The two-bar resize grip on the group's trailing edge. The hit zone
    /// (`RESIZE_HANDLE_PX = 6`) is wider than the painted grip so the
    /// cursor catches it early.
    #[allow(
        clippy::similar_names,
        reason = "cx/cy is the standard center-x/center-y coordinate naming convention; \
                  distinguishing them further would reduce clarity"
    )]
    fn draw_resize_grip(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let grip_col = if self.hover_resize || self.resize_origin.is_some() {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.text_dim.with_alpha_factor(0.55)
        };
        let total_h = self.rect.height();
        let grip_h = (total_h * 0.25).clamp(24.0, 80.0);
        let grip_cx = self.rect.x1 - 3.0;
        let grip_cy = self.rect.center().y;
        let bar = |dx: f64| {
            vello::kurbo::RoundedRect::from_rect(
                Rect::new(
                    grip_cx + dx - 1.0,
                    grip_cy - grip_h / 2.0,
                    grip_cx + dx + 1.0,
                    grip_cy + grip_h / 2.0,
                ),
                1.0,
            )
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            grip_col,
            None,
            &bar(-2.0),
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            grip_col,
            None,
            &bar(2.0),
        );
    }

    /// The floating `[app title]  …  [Restore][×]` pill drawn over a
    /// maximized group's content, matching the concept layout. Also
    /// updates `restore_rect`/`close_rect` for hit-testing, which keeps
    /// running regardless of the cursor band — only the paint is gated by
    /// [`Self::should_show_maximized_chrome`].
    fn draw_maximized_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.should_show_maximized_chrome(Some(self.last_cursor.y)) {
            return;
        }
        // `[app title]  …  [Restore][×]`, matching the concept layout.
        let pad = 6.0_f64;
        let layout = ChromePillLayout {
            pad,
            pill_h: 22.0_f64,
            y0: self.rect.y0 + pad,
        };
        self.draw_maximized_title_chip(scene, transform, tr, theme, layout);
        self.draw_maximized_buttons(scene, transform, tr, theme, layout);
    }

    /// The `[app title]` chip at the left of the maximized-chrome pill row.
    fn draw_maximized_title_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        layout: ChromePillLayout,
    ) {
        let ChromePillLayout { pad, pill_h, y0 } = layout;
        let active_title = self
            .bar
            .tabs()
            .iter()
            .find(|t| t.id == self.bar.active_id())
            .map(|t| t.title.clone())
            .unwrap_or_default();
        let title_size = theme.tokens.typography.scale.xs;
        let (tw, th) = tr.measure(&active_title, title_size, None);
        let title_pill_w = (f64::from(tw) + 24.0).max(80.0);
        let title_pill = Rect::new(
            self.rect.x0 + pad,
            y0,
            self.rect.x0 + pad + title_pill_w,
            y0 + pill_h,
        );
        crate::surface::shadow(
            scene,
            transform,
            title_pill,
            title_pill.height() / 2.0,
            &theme.tokens.elevation.e2,
        );
        let (chip_fill, chip_border) = crate::surface::chip_colors(theme, false);
        crate::surface::pill(scene, transform, title_pill, chip_fill, chip_border, None);
        // Affine composition is floating-point matrix multiply — it
        // cannot overflow or panic the way integer arithmetic can.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let title_text_transform = transform
            * vello::kurbo::Affine::translate((
                title_pill.center().x - f64::from(tw) / 2.0,
                title_pill.center().y - f64::from(th) / 2.0,
            ));
        tr.draw_text(
            scene,
            &active_title,
            title_size,
            theme.tokens.colors.text_primary,
            title_text_transform,
            None,
        );
    }

    /// The `[Restore][×]` button pair at the right of the maximized-chrome
    /// pill row. Also updates `restore_rect`/`close_rect` for hit-testing,
    /// which keeps running regardless of the cursor band — only the paint
    /// is gated by the caller.
    fn draw_maximized_buttons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        layout: ChromePillLayout,
    ) {
        let ChromePillLayout { pad, pill_h, y0 } = layout;
        let title_size = theme.tokens.typography.scale.xs;
        let close_w = 22.0_f64;
        let close_x0 = self.rect.x1 - close_w - pad;
        self.close_rect = Rect::new(close_x0, y0, close_x0 + close_w, y0 + pill_h);
        let restore_w = 76.0_f64;
        let restore_x0 = close_x0 - restore_w - 4.0;
        self.restore_rect = Rect::new(restore_x0, y0, restore_x0 + restore_w, y0 + pill_h);

        crate::surface::shadow(
            scene,
            transform,
            self.restore_rect,
            self.restore_rect.height() / 2.0,
            &theme.tokens.elevation.e2,
        );
        let (restore_fill, restore_border) = crate::surface::chip_colors(theme, self.restore_hover);
        crate::surface::pill(
            scene,
            transform,
            self.restore_rect,
            restore_fill,
            restore_border,
            None,
        );
        // Text label kept (for screen readers/i18n) and offset past
        // a 14px icon slot for the lucide `Minimize2` glyph.
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
        let lx = self.restore_rect.x0 + 28.0;
        let ly = self.restore_rect.center().y - f64::from(lh) / 2.0;
        // Affine composition is floating-point matrix multiply — it
        // cannot overflow or panic the way integer arithmetic can.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let label_transform = transform * vello::kurbo::Affine::translate((lx, ly));
        tr.draw_text(
            scene,
            label,
            title_size,
            theme.tokens.colors.text_primary,
            label_transform,
            None,
        );

        self.draw_maximized_close_button(scene, transform, theme);
    }

    /// The `[×]` close button at the far right of the maximized-chrome
    /// pill row, painted into `self.close_rect` (already computed by
    /// [`Self::draw_maximized_buttons`]).
    fn draw_maximized_close_button(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        // Concept: `hover:bg-red-500/80` (danger token at 80% alpha).
        crate::surface::shadow(
            scene,
            transform,
            self.close_rect,
            self.close_rect.height() / 2.0,
            &theme.tokens.elevation.e2,
        );
        let close_fill = if self.close_hover {
            theme.tokens.colors.semantic.danger.with_alpha_factor(0.8)
        } else {
            crate::surface::chip_colors(theme, false).0
        };
        let close_border = if self.close_hover {
            None
        } else {
            crate::surface::chip_colors(theme, false).1
        };
        crate::surface::pill(
            scene,
            transform,
            self.close_rect,
            close_fill,
            close_border,
            None,
        );
        // On hover the glyph promotes to full white (`hover:text-white`).
        let glyph_color = if self.close_hover {
            vello::peniko::Color::from_rgba8(255, 255, 255, 255)
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
}

impl Widget for WindowGroup {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        if let Some(h) = self.active_host_mut() {
            h.sync(s, p);
        }
    }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        self.active_host_mut().is_some_and(|h| h.update(s, p))
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let bar_node = self.bar.build_layout(taffy, theme, node_map);
        let active_id = self.bar.active_id();
        // See `built_active`'s field doc: forces a rebuild when the
        // active tab switches.
        self.built_active = Some(active_id);
        let tile_node = if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.build_layout(taffy, theme, node_map)
        } else {
            taffy.leaf_node(Style::default())
        };
        // Niri-style infinite strip: `flex_grow: 0` keeps a group at its
        // declared `WindowWidth` regardless of sibling count or slack —
        // placing a new app never resizes siblings, the strip extends
        // past the viewport instead. Maximized overrides to `flex_grow:
        // 1e6` below, mirroring the concept's fixed `width: 100vw`
        // maximized window (`WindowFrame.tsx:188-200`).
        let mut style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::length(px_f32(self.resolved_width_px())),
                height: Dimension::percent(1.0),
            },
            flex_grow: 0.0,
            gap: taffy::Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        };
        if self.maximized {
            style.size.width = Dimension::auto();
            style.flex_grow = 1.0e6;
            style.flex_basis = Dimension::length(0.0);
            // Fullscreen: the tab strip neither paints (see `draw`) nor
            // reserves height; the floating pill draws over the content.
            let mut bar_style = taffy.style(bar_node).cloned().unwrap_or_default();
            bar_style.display = taffy::Display::None;
            let _ = taffy.set_style(bar_node, bar_style);
        }
        let node = taffy.parent_node(style, &[bar_node, tile_node]);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let children = taffy.children(node).unwrap_or_default();
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let Some(&bar_child) = children.first() else {
            return;
        };
        self.bar.apply_layout(taffy, bar_child, new_origin);
        let active_id = self.bar.active_id();
        if let (Some((_, h)), Some(&tile_child)) = (
            self.hosts.iter_mut().find(|(i, _)| *i == active_id),
            children.get(1),
        ) {
            h.apply_layout(taffy, tile_child, new_origin);
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
        // Tolerance rather than exact float equality: this only gates a
        // no-op-transform fast path, so a value merely close to identity
        // is close enough to skip composing it.
        const FADE_EPS: f64 = 1e-9;
        // An indicator that flashes in and out before an app has time to
        // draw would just be a flicker, so nothing paints until either
        // something has drawn or the wait is long enough to say so.
        if !Widget::presented(self) && !Widget::opening(self) {
            self.held_back = true;
            return;
        }
        // The shell latches the spawn fade when it splices the group in,
        // before the app has drawn — without restarting it here the fade
        // would already have run its course against an invisible group.
        if std::mem::take(&mut self.held_back) {
            self.spawned_at = Some(Instant::now());
        }
        // Close checked first (mutually exclusive with spawn in practice):
        // `opacity 0→1, scale 0.92→1, x -16→0` over `SPAWN_ANIM_MS`
        // cubic-out; close mirrors it (α 1→0, scale 1→0.95). The whole
        // paint composes under one alpha+translate+scale layer so hit
        // tests stay aligned with what's on screen mid-fade.
        let (fade_alpha, fade_x, fade_y, fade_scale) = self.fade_transform_params();
        let spawn_active = fade_alpha < 1.0;
        if spawn_active {
            // 16px clip margin so outer strokes (glow rings, focus
            // outlines) aren't clipped mid-fade.
            let clip = Rect::new(
                self.rect.x0 - 16.0,
                self.rect.y0 - 16.0,
                self.rect.x1 + 16.0,
                self.rect.y1 + 16.0,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                fade_alpha,
                transform,
                &clip,
            );
        }
        // `fade_y` is currently always 0, kept in the tuple in case a
        // future close curve wants a settle.
        // Affine composition is floating-point matrix multiply — it
        // cannot overflow or panic the way integer arithmetic can.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
        )]
        let transform = if fade_x.abs() > FADE_EPS
            || fade_y.abs() > FADE_EPS
            || (fade_scale - 1.0).abs() > FADE_EPS
        {
            let cx = self.rect.center().x;
            let cy = self.rect.center().y;
            transform
                * Affine::translate((cx + fade_x, cy + fade_y))
                * Affine::scale(fade_scale)
                * Affine::translate((-cx, -cy))
        } else {
            transform
        };
        // The bar's hit rects stay live either way — only the paint is
        // gated (maximized swaps in the floating pill further down).
        if !self.maximized {
            self.bar.draw(scene, transform, tr, theme, presence);
        }
        let active_id = self.bar.active_id();
        // A tile holding a place for an app that has not arrived takes
        // its name from the pill above it, so the two never disagree.
        let opening_name = self
            .bar
            .tabs()
            .iter()
            .find(|t| t.id == active_id)
            .map(|t| t.title.clone());
        // Scoping search *is* aiming the keyboard at this group, so it
        // lights the same edge/shadow signal as keyboard focus.
        let focused = self.focused || self.search_active;
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            if let Some(name) = &opening_name {
                h.set_opening_name(name);
            }
            h.set_focused(focused);
            h.draw(scene, transform, tr, theme, presence);
        }
        self.draw_closing_hosts(scene, transform, tr, theme, presence);
        self.draw_resize_grip(scene, transform, theme);

        // No outline rings around `self.rect` for drag/search/maximize:
        // per `ui/HIG.md` §9.4 the accent means *live*, not *chosen*.
        if self.maximized {
            self.draw_maximized_chrome(scene, transform, tr, theme);
        } else {
            self.restore_rect = Rect::ZERO;
            self.close_rect = Rect::ZERO;
        }

        // Matches the `spawn_active` push above.
        if spawn_active {
            scene.pop_layer();
        }
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        m: &winit::event::Modifiers,
        s: &mut AutoCommit,
        p: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let children = taffy.children(node_id).unwrap_or_default();
        let bar_child = children.first().copied();
        let tile_child = children.get(1).copied();
        let handle_rect = Rect::new(
            self.rect.x1 - RESIZE_HANDLE_PX,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        if self.handle_direct_input(event, scale_factor, *m, handle_rect) {
            *handled = true;
            return;
        }
        if let Some(bar_child) = bar_child {
            self.bar.handle_event(
                event,
                scale_factor,
                m,
                s,
                p,
                handled,
                focus_target,
                bar_child,
                taffy,
            );
        }

        self.sweep_expired_closing_hosts();
        if self.drain_bar_signals() {
            *handled = true;
        }

        if *handled {
            return;
        }
        let active_id = self.bar.active_id();
        if let (Some((_, h)), Some(tile_child)) = (
            self.hosts.iter_mut().find(|(i, _)| *i == active_id),
            tile_child,
        ) {
            h.handle_event(
                event,
                scale_factor,
                m,
                s,
                p,
                handled,
                focus_target,
                tile_child,
                taffy,
            );
        }
    }
    fn name(&self) -> &'static str {
        "WindowGroup"
    }
    /// **Any**, not all: a group with one live window and a second still
    /// starting must not blink out because a sibling tab hasn't drawn.
    fn presented(&self) -> bool {
        if self.hosts.is_empty() {
            return true;
        }
        // `fold`, not `any`: asking `h.presented()` is what records a
        // tab's window as drawn, so short-circuiting on the first `true`
        // would strand every tab behind it un-asked, each going on
        // painting its placeholder over a window on screen for minutes.
        self.hosts
            .iter()
            .fold(false, |seen, (_, h)| h.presented() || seen)
    }
    /// Only true while *nothing* has presented, so a group with one live
    /// window never turns into a placeholder because a sibling is starting.
    fn opening(&self) -> bool {
        !Widget::presented(self) && self.hosts.iter().any(|(_, h)| h.opening())
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    /// Mirrors the inter-group [`crate::widget::ResizeHandle`] so both
    /// affordances give the same feedback.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let band = Rect::new(
            self.rect.x1 - RESIZE_HANDLE_PX,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        if self.resize_origin.is_some() || band.contains(point) {
            Some(winit::window::CursorIcon::EwResize)
        } else {
            None
        }
    }
    fn wants_pointer_capture(&self) -> bool {
        self.resize_origin.is_some()
    }
    fn take_structure_dirty(&mut self) -> bool {
        // `fold`, not `any`: every child's flag must be drained even if
        // an earlier one already reported dirty.
        let bar = self.bar.take_structure_dirty();
        #[allow(clippy::unnecessary_fold)] // see comment above: must not short-circuit
        let hosts = self
            .hosts
            .iter_mut()
            .map(|(_, h)| h.take_structure_dirty())
            .fold(false, |acc, d| acc || d);
        let style = std::mem::take(&mut self.style_dirty);
        let active_switched = self.built_active != Some(self.bar.active_id());
        bar || hosts || style || active_switched
    }
    fn needs_animation_frame(&self) -> bool {
        // `WindowGroup::children()` uses the default (empty) impl, so
        // ancestors wouldn't otherwise see the bar's drag/spawn signals.
        let spawn_in_flight = self
            .spawned_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(SPAWN_ANIM_MS));
        // GROUP_CLOSE_FADE_MS mirrors CLOSE_FADE_MS (tabs.rs) as a whole
        // millisecond u64, avoiding an f32->u64 cast at every use site.
        let close_window = Duration::from_millis(GROUP_CLOSE_FADE_MS);
        let close_in_flight = self
            .closing_hosts
            .values()
            .any(|t| t.elapsed() < close_window);
        let group_close_in_flight = self
            .closing_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(GROUP_CLOSE_FADE_MS));
        // Only the *active* tab is laid out and drawn, so only its signal
        // counts — otherwise a background terminal's PTY-poll would pin
        // the whole shell at 60fps/100% CPU forever.
        let active_id = self.bar.active_id();
        let host = self
            .hosts
            .iter()
            .find(|(i, _)| *i == active_id)
            .is_some_and(|(_, h)| h.needs_animation_frame());
        spawn_in_flight
            || close_in_flight
            || group_close_in_flight
            || self.bar.needs_animation_frame()
            || host
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
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "a".into(),
                },
            ),
            (
                TabDescriptor::new(2, "B"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "b".into(),
                },
            ),
        ];
        WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
    }

    struct Starting {
        presented: bool,
        opening: bool,
    }

    impl Widget for Starting {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            _m: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _th: &crate::theme::Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _event: &winit::event::WindowEvent,
            _scale: f64,
            _m: &winit::event::Modifiers,
            _state: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
            _handled: &mut bool,
            _focus: &mut Option<NodeId>,
            _node: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "Starting"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn presented(&self) -> bool {
            self.presented
        }
        fn opening(&self) -> bool {
            self.opening
        }
    }

    impl crate::widget::app_widget::AppWidget for Starting {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Starting"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("S")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    fn group_opening(title: &str, presented: bool, opening: bool) -> WindowGroup {
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
            TabDescriptor::new(1, title),
            TileContent::App(Box::new(Starting { presented, opening })),
        )];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
        g.rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        g
    }

    /// A full layout pass matters — a group with no tab rects paints
    /// nothing for unrelated reasons, which would make the "nothing on
    /// screen" case pass for free.
    fn painted_anything(g: &mut WindowGroup) -> bool {
        use taffy::prelude::TaffyMaxContent;
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        g.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );
        !scene.encoding().path_data.is_empty()
    }

    #[test]
    fn the_paint_probe_reports_an_untouched_scene_as_empty() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let scene = Scene::new();
        let _ = (&theme, &mut tr);
        assert!(
            scene.encoding().path_data.is_empty(),
            "an untouched scene must read as nothing painted, or the \
             held-back case passes for free",
        );
    }

    #[test]
    fn a_group_paints_nothing_while_the_wait_is_still_too_short_to_be_one() {
        let mut g = group_opening("Files", false, false);
        assert!(
            !painted_anything(&mut g),
            "a window frame went up for an app that has not drawn and has \
             not been waiting long enough to say so — which is a card \
             flashing on and off in front of an app that opened fast",
        );
    }

    #[test]
    fn a_group_holds_the_window_place_once_the_wait_is_worth_naming() {
        let mut g = group_opening("Files", false, true);
        assert!(
            Widget::opening(&g),
            "the group must report that it is holding a place",
        );
        assert!(
            painted_anything(&mut g),
            "the desktop stayed empty for an app the person asked for and \
             is still waiting on — the whole defect this exists to fix",
        );
    }

    /// A card reading `dev.qos.file-explorer` is the failure this guards:
    /// the person asked for Files, and a package id is not an answer.
    #[test]
    fn the_card_takes_the_apps_name_from_its_tab() {
        let mut g = group_opening("Files", false, true);
        let _ = painted_anything(&mut g);
        let (_, host) = g.hosts.first().expect("the group has its one tab");
        assert_eq!(
            host.opening_name_for_test(),
            "Files",
            "the card must say what the tab says; anything else is two \
             names for one window",
        );
    }

    #[test]
    fn a_group_with_a_live_window_is_never_holding_a_place() {
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "Files"),
                TileContent::App(Box::new(Starting {
                    presented: true,
                    opening: false,
                })),
            ),
            (
                TabDescriptor::new(2, "Notes"),
                TileContent::App(Box::new(Starting {
                    presented: false,
                    opening: true,
                })),
            ),
        ];
        let g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
        assert!(
            Widget::presented(&g),
            "one of its tabs is a window on screen"
        );
        assert!(
            !Widget::opening(&g),
            "a group holding a window the person is using must never be \
             replaced by a card about the tab beside it",
        );
    }

    /// Mirrors `ForeignSurfaceWidget`: being *asked* `presented()` is what
    /// records a window as drawn (`ForeignFrameStore::mark_presented`).
    struct Latching {
        drawn: bool,
        latched: std::rc::Rc<std::cell::Cell<bool>>,
        asked: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl Latching {
        fn new(
            drawn: bool,
        ) -> (
            Self,
            std::rc::Rc<std::cell::Cell<bool>>,
            std::rc::Rc<std::cell::Cell<u32>>,
        ) {
            let latched = std::rc::Rc::new(std::cell::Cell::new(false));
            let asked = std::rc::Rc::new(std::cell::Cell::new(0));
            (
                Self {
                    drawn,
                    latched: latched.clone(),
                    asked: asked.clone(),
                },
                latched,
                asked,
            )
        }
    }

    impl Widget for Latching {
        fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            _m: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _tr: &mut TextRenderer,
            _th: &crate::theme::Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _event: &winit::event::WindowEvent,
            _scale: f64,
            _m: &winit::event::Modifiers,
            _state: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
            _handled: &mut bool,
            _focus: &mut Option<NodeId>,
            _node: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "Latching"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
        fn presented(&self) -> bool {
            self.asked.set(self.asked.get().saturating_add(1));
            if self.drawn {
                self.latched.set(true);
            }
            self.latched.get()
        }
        fn opening(&self) -> bool {
            !self.latched.get()
        }
    }

    impl crate::widget::app_widget::AppWidget for Latching {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Latching"
        }
        fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
            crate::widget::app_widget::IconDescriptor::Glyph("L")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }

    fn group_of_two(
        first_drawn: bool,
        second_drawn: bool,
    ) -> (
        WindowGroup,
        std::rc::Rc<std::cell::Cell<bool>>,
        std::rc::Rc<std::cell::Cell<u32>>,
    ) {
        let (a, _, _) = Latching::new(first_drawn);
        let (b, latched, asked) = Latching::new(second_drawn);
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "Notes"),
                TileContent::App(Box::new(a)),
            ),
            (
                TabDescriptor::new(2, "Files"),
                TileContent::App(Box::new(b)),
            ),
        ];
        (
            WindowGroup::new(10, tabs, 2, WindowWidth::Normal),
            latched,
            asked,
        )
    }

    #[test]
    fn every_tab_in_a_group_is_asked_whether_its_window_has_drawn() {
        let (g, latched, asked) = group_of_two(true, true);

        assert!(Widget::presented(&g), "both of its tabs have drawn");
        assert!(
            asked.get() > 0,
            "the second tab was never asked whether its window had drawn, so \
             nothing recorded that it had",
        );
        assert!(
            latched.get(),
            "a window that has drawn must end up marked as having drawn",
        );
        assert!(
            !Widget::opening(&g),
            "and must not still be holding its own place",
        );
    }

    #[test]
    fn a_drawn_window_behind_another_tab_retires_its_opening_card() {
        let (g, _latched, _asked) = group_of_two(true, true);
        let _ = Widget::presented(&g);
        let host = g
            .hosts
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, h)| h)
            .expect("the group holds the tab it was built with");
        assert!(
            !Widget::opening(host),
            "the card holding this window's place must be gone: the window is \
             drawn, populated, and the one on screen",
        );
    }

    #[test]
    fn a_window_that_has_not_drawn_still_holds_its_place() {
        let (g, latched, _asked) = group_of_two(true, false);
        let _ = Widget::presented(&g);
        assert!(
            !latched.get(),
            "asking is not answering — a window that has not drawn must not be \
             recorded as having drawn",
        );
        let host = g
            .hosts
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, h)| h)
            .expect("the group holds the tab it was built with");
        assert!(Widget::opening(host), "its place is still being held");
    }

    // Common `handle_event` shape shared by every dispatch in the
    // maximize-click tests below (fixed scale factor, default modifiers);
    // returns whether the event was claimed.
    fn dispatch(
        g: &mut WindowGroup,
        event: &WindowEvent,
        state: &mut AutoCommit,
        root_id: &ObjId,
        focus_target: &mut Option<NodeId>,
        group_node: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        g.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            state,
            root_id,
            &mut handled,
            focus_target,
            group_node,
            taffy,
        );
        handled
    }

    /// Build a `WindowGroup`, lay it out, and hover-open a tab pill's
    /// maximize chip. Returns everything a maximize-click test needs plus
    /// `moved`/`press` events aimed at the now-visible chip. Split out of
    /// `maximize_click_on_tab_pill_toggles_window_group_maximized` to keep
    /// that under the line cap.
    #[allow(
        clippy::type_complexity,
        reason = "test fixture bundling everything one maximize-click test needs; a named struct would only move the same fields, not reduce them"
    )]
    fn open_maximize_chip() -> (
        WindowGroup,
        TaffyTree<()>,
        NodeId,
        AutoCommit,
        ObjId,
        Option<NodeId>,
        WindowEvent,
        WindowEvent,
    ) {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::{ElementState, MouseButton};

        let mut g = make_group();
        assert!(!g.maximized, "fresh group must start non-maximized");

        // `max_rects` must be populated by `apply_layout` before the
        // click; reduce-motion so the pill's reveal lands in one pass.
        let theme = Theme::dark().with_reduce_motion(true);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // A pill only carries a maximize chip once hovered open.
        let pill = g.bar.tab_rects()[0];
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut focus_target = None;
        dispatch(
            &mut g,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(pill.center().x, pill.center().y),
            },
            &mut state,
            &root_id,
            &mut focus_target,
            group_node,
            &taffy,
        );
        g.apply_layout(&taffy, group_node, Point::ZERO);

        let max_rect = g.bar.max_rects()[0];
        assert!(
            max_rect.area() > 0.0,
            "an opened pill must carry a maximize chip to click",
        );
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

        (
            g,
            taffy,
            group_node,
            state,
            root_id,
            focus_target,
            moved,
            press,
        )
    }

    #[test]
    fn maximize_click_on_tab_pill_toggles_window_group_maximized() {
        let (mut g, taffy, group_node, mut state, root_id, mut focus_target, moved, press) =
            open_maximize_chip();

        // Seed last_cursor on the icon — the bar's press arm uses the
        // cached cursor for its fresh hit-test.
        dispatch(
            &mut g,
            &moved,
            &mut state,
            &root_id,
            &mut focus_target,
            group_node,
            &taffy,
        );
        let handled = dispatch(
            &mut g,
            &press,
            &mut state,
            &root_id,
            &mut focus_target,
            group_node,
            &taffy,
        );

        assert!(handled, "press on the maximize icon must be claimed");
        assert!(
            g.maximized,
            "maximize click on a tab pill must drive WindowGroup::maximized true",
        );

        dispatch(
            &mut g,
            &moved,
            &mut state,
            &root_id,
            &mut focus_target,
            group_node,
            &taffy,
        );
        dispatch(
            &mut g,
            &press,
            &mut state,
            &root_id,
            &mut focus_target,
            group_node,
            &taffy,
        );
        assert!(
            !g.maximized,
            "second maximize click must restore (toggle off)",
        );
    }

    #[test]
    fn maximize_toggle_marks_structure_dirty_for_layout_rebuild() {
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        assert!(!g.take_structure_dirty(), "fresh group is not dirty");

        // Simulate the bar publishing a maximize request, same path the
        // per-tab click hits.
        g.bar.pending_max = Some(1);
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
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

        assert!(g.maximized, "maximize drain must flip the flag");
        assert!(
            g.take_structure_dirty(),
            "maximize toggle must report structure dirty so App re-runs build_layout",
        );
        // Drained — second poll within the same frame is clean.
        assert!(!g.take_structure_dirty(), "structure_dirty drains on poll");
    }

    #[test]
    fn active_tab_switch_marks_structure_dirty_for_layout_rebuild() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        assert!(!g.take_structure_dirty(), "fresh group is not dirty");

        // Real build so `built_active` is stamped for the active tab.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);
        assert!(
            !g.take_structure_dirty(),
            "a built group with no tab switch stays clean",
        );

        // Same as the wheel-cycle / click path: mutates `active_id` only.
        assert!(g.cycle_active_tab(1), "cycle must change the active tab");
        assert!(
            g.take_structure_dirty(),
            "active-tab switch must report structure dirty so the new tile gets a node",
        );

        // Once the host rebuilds for the new active tab, it drains clean.
        let mut taffy2 = TaffyTree::<()>::new();
        let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = g.build_layout(&mut taffy2, &theme, &mut node_map2);
        assert!(
            !g.take_structure_dirty(),
            "structure_dirty drains once the host rebuilds for the new active tab",
        );
    }

    #[test]
    fn take_pending_maximize_change_drains_on_tab_bar_toggle() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        assert_eq!(
            g.take_pending_maximize_change(),
            None,
            "no pending change before any click",
        );

        // Prime `pending_max` and run a no-op cursor-move through
        // `handle_event` to drive the drain block.
        g.bar.pending_max = Some(1);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut handled = false;
        let mut focus_target = None;
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

        assert!(g.maximized, "drain must flip maximized true");
        assert_eq!(
            g.take_pending_maximize_change(),
            Some(true),
            "first poll after the click reports the new value",
        );
        assert_eq!(
            g.take_pending_maximize_change(),
            None,
            "take-pattern: drained on first poll, second poll is None",
        );

        // A second toggle reports the flipped value — the take-pattern
        // is one-shot but re-armable, so the shell sees every flip.
        g.bar.pending_max = Some(1);
        let mut handled = false;
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
        assert!(!g.maximized, "second drain must flip back to false");
        assert_eq!(
            g.take_pending_maximize_change(),
            Some(false),
            "the second click reports the new (false) value once",
        );
    }

    #[test]
    fn resize_drag_changes_laid_out_width() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        // Helper: (re)build the taffy tree from the group's current state and
        // run a full layout pass, returning the group's laid-out width.
        fn layout_width(g: &mut WindowGroup, theme: &Theme) -> f64 {
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, node, Point::ZERO);
            g.get_rect().width()
        }

        let theme = Theme::dark();
        let mut g = make_group();

        // 1200 px strip: tiers land on whole hundreds (Narrow 400 /
        // Normal 800 / Wide 1200).
        g.set_strip_usable_px(1200.0);
        let _ = g.take_structure_dirty();

        let before = layout_width(&mut g, &theme);
        assert_eq!(
            before, 800.0,
            "make_group() starts at WindowWidth::Normal — two thirds of a \
             1200 px strip",
        );

        // Drag the right-edge handle +200 px.
        g.begin_resize(800.0);
        g.update_resize(1000.0);
        assert_eq!(
            g.resolved_width_px(),
            1000.0,
            "drag updates the width field",
        );
        assert_eq!(
            g.width(),
            WindowWidth::Fraction(833),
            "the drag is stored as the fraction it landed on (1000/1200), \
             not as a pixel count",
        );

        assert!(
            g.take_structure_dirty(),
            "a width change must report structure-dirty so the taffy node's \
             baked length is rebuilt",
        );

        let after = layout_width(&mut g, &theme);
        assert_eq!(
            after, 1000.0,
            "after the rebuild the group must actually be laid out 200 px wider",
        );
    }

    #[test]
    fn maximized_group_gives_all_its_height_to_the_app() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        // Lay a group out and report (group_height, tile_height).
        fn measure(g: &mut WindowGroup, theme: &Theme) -> (f64, f64) {
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, theme, &mut node_map);
            let root = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(900.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, node, Point::ZERO);
            let tile = taffy.children(node).unwrap()[1];
            let tile_h = f64::from(taffy.layout(tile).copied().unwrap_or_default().size.height);
            (g.get_rect().height(), tile_h)
        }

        let theme = Theme::dark();
        let mut g = make_group();
        let (group_h, tile_h) = measure(&mut g, &theme);
        assert!(
            tile_h < group_h,
            "while tiled, the tab strip takes height off the app: \
             group={group_h} tile={tile_h}",
        );

        g.maximized = true;
        let (group_h, tile_h) = measure(&mut g, &theme);
        assert_eq!(
            tile_h, group_h,
            "maximized: the app must get the group's full height (no reserved \
             tab-strip band)",
        );
    }

    #[test]
    fn take_pending_tab_reorder_drains_after_a_reorder() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let theme = Theme::dark();
        let mut g = make_group();
        assert_eq!(
            g.take_pending_tab_reorder(),
            None,
            "nothing pending initially"
        );

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();

        // Publish a reorder the same way the bar does on a drag release, then
        // pump the group's drain block via a harmless event.
        g.bar_mut_for_tests().pending_reorder = Some((0, 1));
        let mut handled = false;
        let mut focus = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        g.handle_event(
            &moved,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        assert_eq!(
            g.take_pending_tab_reorder(),
            Some((0, 1)),
            "a harvested reorder publishes (from, to) for the shell exactly once",
        );
        assert_eq!(
            g.take_pending_tab_reorder(),
            None,
            "take-pattern: drained on the first poll",
        );
    }

    /// Drives the real event path (`CursorMoved` → `Pressed` → `CursorMoved`)
    /// since calling `begin_resize` directly would hide the bug, which is
    /// in its caller.
    #[test]
    fn resize_grab_anchors_on_press_position_not_group_edge() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::{ElementState, MouseButton};

        let theme = Theme::dark();
        let mut g = make_group();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        // Definite-size root: `percent(1.0)` height resolves to 0 against
        // an indefinite parent, which would leave the resize handle's
        // rect empty and unhittable.
        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        let rect = g.get_rect();
        assert!(
            rect.height() > 0.0,
            "group must have a real height to hit-test"
        );
        let right_edge = rect.x1;
        let press_y = rect.center().y;
        let start_w = g.resolved_width_px();
        // Press 3 px inside the handle zone (still a valid grab) rather than
        // exactly on the edge.
        let press_x = right_edge - 3.0;

        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut focus = None;

        let mut dispatch = |g: &mut WindowGroup, ev: &WindowEvent, taffy: &TaffyTree<()>| {
            let mut handled = false;
            g.handle_event(
                ev,
                1.0,
                &mods,
                &mut state,
                &root_id,
                &mut handled,
                &mut focus,
                node,
                taffy,
            );
        };

        let move_to = |x: f64| WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, press_y),
        };
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        dispatch(&mut g, &move_to(press_x), &taffy);
        dispatch(&mut g, &press, &taffy);
        // Drag exactly +100 px from the press position.
        dispatch(&mut g, &move_to(press_x + 100.0), &taffy);

        assert_eq!(
            g.resolved_width_px(),
            start_w + 100.0,
            "a 100 px drag from the press point must widen the group by exactly \
             100 px (no jump from anchoring on the group edge)",
        );
    }

    #[test]
    fn cursor_shape_is_ew_resize_over_right_edge_band() {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let theme = Theme::dark();
        let mut g = make_group();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        let rect = g.get_rect();
        assert!(
            rect.height() > 0.0,
            "group must have a real height to hit-test"
        );
        let cy = rect.center().y;

        // 3 px inside the right edge → inside the RESIZE_HANDLE_PX band.
        assert_eq!(
            Widget::cursor_shape(&g, Point::new(rect.x1 - 3.0, cy)),
            Some(winit::window::CursorIcon::EwResize),
            "pointer in the right-edge resize band requests an east-west resize cursor",
        );
        // Well inside the group, away from the edge → no cursor claim.
        assert_eq!(
            Widget::cursor_shape(&g, Point::new(rect.center().x, cy)),
            None,
            "pointer over the group body (not the edge band) defers",
        );

        // In-flight resize drag holds the cursor even off the band —
        // pointer capture keeps the gesture alive.
        g.begin_resize(px_f32(rect.x1));
        assert_eq!(
            Widget::cursor_shape(&g, Point::new(rect.center().x, cy)),
            Some(winit::window::CursorIcon::EwResize),
            "an in-flight resize drag holds the east-west cursor across the group",
        );
    }

    #[test]
    fn take_pending_width_change_drains_after_resize_gesture() {
        let mut g = make_group();
        // 1200 px strip: `Normal` (two thirds) resolves to 800.
        g.set_strip_usable_px(1200.0);
        assert_eq!(g.resolved_width_px(), 800.0);
        assert_eq!(
            g.take_pending_width_change(),
            None,
            "no pending width change before any drag",
        );

        // Press at the right edge (pointer x = 800 = current right edge).
        g.begin_resize(800.0);
        // Drag +200 px — inside the strip's bounds, so the new width is 1000.
        g.update_resize(1000.0);
        assert_eq!(
            g.resolved_width_px(),
            1000.0,
            "drag updates width in-memory"
        );
        // Mid-drag must not publish a pending change — only end_resize does,
        // so the shell harvests once per gesture, not once per pixel.
        assert_eq!(
            g.take_pending_width_change(),
            None,
            "mid-drag must not publish a pending change",
        );

        g.end_resize();
        assert_eq!(
            g.take_pending_width_change(),
            Some(833),
            "first poll after release reports the new width as a fraction of \
             the strip (1000/1200), so persisting it is display-independent",
        );
        assert_eq!(
            g.take_pending_width_change(),
            None,
            "take-pattern: drained on first poll, second poll is None",
        );

        // Clamping: a drag past the strip's right edge pins at the full
        // usable width, not at some fixed pixel ceiling.
        g.begin_resize(1000.0);
        g.update_resize(50_000.0);
        assert_eq!(
            g.resolved_width_px(),
            max_group_px(1200.0),
            "drag past the strip's edge clamps to the strip's usable width",
        );
        g.end_resize();
        assert_eq!(g.take_pending_width_change(), Some(WIDTH_MAX_PERMILLE));

        // Clamping the other way: the floor is a fraction of the strip too,
        // backstopped by the absolute minimum.
        g.begin_resize(0.0);
        g.update_resize(-50_000.0);
        assert_eq!(
            g.resolved_width_px(),
            min_group_px(1200.0),
            "drag past the strip's minimum clamps to the lower bound",
        );

        // Same stored width, wider display: the *fraction* persists, so a
        // wider strip gets the same share, not the same pixel count.
        let mut wide = make_group();
        wide.set_strip_usable_px(3840.0);
        assert_eq!(
            wide.width(),
            g_normal_width(),
            "fixture precondition: both groups start on the same tier",
        );
        assert_eq!(
            wide.resolved_width_px(),
            2561.0,
            "667/1000 of a 3840 px strip — the same share the 1200 px strip \
             gave at 800 px, to the 1 px that per-mille quantisation costs \
             at this size",
        );
    }

    /// The tier `make_group()` seeds. Named so the cross-display assertion
    /// above reads against the fixture rather than restating a literal.
    fn g_normal_width() -> WindowWidth {
        WindowWidth::Normal
    }

    #[test]
    fn search_active_toggles_when_bar_publishes_pending_search() {
        // Click-to-toggle: seeding `pending_search` and re-draining flips
        // the flag; doing it again flips it back.
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
        assert!(
            !g.search_active,
            "second drain should flip search_active off"
        );
    }

    #[test]
    fn start_spawn_animation_records_timestamp_and_pumps_frames() {
        let mut g = make_group();
        assert!(g.spawned_at.is_none(), "fresh group has no spawn timestamp");
        g.start_spawn_animation();
        assert!(
            g.spawned_at.is_some(),
            "start_spawn_animation records the start"
        );
        assert!(
            g.needs_animation_frame(),
            "in-flight spawn fade must pump the run loop",
        );
    }

    #[test]
    fn spawn_fade_window_clears_after_duration() {
        let mut g = make_group();
        g.spawned_at = Some(instant_ago_ms(SPAWN_ANIM_MS.saturating_add(16)));
        assert!(
            !g.needs_animation_frame(),
            "after the spawn window elapses, the fade no longer demands frames",
        );
    }

    #[test]
    fn spawn_animation_advances_eased_progress_over_time() {
        let mut g = make_group();
        // No spawn in flight — accessor pins to 1.0.
        assert_eq!(
            g.spawn_anim_t(),
            1.0,
            "idle group reports finished progress"
        );

        // First frame: elapsed ~0, so eased progress is near zero.
        g.start_spawn_animation();
        let t_start = g.spawn_anim_t();
        assert!(
            t_start < 0.05,
            "first-frame spawn progress must be near zero, got {t_start}",
        );

        // Roughly midway: cubic-out at raw=0.5 yields 1 - 0.5^3 = 0.875,
        // so we expect well past 0.5. Backdate the timestamp instead of
        // sleeping so the test doesn't waste real time.
        g.spawned_at = Some(instant_ago_ms(SPAWN_ANIM_MS / 2));
        let t_mid = g.spawn_anim_t();
        assert!(
            t_mid > 0.5 && t_mid < 1.0,
            "mid-spawn progress must be past half but not yet complete, got {t_mid}",
        );

        // Past the duration: pins to 1.0.
        g.spawned_at = Some(instant_ago_ms(SPAWN_ANIM_MS.saturating_add(16)));
        assert_eq!(
            g.spawn_anim_t(),
            1.0,
            "after the spawn window elapses, progress pins to 1.0",
        );
    }

    /// Locked to a 350-450ms band so a future pass can't silently shorten
    /// it back to an imperceptible tuning.
    #[test]
    fn spawn_animation_duration_in_visible_band() {
        assert!(
            (350..=450).contains(&SPAWN_ANIM_MS),
            "spawn duration must stay in the 350-450 ms readability band, got {SPAWN_ANIM_MS}",
        );
    }

    #[test]
    fn window_group_close_glyph_is_lucide_x() {
        assert_eq!(WindowGroup::close_glyph_icon_for_tests(), icons::Icon::X,);
    }

    #[test]
    fn window_group_restore_pill_uses_minimize2() {
        assert_eq!(
            WindowGroup::restore_glyph_icon_for_tests(),
            icons::Icon::Minimize2,
        );
    }

    #[test]
    fn insert_detached_tab_plays_tile_spawn_animation() {
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

    /// Its id sits in `closing_hosts` — a stale entry whose timer never
    /// expires once the host is moved — so `needs_animation_frame` would
    /// otherwise keep pumping redraws for a host that's gone.
    #[test]
    fn detach_tab_clears_closing_state_for_moved_tab() {
        let mut g = make_group();
        // Seed `closing_hosts` directly with tab id 1 to simulate the
        // leak: still present in `bar.tabs` and `hosts`, but the
        // side-table erroneously carries a fade timestamp.
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

    #[test]
    fn closing_host_stays_until_fade_complete() {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        // Layout pass so the bar has authoritative pill/close rects —
        // `start_closing` snapshots those into `closing_tabs`.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // Latch via the bar API directly rather than simulating a full
        // press/click sequence.
        g.bar_mut().start_closing(2);

        // CursorMoved off-rect so no hover handlers fire and the event
        // reaches the close-drain block at the bottom of `handle_event`.
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
        assert!(g.has_host(2), "host stays in `hosts` for the fade duration");
        let host_count_during = g.host_count();

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

    /// `GroupRow` relies on `is_closing()` → `close_complete()` to
    /// sequence the prune, so if `closing_at` never flips the group
    /// stays around as an empty shell forever.
    #[test]
    fn closing_last_tab_kicks_off_group_close() {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        // Single-tab group so "close this tab" truly empties the group.
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
            TabDescriptor::new(1, "solo"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "a".into(),
            },
        )];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

        // Full layout pass so the bar has authoritative pill rects.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // Simulates pressing the pill's × on the sole tab.
        g.bar_mut().start_closing(1);

        // First handle_event drains `pending_close` into `closing_hosts`;
        // the group is NOT yet closing while a host close is in flight.
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
            g.has_closing_host(1),
            "precondition: host-level close latched",
        );
        assert!(
            !g.is_closing(),
            "group close must NOT fire while a host-level close is still in flight",
        );

        // Expire the host-level close; the next handle_event sweeps it
        // away, discovers the group is empty, and latches the group close.
        g.force_expire_closing(1);
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
            g.is_closing(),
            "after the sole tab's fade sweeps out, the group must latch its own close fade",
        );
    }

    #[test]
    fn group_close_complete_true_after_fade_duration() {
        let mut g = make_group();
        g.set_closing_at_for_tests(instant_ago_ms(GROUP_CLOSE_FADE_MS.saturating_add(20)));
        assert!(
            g.close_complete(),
            "close_complete must flip true after the fade window elapses",
        );
        g.set_closing_at_for_tests(Instant::now());
        assert!(
            !g.close_complete(),
            "a just-latched close must not report complete",
        );
    }

    /// Guards against a panic or a collapsed rect mid-fade.
    #[test]
    fn group_still_paints_during_close_fade() {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);
        let pre_rect = g.get_rect();

        g.start_closing();

        // The fade scales about the centre via an Affine; the logical
        // `self.rect` used for hit-tests stays put so drop-targets and
        // focus bookkeeping don't shift under the user mid-animation.
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
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

    #[test]
    fn group_needs_animation_frame_while_closing() {
        let mut g = make_group();
        assert!(
            !g.needs_animation_frame(),
            "precondition: idle group is quiet"
        );
        g.start_closing();
        assert!(
            g.needs_animation_frame(),
            "group-level close fade must pump frames until close_complete",
        );
        g.set_closing_at_for_tests(instant_ago_ms(GROUP_CLOSE_FADE_MS.saturating_add(20)));
        assert!(
            !g.needs_animation_frame(),
            "after the fade window elapses, frames are no longer required",
        );
    }

    #[test]
    fn maximized_close_uses_deferred_flow() {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::{ElementState, MouseButton};

        // Single-tab group so the sweep has a clear "last tab just
        // left" trigger for the group-level close fade.
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
            TabDescriptor::new(1, "solo"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "a".into(),
            },
        )];
        let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        // `close_rect` normally gets populated in `draw` while maximized;
        // tests don't paint so we seed it directly.
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
            &press,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            group_node,
            &taffy,
        );

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
        assert!(
            !g.is_closing(),
            "group close must NOT fire while a host-level close is still in flight",
        );

        g.force_expire_closing(1);
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        let mut handled2 = false;
        g.handle_event(
            &moved,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled2,
            &mut focus_target,
            group_node,
            &taffy,
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

    #[test]
    fn window_group_needs_animation_frame_during_close() {
        let mut g = make_group();
        assert!(
            !g.needs_animation_frame(),
            "precondition: idle group doesn't demand frames",
        );
        // Seed `closing_hosts` directly to simulate what `handle_event`
        // does after draining a `pending_close`, bypassing the full event
        // drive.
        g.closing_hosts.insert(1, Instant::now());
        assert!(
            g.needs_animation_frame(),
            "a fresh closing host must pump the run loop",
        );
        g.closing_hosts
            .insert(1, instant_ago_ms(GROUP_CLOSE_FADE_MS.saturating_add(20)));
        assert!(
            !g.needs_animation_frame(),
            "after the fade window elapses the group goes idle again",
        );
    }

    /// Concept parity: `hover:bg-red-500/80` — `semantic.danger` at 80%
    /// alpha, glyph promoted to full white.
    #[test]
    fn maximized_close_chip_uses_danger_token_on_hover() {
        let mut g = make_group();
        g.maximized = true;
        let theme = Theme::dark();

        g.set_close_hover_for_tests(false);
        let idle_chip = g.maximized_close_chip_color_for_tests(&theme);
        assert_eq!(
            (idle_chip.r(), idle_chip.g(), idle_chip.b(), idle_chip.a()),
            (255, 255, 255, 38),
            "idle maximized close chip stays as the chrome white tint",
        );
        let idle_glyph = g.maximized_close_glyph_color_for_tests(&theme);
        let expected = theme.tokens.colors.text_primary;
        assert_eq!(
            (
                idle_glyph.r(),
                idle_glyph.g(),
                idle_glyph.b(),
                idle_glyph.a()
            ),
            (expected.r(), expected.g(), expected.b(), expected.a()),
            "idle glyph uses theme text_primary",
        );

        g.set_close_hover_for_tests(true);
        let hover_chip = g.maximized_close_chip_color_for_tests(&theme);
        let expected_chip = theme.tokens.colors.semantic.danger.with_alpha_factor(0.8);
        assert_eq!(
            (
                hover_chip.r(),
                hover_chip.g(),
                hover_chip.b(),
                hover_chip.a()
            ),
            (
                expected_chip.r(),
                expected_chip.g(),
                expected_chip.b(),
                expected_chip.a()
            ),
            "hover maximized close chip paints the theme danger token at 80% alpha",
        );
        let hover_glyph = g.maximized_close_glyph_color_for_tests(&theme);
        assert_eq!(
            (
                hover_glyph.r(),
                hover_glyph.g(),
                hover_glyph.b(),
                hover_glyph.a()
            ),
            (255, 255, 255, 255),
            "hover glyph promotes to opaque white (concept: hover:text-white)",
        );
    }

    /// Concept parity: `WindowFrame.tsx:108-123`'s `clientY <= 80` gate.
    #[test]
    fn maximized_chrome_visible_when_cursor_y_le_80() {
        let mut g = make_group();
        g.maximized = true;
        assert!(g.should_show_maximized_chrome(Some(40.0)));
        assert!(g.should_show_maximized_chrome(Some(80.0)));
        assert!(!g.should_show_maximized_chrome(Some(81.0)));
        assert!(!g.should_show_maximized_chrome(Some(400.0)));
        // No cursor observed yet is treated as "below the band".
        assert!(!g.should_show_maximized_chrome(None));
    }

    #[test]
    fn maximized_chrome_always_hidden_when_not_maximized() {
        let g = make_group();
        assert!(!g.maximized, "fresh group is not maximized");
        assert!(!g.should_show_maximized_chrome(Some(20.0)));
        assert!(!g.should_show_maximized_chrome(Some(80.0)));
        assert!(!g.should_show_maximized_chrome(None));
    }

    /// Parallel-vec invariant: drift between `bar.tabs()` and `hosts`
    /// would render tab pill `i`'s content from a different host.
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

            // `handle_event` walks the cached taffy children, so a real
            // layout pass is needed before the drain block runs.
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "precondition: tab[{i}] id must equal hosts[{i}].0",
                );
            }

            // Same as `TabBar::handle_event` on a drag release.
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

    #[test]
    fn ctrl_shift_p_toggles_pin_on_active_tab() {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::{KeyEvent, Modifiers};
        use winit::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

        // winit 0.30 keeps `KeyEventExtra` `pub(crate)`, so a `KeyEvent`
        // can't be built through a struct literal — zero-init instead.
        fn key_press(logical_key: Key) -> WindowEvent {
            let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = logical_key;
            ev.physical_key =
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
            ev.location = KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            }
        }

        let mut g = make_group();
        let active = g.bar.active_id();
        assert!(
            !g.bar.is_pinned(active),
            "fresh group: active tab is unpinned"
        );

        let mods = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
        let event = key_press(Key::Character(SmolStr::new_inline("p")));

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut handled = false;
        let mut focus_target = None;
        g.handle_event(
            &event,
            1.0,
            &mods,
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            group_node,
            &taffy,
        );
        assert!(handled, "Ctrl+Shift+P press must be claimed");
        assert!(
            g.bar.is_pinned(active),
            "first Ctrl+Shift+P must pin the active tab",
        );

        // Second press toggles back off.
        let mut handled = false;
        g.handle_event(
            &event,
            1.0,
            &mods,
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            group_node,
            &taffy,
        );
        assert!(handled, "second Ctrl+Shift+P press is also claimed");
        assert!(
            !g.bar.is_pinned(active),
            "second Ctrl+Shift+P must unpin the active tab",
        );

        let plain_mods = Modifiers::default();
        let mut handled = false;
        g.handle_event(
            &event,
            1.0,
            &plain_mods,
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            group_node,
            &taffy,
        );
        assert!(
            !g.bar.is_pinned(active),
            "plain `P` (no Ctrl+Shift) must not toggle pin state",
        );
    }
}
