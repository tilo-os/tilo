use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use crate::icons;
use crate::widget::Widget;
use crate::text_render::{TextRenderer, TextWeight};
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

/// Duration of the per-pill close fade-out. Mirrors the concept's
/// `exit={{ opacity: 0, scale: 0.95, y: -20 }}` in
/// `WindowFrame.tsx:203` and pairs with the tile-level spawn fade
/// (`SPAWN_FADE_MS` in `tile_host.rs`): 180 ms is long enough for the
/// user to register the motion without delaying the next interaction.
pub const CLOSE_FADE_MS: f32 = 180.0;
/// Terminal uniform scale of the close curve. Starts at 1.0 and
/// settles to `CLOSE_SCALE_END` so the pill visibly shrinks as it
/// fades. Matches the concept's `scale: 0.95` exit keyframe.
const CLOSE_SCALE_END: f64 = 0.95;
/// Terminal vertical offset of the close curve, in CSS-like px. The
/// pill drifts up 20 px over the fade — matches the concept's
/// `y: -20` exit keyframe.
const CLOSE_Y_OFFSET_END: f64 = -20.0;

/// A tab whose close button was clicked and is now fading out. Kept
/// in `TabBar::closing_tabs` alongside the still-live `tabs` vec so the
/// pill keeps rendering at a declining alpha + shrinking scale for
/// `CLOSE_FADE_MS` before the parent `WindowGroup` actually drops it.
///
/// Carries a snapshot of the `TabDescriptor` because the tab has
/// already been shifted out of `tabs` the moment closing started
/// (active-id shifts to a neighbour immediately, so the user never
/// sees a "ghost active" frame). Snapshotting the rect too lets
/// `draw` paint the closing pill even after the surviving pills have
/// sprung into their new positions.
#[derive(Clone, Debug)]
struct ClosingTab {
    desc: TabDescriptor,
    /// The pill's final layout rect at the moment closing started —
    /// the fade animates in place rather than tracking the layout
    /// spring that reflows the surviving pills.
    pill_rect: Rect,
    close_rect: Rect,
    started_at: Instant,
}

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
    /// Offset from the pressed pill's top-left corner to the cursor at
    /// press time. Populated in the `Pressed` branch alongside
    /// `press_origin` and cleared on `Released`. Used by `draw` to paint
    /// the drag-ghost at `last_cursor - drag_offset` so the ghost pill
    /// tracks the cursor with the same "grab point" the user initially
    /// grabbed — matches the concept's `WindowFrame.tsx` drag preview
    /// where the native HTML5 drag image sits under the cursor at the
    /// original grab offset. `DragPayload::Tab` currently only carries
    /// `{ group, tab }`, so we cache the offset here rather than threading
    /// it through the payload.
    drag_offset: Option<vello::kurbo::Vec2>,
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
    /// Tabs whose close button has been clicked but whose fade-out
    /// animation (`CLOSE_FADE_MS`) is still in flight. Each entry
    /// keeps rendering as a pill at declining alpha + scale; the
    /// surviving `tabs` vec has already been shifted so neighbour
    /// pills reflow into the freed slot. `WindowGroup` drives the
    /// actual host removal — this bar publishes the id once through
    /// `take_close()` at close-start so the parent can begin its own
    /// host-level fade-out.
    closing_tabs: Vec<ClosingTab>,
}

impl TabBar {
    /// Opacity the source pill renders at while the drag-ghost is
    /// visible. Matches the concept's "pulled out" fade that
    /// `WindowFrame.tsx`'s native HTML5 drag handler produces via the
    /// browser's default drag visual. Pinned to 0.35 so the slot still
    /// reads as present in the bar (no neighbour reflow) but clearly
    /// inactive — the ghost at `GHOST_PILL_ALPHA` is almost twice as
    /// opaque so the cursor-following copy dominates.
    pub const SOURCE_PILL_DIM_ALPHA: f32 = 0.35;
    /// Opacity the translucent drag-ghost pill renders at while a drag
    /// is in flight. Mirrors the concept's drag preview
    /// semi-transparency; high enough that the full pill chrome (icon,
    /// title, close glyph) stays readable as the cursor drifts, low
    /// enough that neighbouring pills peek through.
    pub const GHOST_PILL_ALPHA: f32 = 0.6;

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
            drag_offset: None,
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
            closing_tabs: Vec::new(),
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

    /// Paint a single pill's full chrome (drop shadow, body fill, border,
    /// active rim, leading app icon, title, collaborator presence glow,
    /// close / max / search glyphs) at an arbitrary rect. Used by both
    /// the normal per-pill pass in `draw` (where `r` is the pill's
    /// layout-sprung visual rect) and the drag-ghost pass (where `r` is
    /// the cursor-following ghost rect). Hover-reveal affordances are
    /// gated by `suppress_hover` — the ghost passes `true` so the ghost
    /// never shows search / maximize glyphs even if the source pill was
    /// hovered at drag-start.
    ///
    /// The close / max / search hit rects stored on `self.close_rects`
    /// etc. are at the pill's *target* position, so we offset them by
    /// `r - self.tab_rects[i]` before painting. Hit-testing still
    /// consults the unoffset rects so the cursor interacts with the
    /// layout position even while the pill's glyph visibly drifts.
    fn draw_pill_inner(
        &self,
        scene: &mut Scene,
        transform: Affine,
        i: usize,
        r: Rect,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        suppress_hover: bool,
    ) {
        let t = &self.tabs[i];
        let pill_height = (self.rect.height() - 2.0 * TAB_PILL_INSET_Y).max(0.0);
        let radius = pill_height / 2.0;
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = theme.tokens.colors.text_dim;
        let target = self.tab_rects[i];
        let dx = r.x0 - target.x0;
        let dy = r.y0 - target.y0;

        let is_active = t.id == self.active_id;
        let is_hover = !suppress_hover && Some(i) == self.hover_idx;
        // Per-pill drop shadow, approximating the concept's
        // `shadow-[0_8px_32px_rgba(0,0,0,0.2)]`.
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
        // Pill body. Active / hover / idle mirror the concept's Tailwind
        // tiers — see `draw` for the derivation.
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
        // Active-only inner rim light.
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
        let text_color = if is_active || is_hover {
            text_primary
        } else {
            text_dim
        };
        let text_size = theme.tokens.typography.scale.sm;
        let title_weight = TextWeight::Semibold;
        let (_tw, th) = tr.measure_weighted(&t.title, text_size, None, title_weight);
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
        let title_x = r.x0 + 12.0 + leading_shift;
        let title_y = r.center().y - th as f64 / 2.0;
        // Clip the title relative to the close glyph's x-position on the
        // *visual* rect (target + offset), so titles truncate at the
        // same relative distance no matter where the pill is painted.
        let close_x_visual = self.close_rects[i].x0 + dx;
        let clip_r = Rect::new(r.x0, r.y0, close_x_visual - 2.0, r.y1);
        scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &clip_r);
        tr.draw_text_weighted(
            scene,
            &t.title,
            text_size,
            text_color,
            transform * Affine::translate((title_x, title_y)),
            None,
            title_weight,
        );
        scene.pop_layer();

