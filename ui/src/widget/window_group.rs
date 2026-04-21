use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use taffy::style_helpers::length;
use std::time::{Duration, Instant};
use crate::widget::{Widget, TabBar, TabDescriptor, TileContent, TileHost};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

pub const WIDTH_MIN_PX: u32 = 300;
pub const WIDTH_MAX_PX: u32 = 1200;
pub const RESIZE_HANDLE_PX: f64 = 6.0;

/// Duration of the per-group fade-in played when a `WindowGroup` is
/// newly spawned — e.g. via split-into-new-group from a cross-group
/// drag release. Matches the concept's
/// `initial={{ opacity: 0, scale: 0.95, y: 20 }}` →
/// `animate={{ opacity: 1, scale: 1, y: 0 }}` on each `<motion.div>`
/// window frame (`WindowFrame.tsx:201-205`). 320 ms is long enough for
/// the motion to read as an "arrival" without delaying the user's next
/// interaction on the new group.
const SPAWN_ANIM_MS: u64 = 320;
/// Vertical start offset for the spawn fade, in CSS-like px. Positive
/// values mean the group starts below its target and settles up —
/// mirrors the concept's `y: 20` initial.
const SPAWN_ANIM_Y_OFFSET: f64 = 20.0;

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
        }
    }
    pub fn width(&self) -> WindowWidth { self.width }

    /// Latch the concept-faithful spawn fade-in (opacity 0→1 with a
    /// 20 px → 0 vertical settle over `SPAWN_ANIM_MS`). Callers that
    /// create a brand-new `WindowGroup` — e.g. the split-into-new-group
    /// path in `GroupRow` — invoke this immediately after construction
    /// so the group reads as "arriving" rather than popping in opaque.
    /// A subsequent call within the animation window restarts it, which
    /// is acceptable (the group re-acquired focus; replay is the least
    /// surprising behaviour).
    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(Instant::now());
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
        Some((desc, host))
    }

    /// Insert a previously-detached tab at `idx`, becoming the new active
    /// tab. The content order is kept aligned with the bar's descriptor
    /// order so a subsequent reorder inside this group stays coherent.
    pub fn insert_detached_tab(&mut self, idx: usize, desc: TabDescriptor, host: TileHost) {
        let id = desc.id;
        self.bar.insert_tab(idx, desc);
        let clamped = idx.min(self.hosts.len());
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
        let w_px = self.width.pixels() as f32;
        let node = taffy.new_with_children(Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size { width: Dimension::length(w_px), height: Dimension::percent(1.0) },
            gap: taffy::Size { width: length(0.0), height: length(0.0) },
            ..Default::default()
        }, &[bar_node, tile_node]).unwrap();
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
        // Per-group spawn fade: if `start_spawn_animation` was called on
        // a freshly-created group (typically by the split-into-new-group
        // path), play the concept's
        // `initial={{ opacity: 0, scale: 0.95, y: 20 }}` →
        // `animate={{ opacity: 1, scale: 1, y: 0 }}` curve over
        // `SPAWN_ANIM_MS`. Computed upfront so the whole paint block
        // (bar + tile + chrome rings) composes under a single alpha
        // layer + translate + scale, keeping pill hover targets and
        // resize-handle hit-tests aligned with what the user sees on
        // screen mid-fade.
        let fade_window = Duration::from_millis(SPAWN_ANIM_MS);
        let (fade_alpha, fade_y, fade_scale): (f32, f64, f64) = match self.spawned_at {
            Some(started) => {
                let elapsed = started.elapsed();
                if elapsed >= fade_window {
                    (1.0, 0.0, 1.0)
                } else {
                    let raw = (elapsed.as_secs_f64()
                        / (SPAWN_ANIM_MS as f64 / 1000.0))
                        .clamp(0.0, 1.0);
                    let eased = 1.0 - (1.0 - raw).powi(3);
                    let scale = 0.95 + 0.05 * eased;
                    (eased as f32, SPAWN_ANIM_Y_OFFSET * (1.0 - eased), scale)
                }
            }
            None => (1.0, 0.0, 1.0),
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
        // y-offset and scaled about the group's center point so the
        // group reads as "settling down" into place.
        let transform = if fade_y != 0.0 || fade_scale != 1.0 {
            let cx = self.rect.center().x;
            let cy = self.rect.center().y;
            transform
                * Affine::translate((cx, cy + fade_y))
                * Affine::scale(fade_scale)
                * Affine::translate((-cx, -cy))
        } else {
            transform
        };
        self.bar.draw(scene, transform, tr, theme, presence);
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.draw(scene, transform, tr, theme, presence);
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
            let label = "Restore";
            let (lw, lh) = tr.measure(label, title_size, None);
            let lx = self.restore_rect.center().x - lw as f64 / 2.0;
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
            // hover so the destructive intent is obvious.
            let close_rr =
                vello::kurbo::RoundedRect::from_rect(self.close_rect, pill_h / 2.0);
            let close_alpha: u8 = if self.close_hover { 180 } else { 38 };
            let close_fill = if self.close_hover {
                theme.tokens.colors.semantic.danger
            } else {
                vello::peniko::Color::rgba8(255, 255, 255, close_alpha)
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                close_fill,
                None,
                &close_rr,
            );
            let glyph = "×";
            let glyph_size = theme.tokens.typography.scale.base;
            let (gw, gh) = tr.measure(glyph, glyph_size, None);
            let gx = self.close_rect.center().x - gw as f64 / 2.0;
            let gy = self.close_rect.center().y - gh as f64 / 2.0;
            tr.draw_text(
                scene,
                glyph,
                glyph_size,
                theme.tokens.colors.text_primary,
                transform * vello::kurbo::Affine::translate((gx, gy)),
                None,
            );
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
                // Close-glyph click on a maximized group: drop the active
                // tab's host, remove the tab from the bar, and exit
                // maximize so the group stops drawing floating controls
                // that now point at a stale tab.
                let active_id = self.bar.active_id();
                if let Some(idx) = self.hosts.iter().position(|(i, _)| *i == active_id) {
                    self.hosts.remove(idx);
                }
                self.bar.remove_tab(active_id);
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

        // Close a tab: drop its host + mirror the removal in the bar.
        if let Some(close_id) = self.bar.take_close() {
            if let Some(idx) = self.hosts.iter().position(|(i, _)| *i == close_id) {
                self.hosts.remove(idx);
            }
            self.bar.remove_tab(close_id);
            *handled = true;
        }

        // Maximize toggle from the tab bar's hover-action icon. We don't
        // yet reshape the layout tree (the shell paints floating controls
        // on top instead), so just flip the flag and let the shell pick it
        // up on its next draw.
        if let Some(_id) = self.bar.take_max() {
            self.maximized = !self.maximized;
            *handled = true;
        }

        // Search toggle from the tab bar's hover-action icon. Same pattern
        // as maximize — the bar publishes `pending_search` once per click,
        // we drain it here and flip the search-active ring on the group.
        if let Some(_id) = self.bar.take_search() {
            self.search_active = !self.search_active;
            *handled = true;
        }

        // Drag-reorder within the bar: apply the same permutation to hosts
        // so the visible tab order matches the content order.
        if let Some((from, to)) = self.bar.take_reorder() {
            if from < self.hosts.len() && to < self.hosts.len() {
                let item = self.hosts.remove(from);
                self.hosts.insert(to, item);
            }
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
        bar || hosts
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
        spawn_in_flight
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
}
