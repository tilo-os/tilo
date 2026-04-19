//! Rendering widget for the shell top bar.
//!
//! `TopBar` in `shell/topbar.rs` owns the logical state (workspace nav,
//! center-widget cooldown, tooltip timing, collaborator helpers). This file
//! provides a [`TopBarChrome`] that implements [`quick_ui::widget::Widget`]
//! and paints the bar onto the root scene. Phase 1 wires it into
//! `build_nirios_root` so there is finally a visible top-bar surface; later
//! phases will route clicks on its left/center/right clusters back into the
//! logical `TopBar` for workspace switch / overview toggle / quick-settings.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use chrono::Local;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;

use crate::shell::chrome_state::Chrome;
use crate::shell::state::WorkspaceTree;

pub const TOPBAR_HEIGHT_PX: f32 = 40.0;

/// Which center-widget variant is active in the top-bar carousel.
/// Scroll-wheel over the center band cycles between these; a three-dot
/// indicator on the left of the band shows the current position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

impl CenterWidget {
    pub fn next(self) -> Self {
        match self {
            CenterWidget::Clock => CenterWidget::SystemMonitor,
            CenterWidget::SystemMonitor => CenterWidget::Calendar,
            CenterWidget::Calendar => CenterWidget::Clock,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            CenterWidget::Clock => CenterWidget::Calendar,
            CenterWidget::SystemMonitor => CenterWidget::Clock,
            CenterWidget::Calendar => CenterWidget::SystemMonitor,
        }
    }
    pub fn index(self) -> usize {
        match self {
            CenterWidget::Clock => 0,
            CenterWidget::SystemMonitor => 1,
            CenterWidget::Calendar => 2,
        }
    }
    pub fn from_index(i: usize) -> Self {
        match i % 3 {
            0 => CenterWidget::Clock,
            1 => CenterWidget::SystemMonitor,
            _ => CenterWidget::Calendar,
        }
    }
}

/// Snapshot of the data the top bar renders. Cached at construction and
/// refreshed when the shell calls [`TopBarChrome::sync_from_tree`] (e.g. in
/// response to a workspace switch). Keeping the render widget off
/// `WorkspaceTree` lets `build_nirios_root` stay a pure function of the
/// tree and avoids a circular borrow against the shell's owned logical
/// `TopBar`.
pub struct TopBarChrome {
    rect: Rect,
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    collaborator_initials: Vec<char>,
    /// Count of collaborators beyond the three rendered as avatars. When
    /// nonzero the top bar paints a "+N" chip after the stack; concept
    /// keeps the chip's visual weight lighter than a real avatar.
    collaborator_overflow: usize,
    /// Shared chrome state; used to toggle overlays and switch workspaces
    /// in response to clicks on the top bar's left/right clusters.
    chrome: Option<Chrome>,
    /// Cached revision from `Chrome::revision` so `update` can detect when
    /// the tree has mutated since the last sync.
    synced_revision: u64,
    // Click geometry — recomputed every frame so it tracks DPI / resize.
    overview_btn_rect: Rect,
    up_btn_rect: Rect,
    down_btn_rect: Rect,
    ws_pill_rect: Rect,
    tray_pill_rect: Rect,
    /// Active center-widget. Scroll-wheel over `center_rect` cycles the
    /// variant; clicks on one of the three dot indicators jumps directly.
    center_widget: CenterWidget,
    /// Hit region for the center band (dots + content). Populated every
    /// draw so scroll + click hit-testing stays aligned with what's shown.
    center_rect: Rect,
    /// Dot-indicator click targets (one per variant). Same populate-on-draw
    /// contract as the other rect fields on this struct.
    center_dot_rects: [Rect; 3],
    /// Scroll-wheel accumulator over the center band. Matches the
    /// sidebar-wheel scroll contract so a single deliberate wheel tick
    /// advances one variant.
    center_scroll_accum: f32,
    /// Last cursor position in logical coords. Winit delivers the position
    /// with `CursorMoved` but not `MouseInput`, so we cache it here to
    /// hit-test click events.
    last_cursor: Option<Point>,
}