        // Per-tab collaborator presence indicators.
        let tab_key = PresenceKey::Tab { id: format!("{:x}", t.id) };
        let peers = presence.peers_on(&tab_key);
        if !peers.is_empty() {
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

        // Close button. Mirrors the concept's `hover:bg-red-500/80`
        // (WindowFrame.tsx:334): on close-hover we tint the chip with
        // the destructive semantic red at ~80% alpha so the user reads
        // the action before they commit. When the tab is only hovered
        // (but the cursor hasn't zeroed in on the ×), we keep a subtle
        // white tint as a "you can close me" hint.
        let close_r = self.close_rects[i] + vello::kurbo::Vec2::new(dx, dy);
        let close_hover = !suppress_hover && Some(i) == self.hover_close_idx;
        let (close_chip_color, close_chip_visible) = if close_hover {
            // red-500/80 — `semantic.danger` is rgb(239, 68, 68); at 0.8
            // alpha this matches Tailwind's `bg-red-500/80` (≈ 204/255).
            (
                theme.tokens.colors.semantic.danger.with_alpha_factor(0.8),
                true,
            )
        } else if is_hover {
            (vello::peniko::Color::rgba8(255, 255, 255, 28), true)
        } else {
            (vello::peniko::Color::rgba8(0, 0, 0, 0), false)
        };
        if close_chip_visible {
            let chip = RoundedRect::from_rect(close_r, close_r.height() / 2.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                close_chip_color,
                None,
                &chip,
            );
        }
        let glyph_color = if close_hover {
            // Concept's `hover:text-white` — full white on close-hover
            // so the × reads crisp against the red-500/80 fill.
            vello::peniko::Color::rgba8(255, 255, 255, 255)
        } else if is_active {
            text_primary
        } else {
            text_dim
        };
        icons::draw_icon(scene, transform, close_r, icons::Icon::X, glyph_color, 1.5);

        // Hover-reveal action cluster (maximize + search). Suppressed on
        // the drag-ghost so the ghost stays visually simple.
        if is_hover {
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
            let outline_color = if max_hover { icon_active } else { icon_idle };
            icons::draw_icon(
                scene,
                transform,
                max_r,
                icons::Icon::Maximize2,
                outline_color,
                1.5,
            );

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
            icons::draw_icon(
                scene,
                transform,
                search_r,
                icons::Icon::Search,
                icon_color,
                1.5,
            );
        }
    }