impl TopBarChrome {
    pub fn from_tree(tree: &WorkspaceTree) -> Self {
        let mut this = Self {
            rect: Rect::ZERO,
            workspace_name: String::new(),
            workspace_index: 0,
            workspace_total: 1,
            collaborator_initials: Vec::new(),
            collaborator_overflow: 0,
            chrome: None,
            synced_revision: 0,
            overview_btn_rect: Rect::ZERO,
            up_btn_rect: Rect::ZERO,
            down_btn_rect: Rect::ZERO,
            ws_pill_rect: Rect::ZERO,
            tray_pill_rect: Rect::ZERO,
            center_widget: CenterWidget::Clock,
            center_rect: Rect::ZERO,
            center_dot_rects: [Rect::ZERO; 3],
            center_scroll_accum: 0.0,
            last_cursor: None,
        };
        this.sync_from_tree(tree);
        this
    }

    /// Attach a shared chrome-state handle. The top bar uses it to toggle
    /// overlays (quick-settings, workspace-settings, overview) and to
    /// switch the active workspace via the up/down chevrons.
    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        let rev = chrome.borrow().revision;
        self.chrome = Some(chrome);
        self.synced_revision = rev;
        self
    }

    pub fn sync_from_tree(&mut self, tree: &WorkspaceTree) {
        self.workspace_total = tree.workspaces.len().max(1);
        let (idx, name) = tree
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.id == tree.active)
            .map(|(i, w)| (i, w.name.clone()))
            .unwrap_or((0, String::from("Workspace")));
        self.workspace_index = idx;
        self.workspace_name = name;
        self.collaborator_initials = tree
            .collaborators
            .iter()
            .take(3)
            .filter_map(|c| c.name.chars().next().map(|ch| ch.to_ascii_uppercase()))
            .collect();
        self.collaborator_overflow = tree.collaborators.len().saturating_sub(3);
    }

    fn current_clock_string() -> String {
        Local::now().format("%H:%M").to_string()
    }

    /// Initials painted as avatars (up to 3). Exposed for integration tests.
    pub fn collaborator_initials(&self) -> &[char] {
        &self.collaborator_initials
    }

    /// Count of collaborators beyond the three rendered as avatars.
    /// Zero when three-or-fewer peers; otherwise shown as the "+N" chip.
    pub fn collaborator_overflow(&self) -> usize {
        self.collaborator_overflow
    }

    /// Active center-widget variant. Exposed for integration tests so they
    /// can verify the scroll-wheel and dot-click state transitions.
    pub fn center_widget(&self) -> CenterWidget {
        self.center_widget
    }
}