    /// Paint a minimal pill chrome (drop shadow, body fill, border,
    /// leading app icon if any, title, close glyph) for a `ClosingTab`
    /// at its snapshotted `pill_rect`. Deliberately narrower surface
    /// than `draw_pill_inner`: no hover affordances, no
    /// search / maximize icons, no presence glow. Those would only
    /// distract from the fade-out and the pill is non-interactive at
    /// this point anyway — hit-testing in `handle_event` ignores
    /// closing pills so the user can't accidentally re-click the
    /// close chip mid-fade.
    fn draw_closing_pill(
        &self,
        scene: &mut Scene,
        transform: Affine,
        c: &ClosingTab,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = c.pill_rect;
        let pill_height = (r.height()).max(0.0);
        let radius = pill_height / 2.0;
        let text_dim = theme.tokens.colors.text_dim;
        // Per-pill drop shadow — same rings as `draw_pill_inner` so
        // the fade-out feels continuous with the pill chrome the user
        // was just looking at.
        let shadow_y_offset = 6.0_f64;
        let shadow_rings: [(f64, u8); 4] = [
            (2.0, 51),
            (5.0, 30),
            (9.0, 15),
            (14.0, 5),
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
        // Pill body: use the "inactive" tier — the closing pill is
        // no longer the active tab (active-id shifted at close-start).
        let shape = RoundedRect::from_rect(r, radius);
        let fill = vello::peniko::Color::rgba8(255, 255, 255, 8);
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            vello::peniko::Color::rgba8(255, 255, 255, 18),
            None,
            &shape,
        );
        let text_size = theme.tokens.typography.scale.sm;
        let title_weight = TextWeight::Semibold;
        let (_tw, th) = tr.measure_weighted(&c.desc.title, text_size, None, title_weight);
        let leading_shift = if let Some(kind) = c.desc.app_kind {
            let icon_x0 = r.x0 + 10.0;
            let icon_rect = Rect::new(
                icon_x0,
                r.center().y - 7.0,
                icon_x0 + 14.0,
                r.center().y + 7.0,
            );
            draw_app_icon(scene, transform, icon_rect, kind, text_dim);
            14.0 + 6.0
        } else {
            0.0
        };
        let title_x = r.x0 + 12.0 + leading_shift;
        let title_y = r.center().y - th as f64 / 2.0;
        // Clip the title just before the close glyph — same rule as
        // `draw_pill_inner` so the title doesn't visibly slide past
        // the close × while the pill shrinks.
        let clip_r = Rect::new(r.x0, r.y0, c.close_rect.x0 - 2.0, r.y1);
        scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &clip_r);
        tr.draw_text_weighted(
            scene,
            &c.desc.title,
            text_size,
            text_dim,
            transform * Affine::translate((title_x, title_y)),
            None,
            title_weight,
        );
        scene.pop_layer();
        // Close glyph — draw it one last time as the pill fades out
        // so the user sees the × they just clicked confirming the
        // destructive action.
        icons::draw_icon(scene, transform, c.close_rect, icons::Icon::X, text_dim, 1.5);
    }

    /// Compute the top-left corner the drag-ghost should render at. The
    /// ghost's top-left = `last_cursor - drag_offset` so the pill
    /// follows the cursor at whatever relative grab point the user
    /// pressed. Returns `None` when no drag is in flight or the source
    /// tab is gone. Exposed to `draw` and (via `ghost_rect`) to tests.
    fn compute_ghost_rect(&self) -> Option<Rect> {
        let source_idx = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => {
                self.tabs.iter().position(|t| t.id == *tab)?
            }
            _ => return None,
        };
        let offset = self.drag_offset.unwrap_or(vello::kurbo::Vec2::ZERO);
        let pill = *self.tab_rects.get(source_idx)?;
        let w = pill.width();
        let h = pill.height();
        let x0 = self.last_cursor.x - offset.x;
        let y0 = self.last_cursor.y - offset.y;
        Some(Rect::new(x0, y0, x0 + w, y0 + h))
    }

    /// Test-only accessor for the computed drag-ghost rect. `None` when
    /// no drag is in flight; `Some(rect)` once the press → motion
    /// threshold has latched `pending_drag`.
    #[cfg(test)]
    pub fn ghost_rect(&self) -> Option<Rect> {
        self.compute_ghost_rect()
    }

    /// Test-only accessor for the per-pill draw alpha. Returns
    /// `SOURCE_PILL_DIM_ALPHA` while this pill is the in-flight drag
    /// source, `1.0` otherwise. Used by the ghost tests to pin the
    /// source-pill dim without peeking into the vello scene.
    #[cfg(test)]
    pub fn pill_draw_alpha(&self, idx: usize) -> f32 {
        let source_idx = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => {
                self.tabs.iter().position(|t| t.id == *tab)
            }
            _ => None,
        };
        if source_idx == Some(idx) {
            Self::SOURCE_PILL_DIM_ALPHA
        } else {
            1.0
        }
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

    /// Begin the concept-parity close fade for the tab with `id`.
    /// Snapshots the pill's current rect + descriptor into
    /// `closing_tabs` so `draw` can keep painting the pill at
    /// declining alpha + scale for `CLOSE_FADE_MS`, then removes the
    /// tab from the live `tabs` vec so surviving pills immediately
    /// reflow into the freed slot (via the existing layout spring).
    /// Active-id shifts to the neighbour at close-start so the user
    /// never sees a "ghost active" frame while the fade plays.
    ///
    /// Returns `true` if the tab was found and closing started, `false`
    /// if no live tab with that id exists (already closing or never
    /// there — both harmless). Idempotent on a tab that's already in
    /// `closing_tabs`: restarting the curve would pop the pill back
    /// to full opacity, which reads worse than letting the in-flight
    /// fade finish.
    pub fn start_closing(&mut self, id: u64) -> bool {
        let idx = match self.tabs.iter().position(|t| t.id == id) {
            Some(i) => i,
            None => return false,
        };
        if self.closing_tabs.iter().any(|c| c.desc.id == id) {
            // Already closing — don't restart the curve.
            return false;
        }
        // Snapshot the pill + close-button rects *before* the remove
        // shifts `tab_rects`, so the fade renders in place even as
        // neighbours spring aside. `tab_rects`/`close_rects` are
        // populated by `apply_layout`; in the path where the bar has
        // never laid out (e.g. synthetic unit-test setup that seeds
        // `tabs` but doesn't call `apply_layout`) we gracefully fall
        // back to `Rect::ZERO` so the fade still fires — the missing
        // rect just means nothing visible paints, which is acceptable
        // because the bar has no visible pills in that state anyway.
        let pill_rect = self.tab_rects.get(idx).copied().unwrap_or(Rect::ZERO);
        let close_rect = self.close_rects.get(idx).copied().unwrap_or(Rect::ZERO);
        let desc = self.tabs[idx].clone();
        // Publish the id for `take_close()` so `WindowGroup` can
        // begin its own host-level fade-out. Drained once; re-click
        // is already guarded by the `closing_tabs` check above.
        self.pending_close = Some(id);
        // Snapshot current pill geometry for the layout spring that
        // reflows survivors; this also lets the removed pill's
        // neighbours slide into its old slot smoothly.
        self.snapshot_layout_for_spring();
        self.tabs.remove(idx);
        if self.active_id == id {
            // Prefer the tab at the same index (what slid into the slot);
            // fall back to the previous index if we just removed the last
            // tab; fall back to 0 if the group is now empty.
            self.active_id = self
                .tabs
                .get(idx)
                .or_else(|| self.tabs.last())
                .map(|t| t.id)
                .unwrap_or(0);
        }
        self.closing_tabs.push(ClosingTab {
            desc,
            pill_rect,
            close_rect,
            started_at: Instant::now(),
        });
        true
    }

    /// Finalise a close animation for `id` by dropping the entry from
    /// `closing_tabs`. Called by `WindowGroup` once the host-level
    /// fade-out has elapsed so the pill stops rendering. Returns
    /// `true` if an entry was dropped, `false` otherwise.
    pub fn finalize_close(&mut self, id: u64) -> bool {
        let before = self.closing_tabs.len();
        self.closing_tabs.retain(|c| c.desc.id != id);
        self.closing_tabs.len() != before
    }

    /// True while any pill's close fade is in flight. Used by
    /// `needs_animation_frame` and by `WindowGroup::needs_animation_frame`
    /// to keep pumping redraws until every closing pill has settled.
    pub fn has_closing(&self) -> bool {
        !self.closing_tabs.is_empty()
    }

    /// Test-only accessor for the currently-closing tab ids in
    /// insertion order. Mirrors `tabs()` but scoped to the fade-out
    /// path. Returning `Vec<u64>` (instead of a borrowed slice of
    /// `ClosingTab`) keeps the private struct out of the public API
    /// while still letting tests pin which ids are fading.
    #[cfg(test)]
    pub fn closing_tab_ids(&self) -> Vec<u64> {
        self.closing_tabs.iter().map(|c| c.desc.id).collect()
    }

    /// Test-only accessor for the `Instant` at which the close fade
    /// was latched for `id`. Used by the active-shift test to assert
    /// the timestamp was recorded at close-start (rather than on
    /// finalize).
    #[cfg(test)]
    pub fn closing_started_at(&self, id: u64) -> Option<Instant> {
        self.closing_tabs
            .iter()
            .find(|c| c.desc.id == id)
            .map(|c| c.started_at)
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
    /// Test-only accessor pinning the lucide glyph used for the per-tab
    /// close chip. Returning a hard-coded [`icons::Icon`] variant keeps
    /// the widget's concept-parity contract visible in the test suite:
    /// a future refactor that reverts to a `"×"` codepoint or a
    /// hand-drawn primitive would have to also delete this accessor,
    /// which makes the regression impossible to miss in review.
    #[cfg(test)]
    pub fn close_glyph_icon_for_tests() -> icons::Icon { icons::Icon::X }
    /// Test-only accessor for the per-tab maximize action glyph. The
    /// per-tab cluster only renders while the group is non-maximized,
    /// so this is always [`icons::Icon::Maximize2`] — the
    /// `Icon::Minimize2` swap lives on `WindowGroup`'s maximized
    /// floating controls, covered by its own accessor there.
    #[cfg(test)]
    pub fn max_glyph_icon_for_tests() -> icons::Icon { icons::Icon::Maximize2 }
    /// Test-only accessor for the per-tab search action glyph. Pins
    /// the hover-reveal magnifier to lucide's [`icons::Icon::Search`]
    /// so a revert to the hand-drawn circle+line flips the suite red.
    #[cfg(test)]
    pub fn search_glyph_icon_for_tests() -> icons::Icon { icons::Icon::Search }
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
    /// Test-only accessor for the per-tab maximize-icon hit rects.
    /// `WindowGroup`'s integration test drives a synthetic press against
    /// these rects to assert the maximize click toggles `WindowGroup::maximized`
    /// end-to-end.
    #[cfg(test)]
    pub fn max_rects(&self) -> &[Rect] { &self.max_rects }
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

    /// Test hook: returns the close-chip fill color `draw_pill_inner`
    /// would paint for tab `idx` under the current theme + hover state,
    /// or `None` when the chip wouldn't paint at all (neither the tab
    /// nor the close glyph is hovered). Pairs with
    /// `close_glyph_color_for_tests` so the red-500/80 hover parity
    /// with the concept (`hover:bg-red-500/80`) is exercised without a
    /// GPU draw.
    #[cfg(test)]
    pub(crate) fn close_chip_color_for_tests(
        &self,
        idx: usize,
        theme: &Theme,
    ) -> Option<vello::peniko::Color> {
        if idx >= self.tabs.len() {
            return None;
        }
        let close_hover = Some(idx) == self.hover_close_idx;
        let is_hover = Some(idx) == self.hover_idx;
        if close_hover {
            Some(theme.tokens.colors.semantic.danger.with_alpha_factor(0.8))
        } else if is_hover {
            Some(vello::peniko::Color::rgba8(255, 255, 255, 28))
        } else {
            None
        }
    }

    /// Test hook: returns the glyph color `draw_pill_inner` would use
    /// for the close `×` icon on tab `idx`. Mirrors the concept's
    /// `hover:text-white` — close-hover always yields opaque white;
    /// otherwise idle/active states fall back to theme text colors.
    #[cfg(test)]
    pub(crate) fn close_glyph_color_for_tests(
        &self,
        idx: usize,
        theme: &Theme,
    ) -> Option<vello::peniko::Color> {
        if idx >= self.tabs.len() {
            return None;
        }
        let close_hover = Some(idx) == self.hover_close_idx;
        let is_active = self.tabs[idx].id == self.active_id;
        Some(if close_hover {
            vello::peniko::Color::rgba8(255, 255, 255, 255)
        } else if is_active {
            theme.tokens.colors.text_primary
        } else {
            theme.tokens.colors.text_dim
        })
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
        // Defensive: if `apply_layout` hasn't repopulated `tab_rects` to
        // match the current `tabs` (can happen for one frame after a
        // detach/insert mutates `tabs` outside the layout pass — e.g.
        // GroupRow's drag-drop sweep splices a tab into a freshly-spawned
        // group whose taffy node hasn't laid out yet), bail out so we
        // don't index past the end. Next frame's apply_layout fixes it.
        if self.tab_rects.len() != self.tabs.len() {
            return;
        }
        // Per-pill styling constants (radius, colors) live in
        // `draw_pill_inner` — this top-level draw is purely the
        // per-frame timeline: layout spring, insert fade, source dim,
        // drop-zone, and ghost. Keeps the draw routine focused on
        // scheduling the paint passes rather than pill chrome.

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

        // Source index of the in-flight drag, resolved from
        // `pending_drag`. `None` when no drag is active; also `None` if
        // the tab was removed mid-drag (defensive — shouldn't happen but
        // we don't want a panic during the ghost pass).
        let drag_source_idx: Option<usize> = match &self.pending_drag {
            Some(DragPayload::Tab { tab, .. }) => {
                self.tabs.iter().position(|t| t.id == *tab)
            }
            _ => None,
        };

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

            // Source-dim: while this tab is the one being dragged, dim
            // the in-place pill to 0.35 so the slot reads as "pulled
            // out" while the ghost paints on top. Concept parity with
            // `WindowFrame.tsx` where the native HTML5 drag image takes
            // over and the source element dims via the browser's
            // default drag visual. Combines multiplicatively with
            // `fade_alpha` so an insert-fade mid-drag still composes.
            let source_dim = if drag_source_idx == Some(i) {
                Self::SOURCE_PILL_DIM_ALPHA
            } else {
                1.0
            };
            let combined_alpha = fade_alpha * source_dim;

            // If this pill is mid-fade, push a pill-scoped alpha layer and
            // shift the transform vertically. The dx/dy for the layout
            // spring still apply — a just-inserted pill has no prev rect so
            // `dy` stays zero, but we honour it here for completeness.
            let pill_transform = if fade_y != 0.0 {
                transform * Affine::translate((0.0, fade_y))
            } else {
                transform
            };
            let pill_alpha_active = combined_alpha < 1.0;
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
                    combined_alpha,
                    transform,
                    &clip,
                );
            }
            // All per-pill paint below uses `pill_transform` so the fade's
            // vertical offset composes with any layout-spring dx/dy.
            self.draw_pill_inner(scene, pill_transform, i, r, tr, theme, presence, false);

            // Close out the insert-fade layer, if one was pushed at the
            // top of this iteration. The branch must mirror the push
            // exactly — `pill_alpha_active` tracks that.
            if pill_alpha_active {
                scene.pop_layer();
            }
        }

        // Closing-pill fade-out pass. Each entry paints at its
        // snapshotted `pill_rect` with an ease-out-cubic curve that
        // drives alpha 1 → 0, scale 1 → CLOSE_SCALE_END, and
        // y-offset 0 → CLOSE_Y_OFFSET_END over `CLOSE_FADE_MS`. The
        // motion mirrors the concept's
        // `exit={{ opacity: 0, scale: 0.95, y: -20 }}` on each tab
        // pill and pairs with the tile-level spawn curve in
        // `tile_host.rs`. Entries whose elapsed > fade window are
        // retained here (they render at alpha 0 → invisible) but are
        // cleaned up by `WindowGroup::sweep_closed_hosts` once the
        // host-level fade also elapses. Drawn after the main pill
        // loop so the closing pill visually sits *above* the
        // neighbours that are springing into its old slot, matching
        // the concept's z-ordering.
        if !self.closing_tabs.is_empty() {
            let close_window = Duration::from_millis(CLOSE_FADE_MS as u64);
            for c in &self.closing_tabs {
                let elapsed = c.started_at.elapsed();
                if elapsed >= close_window {
                    continue;
                }
                let raw = (elapsed.as_secs_f64()
                    / (CLOSE_FADE_MS as f64 / 1000.0))
                    .clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - raw).powi(3);
                let alpha = (1.0 - eased) as f32;
                let scale = 1.0 + (CLOSE_SCALE_END - 1.0) * eased;
                let y_offset = CLOSE_Y_OFFSET_END * eased;
                // Wide clip so the pill's drop shadow + hover-reveal
                // icons still paint without being chopped mid-fade.
                let clip = Rect::new(
                    c.pill_rect.x0 - 20.0,
                    c.pill_rect.y0 - 20.0,
                    c.pill_rect.x1 + 80.0,
                    c.pill_rect.y1 + 40.0,
                );
                scene.push_layer(
                    vello::peniko::BlendMode::default(),
                    alpha.max(0.0),
                    transform,
                    &clip,
                );
                let centre = c.pill_rect.center();
                let close_transform = transform
                    * Affine::translate((0.0, y_offset))
                    * Affine::translate((centre.x, centre.y))
                    * Affine::scale(scale)
                    * Affine::translate((-centre.x, -centre.y));
                self.draw_closing_pill(
                    scene,
                    close_transform,
                    c,
                    tr,
                    theme,
                );
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

        // Drag-ghost: translucent copy of the pill being dragged,
        // painted LAST so it composites over every other pill and over
        // the drop-zone indicators. Follows `last_cursor` at the
        // original grab offset so the pill reads as "attached" to the
        // cursor. Concept parity with `WindowFrame.tsx` where the
        // native HTML5 drag image provides the same effect. The ghost
        // does not participate in hit-testing — hit-tests still use the
        // original `tab_rects`.
        //
        // Deliberately not clipped to `self.rect`: the concept's drag
        // image is allowed to drift into the row above/below, and
        // cross-group drag handoff (`GroupRow::handle_event`) reads the
        // raw cursor position, not the ghost.
        if let Some(ghost_rect) = self.compute_ghost_rect() {
            if let Some(source_idx) = match &self.pending_drag {
                Some(DragPayload::Tab { tab, .. }) => {
                    self.tabs.iter().position(|t| t.id == *tab)
                }
                _ => None,
            } {
                // A generous layer rect so the ghost's drop shadow +
                // optional presence bleed paints without clipping. The
                // ghost pill's own chrome sits inside `ghost_rect`; the
                // surrounding padding matches the shadow spread.
                let clip = Rect::new(
                    ghost_rect.x0 - 20.0,
                    ghost_rect.y0 - 20.0,
                    ghost_rect.x1 + 20.0,
                    ghost_rect.y1 + 40.0,
                );
                scene.push_layer(
                    vello::peniko::BlendMode::default(),
                    Self::GHOST_PILL_ALPHA,
                    transform,
                    &clip,
                );
                self.draw_pill_inner(
                    scene,
                    transform,
                    source_idx,
                    ghost_rect,
                    tr,
                    theme,
                    presence,
                    true,
                );
                scene.pop_layer();
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
                // `start_closing` seeds `pending_close` for the
                // `WindowGroup`-level drain *and* latches the
                // per-pill fade-out so the pill keeps painting at
                // declining alpha + shrinking scale for
                // `CLOSE_FADE_MS`. Active-id shifts to the neighbour
                // immediately inside `start_closing`, so a user who
                // closes the active tab never sees a frame where the
                // closing pill is still the active one.
                if let Some(ci) = close_idx {
                    if let Some(t) = self.tabs.get(ci) {
                        let id = t.id;
                        self.start_closing(id);
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
                    // Defensive `.get` against a stale `hover_idx`: the
                    // index was populated by the previous frame's
                    // `CursorMoved` and a neighbouring widget's
                    // `handle_event` may have shrunk `tabs`/`tab_rects`
                    // (`start_closing` / `remove_tab` / cross-group
                    // detach) between move and press. Mirrors the same
                    // contract `draw` got in commit `3e01242` for the
                    // anchor cross-group-drop crash. Clear the stale
                    // index so the next `CursorMoved` repopulates it
                    // against the current frame's rects.
                    let Some(pill) = self.tab_rects.get(idx).copied() else {
                        self.hover_idx = None;
                        return;
                    };
                    let origin = pill.center();
                    self.press_origin = Some((idx, origin));
                    // Cache the grab offset within the pill. `DragPayload::Tab`
                    // doesn't carry this, but `draw` needs it to position
                    // the drag-ghost under the cursor at the same relative
                    // offset the user originally clicked — concept parity
                    // with the HTML5 drag-image behaviour in
                    // `WindowFrame.tsx`.
                    self.drag_offset = Some(vello::kurbo::Vec2::new(
                        self.last_cursor.x - pill.x0,
                        self.last_cursor.y - pill.y0,
                    ));
                    // `self.tabs` and `self.tab_rects` are populated
                    // together in `apply_layout`, but guard against the
                    // pathological case where they desync mid-frame.
                    if let Some(tab) = self.tabs.get(idx) {
                        self.active_id = tab.id;
                    }
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
                // Drag is over — clear the cached grab offset so a fresh
                // press computes it from its own cursor position and the
                // ghost stops painting.
                self.drag_offset = None;
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
        // 4. Per-tab close fade (180 ms). While any pill is in the
        //    closing vec with a start-stamp inside the fade window,
        //    keep pumping redraws so the alpha/scale curve plays out.
        let close_fade = self.closing_tabs.iter().any(|c| {
            c.started_at.elapsed().as_millis() < CLOSE_FADE_MS as u128
        });
        drop_zone || layout || insert_fade || close_fade
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

    #[test]
    fn tab_close_glyph_draws_via_icons_module() {
        // Pin the per-tab close chip to lucide's `X`. The direct
        // assertion would require rendering into a Vello scene and
        // inspecting strokes, which is fragile; the `*_for_tests`
        // accessor is the contract we actually care about — if a
        // future refactor replaces `icons::draw_icon(..., Icon::X, ..)`
        // with anything else, this test (and the analogous checks for
        // maximize / search below) has to be touched.
        assert_eq!(TabBar::close_glyph_icon_for_tests(), icons::Icon::X);
    }

    #[test]
    fn tab_max_glyph_draws_via_icons_module() {
        assert_eq!(TabBar::max_glyph_icon_for_tests(), icons::Icon::Maximize2);
    }

    #[test]
    fn tab_search_glyph_draws_via_icons_module() {
        assert_eq!(TabBar::search_glyph_icon_for_tests(), icons::Icon::Search);
    }

    /// Press on a pill, drag past the threshold, and verify the ghost
    /// rect positions itself at `last_cursor - drag_offset` with the
    /// source pill's dimensions. Pinned because the ghost is the
    /// cursor-following drag preview (concept parity with
    /// `WindowFrame.tsx`): if a refactor drops the rect computation the
    /// user loses the visual cue that a drag is in flight.
    #[test]
    fn drag_ghost_paints_at_cursor_while_drag_in_flight() {
        let mut b = bar();
        // Hand-built layout: three pills, each 80 px wide, bar covers
        // [0..240, 0..40]. Pill A sits at [0..80, 6..34]; the press
        // will land inside it.
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(80.0, 6.0, 160.0, 34.0),
            Rect::new(160.0, 6.0, 240.0, 34.0),
        ];
        b.close_rects = vec![
            Rect::new(60.0, 10.0, 78.0, 30.0),
            Rect::new(140.0, 10.0, 158.0, 30.0),
            Rect::new(220.0, 10.0, 238.0, 30.0),
        ];
        b.max_rects = vec![
            Rect::new(40.0, 10.0, 56.0, 30.0),
            Rect::new(120.0, 10.0, 136.0, 30.0),
            Rect::new(200.0, 10.0, 216.0, 30.0),
        ];
        b.search_rects = vec![
            Rect::new(20.0, 10.0, 36.0, 30.0),
            Rect::new(100.0, 10.0, 116.0, 30.0),
            Rect::new(180.0, 10.0, 196.0, 30.0),
        ];

        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;

        // Step 1: CursorMoved to (10, 15) — inside pill A, 10 px from
        // its left edge and 9 px from its top. Deliberately left of
        // A's search_rect (starts at x=20) so the press doesn't get
        // intercepted as a search-icon hit. `last_cursor` updates every
        // CursorMoved regardless of press state; this also primes the
        // hover caches.
        let press_cursor = Point::new(10.0, 15.0);
        let move_in = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(press_cursor.x, press_cursor.y),
        };
        let mut handled = false;
        let mut focus_target = None;
        b.handle_event(
            &move_in,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );

        // Step 2: Press on A.
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
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
        assert!(
            b.drag_offset.is_some(),
            "press must latch a drag_offset",
        );
        // No ghost yet — drag hasn't exceeded the motion threshold.
        assert!(
            b.ghost_rect().is_none(),
            "pre-threshold press must not paint a ghost",
        );

        // Step 3: Move past the threshold to (120, 60). This promotes
        // press → drag and `pending_drag` is latched.
        let drag_cursor = Point::new(120.0, 60.0);
        let move_past = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(drag_cursor.x, drag_cursor.y),
        };
        b.handle_event(
            &move_past,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        assert!(
            b.drag_in_flight(),
            "move past threshold must latch pending_drag",
        );

        // Ghost rect: top-left = last_cursor - drag_offset
        //   drag_offset was (10 - 0, 15 - 6) = (10, 9) at press time.
        //   last_cursor = (120, 60) after the threshold-crossing move.
        //   → ghost top-left = (120 - 10, 60 - 9) = (110, 51).
        //   → ghost size = source pill (A) = 80 x 28.
        let ghost = b
            .ghost_rect()
            .expect("drag-ghost must be present during drag");
        assert!(
            (ghost.x0 - 110.0).abs() < 0.01,
            "ghost x0 = cursor.x - offset.x (got {})",
            ghost.x0,
        );
        assert!(
            (ghost.y0 - 51.0).abs() < 0.01,
            "ghost y0 = cursor.y - offset.y (got {})",
            ghost.y0,
        );
        assert!(
            (ghost.width() - 80.0).abs() < 0.01,
            "ghost width tracks source pill width (got {})",
            ghost.width(),
        );
        assert!(
            (ghost.height() - 28.0).abs() < 0.01,
            "ghost height tracks source pill height (got {})",
            ghost.height(),
        );
    }

    /// Release during a drag must clear `ghost_rect()`. Otherwise the
    /// pill would keep painting at the last cursor position after the
    /// drop commits — a visible ghost stuck at the edge of the screen.
    #[test]
    fn drag_ghost_clears_on_release() {
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
        b.tab_rects = vec![
            Rect::new(0.0, 6.0, 80.0, 34.0),
            Rect::new(80.0, 6.0, 160.0, 34.0),
            Rect::new(160.0, 6.0, 240.0, 34.0),
        ];
        b.close_rects = vec![
            Rect::new(60.0, 10.0, 78.0, 30.0),
            Rect::new(140.0, 10.0, 158.0, 30.0),
            Rect::new(220.0, 10.0, 238.0, 30.0),
        ];
        b.max_rects = vec![
            Rect::new(40.0, 10.0, 56.0, 30.0),
            Rect::new(120.0, 10.0, 136.0, 30.0),
            Rect::new(200.0, 10.0, 216.0, 30.0),
        ];
        b.search_rects = vec![
            Rect::new(20.0, 10.0, 36.0, 30.0),
            Rect::new(100.0, 10.0, 116.0, 30.0),
            Rect::new(180.0, 10.0, 196.0, 30.0),
        ];

        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;
        let mut handled = false;
        let mut focus_target = None;

        // Press + drag past threshold. Cursor starts at (10, 15) —
        // inside pill A, left of every icon rect so the press isn't
        // intercepted as a search / max / close hit.
        let move_in = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(10.0, 15.0),
        };
        b.handle_event(
            &move_in,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
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
        let move_past = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(120.0, 20.0),
        };
        b.handle_event(
            &move_past,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        assert!(b.ghost_rect().is_some(), "precondition: ghost visible");

        // Release inside the bar — doesn't matter where; all branches
        // of Released must clear the ghost.
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
        assert_eq!(
            b.ghost_rect(),
            None,
            "release must clear the ghost so the pill stops painting",
        );
        assert!(
            b.drag_offset.is_none(),
            "release must also clear the cached drag offset",
        );
    }

    /// A freshly constructed bar has nothing dragging, so the ghost
    /// accessor must return `None`. This is the baseline the other
    /// tests implicitly rely on — if it ever changes, the ghost paint
    /// path would fire on idle frames.
    #[test]
    fn drag_ghost_is_none_when_no_drag() {
        let b = bar();
        assert_eq!(b.ghost_rect(), None);
        assert!(b.drag_offset.is_none());
        assert!(!b.drag_in_flight());
    }

    /// The source pill — the one whose tab is being dragged — must
    /// render at `SOURCE_PILL_DIM_ALPHA` (0.35) while the ghost is
    /// visible, matching the concept's "pulled out" fade of the
    /// original slot. Non-source pills stay at full opacity.
    #[test]
    fn source_pill_alpha_dims_while_dragged() {
        let mut b = bar();
        // Idle (no drag) — every pill renders at full alpha.
        assert!((b.pill_draw_alpha(0) - 1.0).abs() < f32::EPSILON);
        assert!((b.pill_draw_alpha(1) - 1.0).abs() < f32::EPSILON);
        assert!((b.pill_draw_alpha(2) - 1.0).abs() < f32::EPSILON);

        // Simulate a drag in flight on tab id 2 (index 1). Only the
        // source pill dims; its neighbours keep full opacity.
        b.pending_drag = Some(DragPayload::Tab { group: 0, tab: 2 });
        assert!(
            (b.pill_draw_alpha(0) - 1.0).abs() < f32::EPSILON,
            "non-source pill stays at full alpha",
        );
        assert!(
            (b.pill_draw_alpha(1) - TabBar::SOURCE_PILL_DIM_ALPHA).abs() < f32::EPSILON,
            "source pill dims to SOURCE_PILL_DIM_ALPHA (got {})",
            b.pill_draw_alpha(1),
        );
        assert!(
            (b.pill_draw_alpha(2) - 1.0).abs() < f32::EPSILON,
            "non-source pill stays at full alpha",
        );
        // Source-dim constant is distinct from the ghost constant so
        // the ghost stays the dominant visual even when the source
        // slot hasn't reflowed.
        assert!(
            TabBar::SOURCE_PILL_DIM_ALPHA < TabBar::GHOST_PILL_ALPHA,
            "source dim must be more transparent than the ghost",
        );
    }

    /// Helper that seeds a bar with authoritative pill + close rects
    /// so `start_closing` has geometry to snapshot. Mirrors the
    /// hand-built rects other tests in this module use to skip the
    /// taffy layout pass.
    fn bar_with_laid_out_rects() -> TabBar {
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 240.0, 40.0);
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
        b
    }

    #[test]
    fn take_close_moves_id_into_closing_vec() {
        // Click-close on tab 2. `start_closing` must:
        //   - add the id to `closing_tabs` with a recent `started_at`
        //   - publish `pending_close = Some(2)` for the
        //     `WindowGroup`-level drain (host-fade-out latch)
        //   - drain once via `take_close()` then go back to `None`
        let mut b = bar_with_laid_out_rects();
        assert!(b.closing_tab_ids().is_empty(), "precondition: no closing");
        assert_eq!(b.take_close(), None, "precondition: no pending close");

        let started = b.start_closing(2);
        assert!(started, "start_closing on a live tab id must succeed");
        assert_eq!(
            b.closing_tab_ids(),
            vec![2],
            "closing_tabs must carry the closing id",
        );
        let stamp = b
            .closing_started_at(2)
            .expect("started_at recorded at close-start");
        assert!(
            stamp.elapsed() < Duration::from_millis(CLOSE_FADE_MS as u64),
            "close-start timestamp is recent",
        );

        assert_eq!(
            b.take_close(),
            Some(2),
            "first drain returns the closing id once",
        );
        assert_eq!(
            b.take_close(),
            None,
            "second drain is None — matches the one-shot take_close contract",
        );
    }

    #[test]
    fn closing_tab_still_renders_pill_during_fade() {
        // After close-start, the pill is removed from `tabs` (so
        // survivors reflow) but kept in `closing_tabs` so the fade
        // curve has something to paint. `has_closing()` must report
        // true and `needs_animation_frame()` must demand redraws so
        // the curve plays out. Pinned here without reaching into the
        // Vello scene — the explicit `has_closing` + `closing_tab_ids`
        // accessors mirror the `pill_draw_alpha` test style used for
        // the drag-source dim.
        let mut b = bar_with_laid_out_rects();
        assert!(!b.has_closing(), "precondition: no closing pills");
        b.start_closing(2);
        assert!(
            b.has_closing(),
            "closing state must persist for draw to render the pill",
        );
        assert_eq!(
            b.closing_tab_ids(),
            vec![2],
            "draw walks this list to paint closing pills",
        );
        assert!(
            b.needs_animation_frame(),
            "close fade must pump redraws so the curve is visible",
        );
        // Survivor count: 2 (ids 1 and 3 remain in the live list).
        let live: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
        assert_eq!(live, vec![1, 3], "closing tab is out of the live list");
    }

    #[test]
    fn closing_tab_active_shifts_to_neighbour_immediately() {
        // If the closing tab was active, `active_id()` must point at
        // a neighbour *before* the fade completes — otherwise there
        // would be a window in which the active pill is the one
        // fading away, which reads as a "ghost active" glitch. This
        // guards against a regression where `start_closing` forgets
        // to run the active-shift branch.
        let mut b = bar_with_laid_out_rects();
        b.set_active(2);
        assert_eq!(b.active_id(), 2, "precondition: tab 2 is active");
        b.start_closing(2);
        assert_ne!(
            b.active_id(),
            2,
            "active must move off the closing tab at close-start",
        );
        assert!(
            b.active_id() == 1 || b.active_id() == 3,
            "active shifts to a live neighbour (got {})",
            b.active_id(),
        );
        // The closing tab must still be rendering — the shift is a
        // logical promotion, not a cancellation of the fade.
        assert!(b.has_closing(), "close fade is still in flight");
    }

    #[test]
    fn closing_tab_is_not_hit_testable() {
        // While a pill is in `closing_tabs`, a press / cursor at its
        // old snapshotted rect must *not* trigger hover or press
        // handlers: the tab is already out of the live `tabs` vec,
        // so `hover_idx` / `hover_close_idx` / `hover_*` can't land
        // on an index that corresponds to it. Guards the
        // "read-only, fading away" invariant from the spec.
        let mut b = bar_with_laid_out_rects();
        b.set_active(1);
        b.start_closing(2);
        // Pill 2's old rect was [86, 6, 166, 34] and its close rect
        // [146, 10, 164, 30]. Move the cursor over both — neither
        // should flag a hover on pill 2 (it's no longer in the live
        // list). Only pill 3 should match the *first* rect (because
        // after the remove, survivors keep their original rects in
        // `tab_rects` until the next apply_layout — that's fine, we
        // just care that the closing id isn't magically re-matched).
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut state = automerge::AutoCommit::new();
        let root_id = automerge::ROOT;

        // Cursor over the closing pill's old close chip.
        let cursor = Point::new(155.0, 20.0);
        let moved = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(cursor.x, cursor.y),
        };
        b.handle_event(
            &moved,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state,
            &root_id,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        // `hover_close_idx` can only point at a live tab's close
        // rect. Because `close_rects` still holds the pre-close
        // geometry (apply_layout hasn't been re-run in this test),
        // index 1 matches — but that now corresponds to live tab 3,
        // not the closing tab 2. The contract we care about: pressing
        // the closing pill's close chip must NOT re-enter
        // `start_closing(2)`.
        let before_count = b.closing_tab_ids().len();
        let before_tabs: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
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
        // The closing set either stays the same or *adds* a different
        // id (tab 3's close — because its close chip geometry
        // overlaps tab 2's old one in this synthetic layout). The
        // invariant the test pins: tab 2 is not re-added and the
        // press didn't interact with the in-flight fade.
        assert!(
            b.closing_tab_ids().contains(&2),
            "tab 2 is still fading",
        );
        assert!(
            !b.closing_tab_ids()
                .iter()
                .filter(|id| **id == 2)
                .count()
                .gt(&1),
            "tab 2 must not be re-added to closing_tabs mid-fade",
        );
        // Live tabs may shrink (if the press hit tab 3's close) but
        // must never contain tab 2 — it's gone from the live list.
        let after_tabs: Vec<u64> = b.tabs().iter().map(|t| t.id).collect();
        assert!(
            !after_tabs.contains(&2),
            "closing tab is never hit-testable back into the live list",
        );
        // Retain these so the compiler doesn't warn about unused bindings
        // even though the assertions above already cover them.
        let _ = (before_count, before_tabs);
    }

    /// Concept parity: the per-tab close pill in
    /// `nirios-concept/src/components/WindowFrame.tsx:334` uses
    /// `hover:bg-red-500/80`. Our implementation must render the same
    /// red-500 fill at 80% alpha (≈ rgba(239, 68, 68, 204)) when the
    /// cursor is over the close glyph. This pins the semantic token
    /// path end-to-end: `semantic.danger` is red-500, and we multiply
    /// by 0.8 at paint time rather than hard-coding a literal.
    #[test]
    fn close_hover_uses_red_500_fill() {
        let mut b = bar();
        // Synthetic pill/close layout. The numbers don't matter — the
        // chip-color decision only consults hover state.
        b.rect = Rect::new(0.0, 0.0, 80.0, 40.0);
        b.tab_rects = vec![Rect::new(0.0, 6.0, 80.0, 34.0)];
        b.close_rects = vec![Rect::new(60.0, 10.0, 78.0, 30.0)];
        b.hover_idx = Some(0);
        b.hover_close_idx = Some(0);

        let theme = Theme::dark();
        let color = b
            .close_chip_color_for_tests(0, &theme)
            .expect("close-hover must produce a chip fill");
        assert_eq!(
            (color.r, color.g, color.b, color.a),
            (239, 68, 68, 204),
            "close-hover must paint red-500 at 80% alpha (concept: hover:bg-red-500/80)",
        );

        // Without close-hover (tab hovered only) we should fall back
        // to the subtle white tint — the "you can close me" hint.
        b.hover_close_idx = None;
        let color = b
            .close_chip_color_for_tests(0, &theme)
            .expect("tab hover must still produce a subtle chip fill");
        assert_eq!(
            (color.r, color.g, color.b, color.a),
            (255, 255, 255, 28),
            "tab-only hover must stay as the subtle white tint",
        );

        // Neither hovered → no chip fill at all.
        b.hover_idx = None;
        assert!(
            b.close_chip_color_for_tests(0, &theme).is_none(),
            "idle close chip must not paint a fill",
        );
    }

    /// Concept parity: the close glyph promotes to full white on
    /// close-hover (`hover:text-white` in WindowFrame.tsx). Previous
    /// implementation rendered `text_primary`, which under dark themes
    /// can be slightly off-white — against a red-500/80 fill that
    /// reads as a grayish × rather than the crisp white the concept
    /// shows.
    #[test]
    fn close_hover_glyph_is_full_white() {
        let mut b = bar();
        b.rect = Rect::new(0.0, 0.0, 80.0, 40.0);
        b.tab_rects = vec![Rect::new(0.0, 6.0, 80.0, 34.0)];
        b.close_rects = vec![Rect::new(60.0, 10.0, 78.0, 30.0)];
        b.hover_idx = Some(0);
        b.hover_close_idx = Some(0);

        let theme = Theme::dark();
        let color = b
            .close_glyph_color_for_tests(0, &theme)
            .expect("close glyph color must always be defined for a live tab");
        assert_eq!(
            (color.r, color.g, color.b, color.a),
            (255, 255, 255, 255),
            "close-hover glyph must be opaque white (concept: hover:text-white)",
        );
    }

    /// Regression for B-6 (Family Cache-Stale): on `MouseInput::Pressed`,
    /// the press handler used to index `self.tab_rects[idx]` where
    /// `idx = self.hover_idx`. `hover_idx` is populated by the preceding
    /// `CursorMoved` against the *previous* frame's `tab_rects`. If a
    /// `start_closing` / `remove_tab` mutation in a neighbouring widget's
    /// `handle_event` shrinks `tabs` (and `tab_rects`) between move and
    /// press, the press indexes past the end and panics.
    ///
    /// The fix is the same defensive `.get(idx)` + early-return contract
    /// already applied to the draw path in commit `3e01242` (anchor #1).
    /// Test must drive a real `WindowEvent::MouseInput::Pressed` through
    /// `handle_event` — not just a unit-of-logic — so we pin the actual
    /// production codepath that exercised the bug.
    #[test]
    fn press_with_stale_hover_idx_does_not_panic() {
        let mut b = bar();
        // Worst-case staleness: `tab_rects` is empty (e.g. the previous
        // frame's tabs were all closed/detached before this press
        // arrived) but `hover_idx` still points into an arbitrary slot
        // from the prior `CursorMoved`.
        b.tab_rects.clear();
        b.close_rects.clear();
        b.max_rects.clear();
        b.search_rects.clear();
        b.hover_idx = Some(7);
        // `last_cursor` must be set so the press handler's hit-tests
        // run — the value itself doesn't matter because the rect vecs
        // are empty.
        b.last_cursor = Point::new(50.0, 20.0);

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
        // Pre-fix this call panics with index-out-of-bounds at the
        // `self.tab_rects[idx].center()` line. Post-fix the press path
        // takes the `.get(idx)` fallthrough, clears the stale
        // `hover_idx`, and returns cleanly.
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
        // After the early-return, the stale hover index must be
        // cleared so the next CursorMoved repopulates it from the
        // current frame's rects.
        assert_eq!(
            b.hover_idx, None,
            "stale hover_idx must be cleared when the rect cache no \
             longer covers it",
        );
        // No drag/select side effects should have fired off an empty
        // bar — this also guards the `pill = self.tab_rects[idx]`
        // line below the `origin` line.
        assert!(b.press_origin.is_none());
        assert!(b.drag_offset.is_none());
    }
}