impl Widget for TopBarChrome {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Re-sync when the shared chrome state has mutated since our last
        // pass — typically after a workspace switch or overlay toggle
        // triggered by a sibling widget.
        if let Some(chrome) = self.chrome.clone() {
            let rev = chrome.borrow().revision;
            if rev != self.synced_revision {
                // Clone the tree to avoid re-borrowing chrome while calling
                // `sync_from_tree` (which borrows &self mut).
                let tree_snap = chrome.borrow().tree.clone();
                self.sync_from_tree(&tree_snap);
                self.synced_revision = rev;
                return true;
            }
        }
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(TOPBAR_HEIGHT_PX),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &self.rect);

        let border_rect = Rect::new(
            self.rect.x0,
            self.rect.y1 - 1.0,
            self.rect.x1,
            self.rect.y1,
        );
        scene.fill(Fill::NonZero, transform, colors.border_standard, None, &border_rect);

        let pad_x = tokens.spacing.s3 as f64;
        let center_y = (self.rect.y0 + self.rect.y1) / 2.0;
        let icon_size = 14.0_f64;

        // --- Left cluster: overview icon + chevrons + workspace name ---
        let mut x = self.rect.x0 + pad_x;

        // Overview "grid" icon: 2x2 mini-squares.
        let cell = (icon_size - 4.0) / 2.0;
        self.overview_btn_rect = Rect::new(
            x - 4.0,
            center_y - icon_size / 2.0 - 4.0,
            x + icon_size + 4.0,
            center_y + icon_size / 2.0 + 4.0,
        );
        for (cx, cy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let r = Rect::new(
                x + cx * (cell + 2.0),
                center_y - icon_size / 2.0 + cy * (cell + 2.0),
                x + cx * (cell + 2.0) + cell,
                center_y - icon_size / 2.0 + cy * (cell + 2.0) + cell,
            );
            scene.fill(Fill::NonZero, transform, colors.text_dim, None, &r);
        }
        x += icon_size + tokens.spacing.s3 as f64;

        // Vertical divider
        let divider = Rect::new(x, center_y - 8.0, x + 1.0, center_y + 8.0);
        scene.fill(Fill::NonZero, transform, colors.border_standard, None, &divider);
        x += 1.0 + tokens.spacing.s2 as f64;

        // Up-chevron ▲
        let chev = 10.0_f64;
        self.up_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let up = vello::kurbo::BezPath::from_iter([
            vello::kurbo::PathEl::MoveTo(Point::new(x, center_y + chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev / 2.0, center_y - chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev, center_y + chev * 0.35)),
        ]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            if self.workspace_index == 0 { colors.border_standard } else { colors.text_dim },
            None,
            &up,
        );
        x += chev + tokens.spacing.s2 as f64;

        // Down-chevron ▼
        self.down_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let dn = vello::kurbo::BezPath::from_iter([
            vello::kurbo::PathEl::MoveTo(Point::new(x, center_y - chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev / 2.0, center_y + chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev, center_y - chev * 0.35)),
        ]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            if self.workspace_index + 1 >= self.workspace_total {
                colors.border_standard
            } else {
                colors.text_dim
            },
            None,
            &dn,
        );
        x += chev + tokens.spacing.s3 as f64;

        // Workspace-name button: text + gear glyph
        let ws_text_size = tokens.typography.scale.sm;
        let ws_label = format!(
            "{}  {}/{}",
            self.workspace_name, self.workspace_index + 1, self.workspace_total
        );
        let (ws_w, ws_h) = text_renderer.measure(&ws_label, ws_text_size, None);
        let ws_y = center_y - ws_h as f64 / 2.0;
        // Subtle pill backdrop so the workspace name reads as clickable.
        let ws_pill_outer = Rect::new(
            x - 6.0,
            center_y - 12.0,
            x + ws_w as f64 + 28.0,
            center_y + 12.0,
        );
        self.ws_pill_rect = ws_pill_outer;
        let pill = RoundedRect::from_rect(ws_pill_outer, tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &pill);
        text_renderer.draw_text(
            scene,
            &ws_label,
            ws_text_size,
            colors.text_primary,
            transform * Affine::translate((x, ws_y)),
            None,
        );
        let gear_x = x + ws_w as f64 + 6.0;
        // Gear placeholder: small circle with a dot inside.
        let gear_ring = vello::kurbo::Circle::new(Point::new(gear_x + 4.0, center_y), 4.5);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.2),
            transform,
            colors.text_dim,
            None,
            &gear_ring,
        );
        let gear_hole = vello::kurbo::Circle::new(Point::new(gear_x + 4.0, center_y), 1.3);
        scene.fill(Fill::NonZero, transform, colors.text_dim, None, &gear_hole);

        // --- Center cluster: carousel band with 3 dots + active widget ---
        let band_w = 360.0_f64;
        let band_h = 28.0_f64;
        let band_cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let band = Rect::new(
            band_cx - band_w / 2.0,
            center_y - band_h / 2.0,
            band_cx + band_w / 2.0,
            center_y + band_h / 2.0,
        );
        self.center_rect = band;
        // Subtle pill backdrop so the band reads as a scroll surface.
        let band_rr = RoundedRect::from_rect(band, tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &band_rr);

        // Three-dot indicator on the left of the band (vertical stack).
        let dot_x = band.x0 + 12.0;
        let dot_size = 3.0_f64;
        let dot_gap = 4.0_f64;
        let dot_col_h = 3.0 * dot_size + 2.0 * dot_gap;
        let dot_top = center_y - dot_col_h / 2.0;
        for i in 0..3 {
            let cy = dot_top + i as f64 * (dot_size + dot_gap) + dot_size / 2.0;
            let active = self.center_widget.index() == i;
            let r = dot_size * if active { 1.4 } else { 1.0 };
            let color = if active { colors.accent[5] } else { colors.text_dim };
            // Store dot hit region (generous 10 px square for clickability).
            self.center_dot_rects[i] = Rect::new(
                dot_x - 10.0,
                cy - 10.0,
                dot_x + 10.0,
                cy + 10.0,
            );
            let c = vello::kurbo::Circle::new(Point::new(dot_x, cy), r);
            scene.fill(Fill::NonZero, transform, color, None, &c);
        }

        // Widget content fills the band to the right of the dots.
        let content_x0 = dot_x + 24.0;
        let content_x1 = band.x1 - 12.0;
        let content_cx = (content_x0 + content_x1) / 2.0;
        match self.center_widget {
            CenterWidget::Clock => {
                let s = Self::current_clock_string();
                let size = tokens.typography.scale.base;
                let (tw, th) = text_renderer.measure(&s, size, None);
                let tx = content_cx - tw as f64 / 2.0;
                let ty = center_y - th as f64 / 2.0;
                text_renderer.draw_text(
                    scene,
                    &s,
                    size,
                    colors.text_primary,
                    transform * Affine::translate((tx, ty)),
                    None,
                );
            }
            CenterWidget::SystemMonitor => {
                // CPU % (left) • sparkline (stretch) • RAM / TMP (right).
                // Concept uses live random data; here we render a stable
                // snapshot derived from the clock seconds so nothing
                // repaints every frame yet the values change over time.
                let t = Local::now();
                let secs = t.format("%S").to_string().parse::<u32>().unwrap_or(0);
                let cpu = 12 + (secs % 20);
                let ram = 45 + (secs % 8);
                let tmp = 40 + (secs % 11);
                let sm_size = tokens.typography.scale.xs;
                let cpu_str = format!("{cpu}%");
                let ram_str = format!("{ram}%");
                let tmp_str = format!("{tmp}°");
                // Left: CPU label + value.
                text_renderer.draw_text(
                    scene,
                    "CPU",
                    sm_size,
                    colors.accent[4],
                    transform * Affine::translate((content_x0, center_y - 6.0)),
                    None,
                );
                text_renderer.draw_text(
                    scene,
                    &cpu_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x0 + 26.0, center_y - 7.0)),
                    None,
                );
                // Sparkline: 20 bars between CPU label and RAM cluster.
                let spark_x0 = content_x0 + 70.0;
                let spark_x1 = content_x1 - 100.0;
                let spark_w = (spark_x1 - spark_x0).max(40.0);
                let bars = 20usize;
                let bar_gap = 1.0;
                let bar_w = (spark_w - (bars as f64 - 1.0) * bar_gap) / bars as f64;
                for i in 0..bars {
                    // Deterministic but varied heights driven by seconds so
                    // the strip looks "live" on a re-render.
                    let seed = (secs as usize + i * 7) % 100;
                    let h = 4.0 + (seed as f64) * 0.08;
                    let bx = spark_x0 + i as f64 * (bar_w + bar_gap);
                    let b = Rect::new(bx, center_y + 6.0 - h, bx + bar_w, center_y + 6.0);
                    scene.fill(Fill::NonZero, transform, colors.accent[4], None, &b);
                }
                // Right: RAM + TMP chips.
                text_renderer.draw_text(
                    scene,
                    "RAM",
                    sm_size,
                    colors.accent[5],
                    transform * Affine::translate((content_x1 - 96.0, center_y - 6.0)),
                    None,
                );
                text_renderer.draw_text(
                    scene,
                    &ram_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x1 - 70.0, center_y - 7.0)),
                    None,
                );
                text_renderer.draw_text(
                    scene,
                    "TMP",
                    sm_size,
                    colors.text_dim,
                    transform * Affine::translate((content_x1 - 40.0, center_y - 6.0)),
                    None,
                );
                text_renderer.draw_text(
                    scene,
                    &tmp_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x1 - 18.0, center_y - 7.0)),
                    None,
                );
            }
            CenterWidget::Calendar => {
                // "Next: Design Sync  15:30" — concept uses a hardcoded next
                // event; we keep the same placeholder until real calendars
                // land.
                let prefix = "Next:";
                let event = "Design Sync";
                let time = "15:30";
                let size = tokens.typography.scale.sm;
                let (pw, ph) = text_renderer.measure(prefix, size, None);
                let (ew, _) = text_renderer.measure(event, size, None);
                let (tw, _) = text_renderer.measure(time, tokens.typography.scale.xs, None);
                let gap = 8.0;
                let total = pw as f64 + gap + ew as f64 + gap + tw as f64;
                let mut tx = content_cx - total / 2.0;
                let ty = center_y - ph as f64 / 2.0;
                text_renderer.draw_text(
                    scene,
                    prefix,
                    size,
                    colors.text_dim,
                    transform * Affine::translate((tx, ty)),
                    None,
                );
                tx += pw as f64 + gap;
                text_renderer.draw_text(
                    scene,
                    event,
                    size,
                    colors.text_primary,
                    transform * Affine::translate((tx, ty)),
                    None,
                );
                tx += ew as f64 + gap;
                text_renderer.draw_text(
                    scene,
                    time,
                    tokens.typography.scale.xs,
                    colors.text_dim,
                    transform * Affine::translate((tx, ty + 1.0)),
                    None,
                );
            }
        }

        // --- Right cluster: avatar stack + tray pill ---
        let mut rx = self.rect.x1 - pad_x;

        // Tray pill (rounded-full) with three dots for wifi / monitor / battery.
        let tray_w = 52.0_f64;
        let tray_h = 22.0_f64;
        self.tray_pill_rect = Rect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
        );
        let tray = RoundedRect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
            tray_h / 2.0,
        );
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &tray);
        for i in 0..3 {
            let dx = rx - tray_w + 12.0 + i as f64 * 12.0;
            let c = vello::kurbo::Circle::new(Point::new(dx, center_y), 2.0);
            scene.fill(Fill::NonZero, transform, colors.text_dim, None, &c);
        }
        rx -= tray_w + tokens.spacing.s3 as f64;

        // Avatar stack: up to 3 circles, overlapping by 4 px, newest on top.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let cx = rx - i as f64 * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            // Background plate (slight ring so overlapping circles stay readable).
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.overlay,
                None,
                &vello::kurbo::Circle::new(center, 9.0),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &vello::kurbo::Circle::new(center, 7.5),
            );
            let s = initial.to_string();
            let (tw, th) = text_renderer.measure(&s, tokens.typography.scale.sm, None);
            text_renderer.draw_text(
                scene,
                &s,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0)),
                None,
            );
        }

        // Overflow chip: "+N" in the same avatar slot, rendered in neutral
        // raised-surface colour so it reads as "count of extras" rather than
        // another person. Concept places this to the left of the stack.
        if self.collaborator_overflow > 0 {
            let i = self.collaborator_initials.len();
            let cx = rx - i as f64 * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.overlay,
                None,
                &vello::kurbo::Circle::new(center, 9.0),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &vello::kurbo::Circle::new(center, 7.5),
            );
            let s = format!("+{}", self.collaborator_overflow);
            let (tw, th) = text_renderer.measure(&s, tokens.typography.scale.xs, None);
            text_renderer.draw_text(
                scene,
                &s,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform * Affine::translate((center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Track the cursor position so `MouseInput` (which doesn't carry a
        // position) can still hit-test against the click geometry captured
        // during the most recent draw pass.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ));
            return;
        }
        // Scroll-wheel over the center band cycles the active center-widget
        // variant (Clock → SystemMonitor → Calendar → …). Mirrors the
        // sidebar-wheel accumulator contract so a single deliberate tick
        // advances one variant.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let Some(p) = self.last_cursor else { return };
            if !self.center_rect.contains(p) {
                return;
            }
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f32,
                winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.y as f32) / 40.0,
            };
            self.center_scroll_accum += dy;
            while self.center_scroll_accum >= 1.0 {
                self.center_widget = self.center_widget.next();
                self.center_scroll_accum -= 1.0;
            }
            while self.center_scroll_accum <= -1.0 {
                self.center_widget = self.center_widget.prev();
                self.center_scroll_accum += 1.0;
            }
            *handled = true;
            return;
        }
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        // Dot-indicator clicks: jump directly to that variant. These don't
        // need the shared chrome handle — purely local widget state.
        if let Some(p) = self.last_cursor {
            for (i, rect) in self.center_dot_rects.iter().enumerate() {
                if rect.contains(p) {
                    self.center_widget = CenterWidget::from_index(i);
                    *handled = true;
                    return;
                }
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Need the cursor position — winit delivers MouseInput without it,
        // so we reach into the last-known cursor via the Modifiers path.
        // For now, fall back to a separate CursorMoved tracker stored on
        // the widget. Without a cursor tracker we can't map; skip until a
        // CursorMoved arrives to prime the click area.
        let pos = match self.last_cursor {
            Some(p) => p,
            None => return,
        };
        let _ = scale_factor;
        let p = pos;
        let mut state = chrome.borrow_mut();
        if self.overview_btn_rect.contains(p) {
            state.overlays.overview = !state.overlays.overview;
            state.bump();
            *handled = true;
            return;
        }
        if self.up_btn_rect.contains(p) {
            if state.switch_workspace(-1) {
                *handled = true;
            }
            return;
        }
        if self.down_btn_rect.contains(p) {
            if state.switch_workspace(1) {
                *handled = true;
            }
            return;
        }
        if self.ws_pill_rect.contains(p) {
            state.overlays.ws_settings = !state.overlays.ws_settings;
            state.bump();
            *handled = true;
            return;
        }
        if self.tray_pill_rect.contains(p) {
            state.overlays.quick_settings = !state.overlays.quick_settings;
            state.bump();
            *handled = true;
        }
    }

    fn name(&self) -> &str {
        "TopBarChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
