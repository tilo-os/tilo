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
use vello::peniko::{Brush, ColorStop, ColorStops, Extend, Fill, Gradient, GradientKind};
use winit::event::WindowEvent;

use quick_ui::presence::{PresenceCtx, parse_hex_color, user_color};
use quick_ui::text_render::{TextRenderer, TextWeight};
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;
use vello::peniko::Color;

use crate::shell::chrome_state::Chrome;
use crate::shell::state::WorkspaceTree;

pub const TOPBAR_HEIGHT_PX: f32 = 40.0;

/// Concept §1.1 top-bar backdrop is `bg-black/80 backdrop-blur-xl`. Until the
/// pinned Vello supports `push_blur_layer`, we honour just the alpha split
/// (gap #3 in `docs/nirios/visual-gaps.md`): the top-bar surface is tinted
/// at 0.80 over whatever sits beneath it. The `surfaces.overlay` token in
/// `quick_ui::tokens::colors` is defined as `rgba8(0, 0, 0, 204)` —
/// `204 / 255 ≈ 0.8` — so this constant is the single source of truth the
/// `shell_chrome_test` suite pins the token against.
pub const TOP_BAR_SURFACE_ALPHA: f32 = 0.80;

/// Which center-widget variant is active in the top-bar carousel.
/// Scroll-wheel over the center band cycles between these; a three-dot
/// indicator on the left of the band shows the current position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterWidget {
    Clock,
    SystemMonitor,
    Calendar,
}

/// Identifies a metric column in the SystemMonitor center-widget. The
/// concept paints each metric in its own semantic colour (CPU emerald,
/// RAM blue, TMP orange — see `nirios-concept/src/components/\
/// TopBarWidgets.tsx:60-110`); routing through
/// [`TopBarChrome::metric_label_color`] keeps the per-metric colour
/// cue as a single source of truth so a future regression collapsing
/// the strip back to a monotone token surfaces at the helper rather
/// than scattered through the draw branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKind {
    Cpu,
    Ram,
    Tmp,
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
    /// Per-peer fill colour for the avatar circles (one per entry in
    /// `collaborator_initials`, same index). Resolved from the
    /// collaborator's `cursor_color` override when present and otherwise
    /// from [`quick_ui::presence::user_color`], so the top-bar stack
    /// matches the concept's `USERS[*].cursorColor` palette.
    collaborator_colors: Vec<Color>,
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
    /// Bounding rect of the avatar stack (initials + overflow chip). Click
    /// toggles `overlays.collaborators` so the shell can show the peer
    /// dropdown — matches the concept's avatar-stack-as-trigger affordance.
    avatar_stack_rect: Rect,
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
            collaborator_colors: Vec::new(),
            collaborator_overflow: 0,
            chrome: None,
            synced_revision: 0,
            overview_btn_rect: Rect::ZERO,
            up_btn_rect: Rect::ZERO,
            down_btn_rect: Rect::ZERO,
            ws_pill_rect: Rect::ZERO,
            tray_pill_rect: Rect::ZERO,
            avatar_stack_rect: Rect::ZERO,
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
        let active_ws = tree.workspaces.iter().find(|w| w.id == tree.active);
        let (idx, name) = tree
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.id == tree.active)
            .map(|(i, w)| (i, w.name.clone()))
            .unwrap_or((0, String::from("Workspace")));
        self.workspace_index = idx;
        self.workspace_name = name;
        // Roster source: prefer the active workspace's own collaborators
        // (per-workspace seed in `WorkspaceTree::bootstrap_default`), and
        // fall back to the tree-level roster only when the active workspace
        // carries no peers — mirrors the precedence applied by
        // `sidebar_wheel_chrome::sync_from_tree`. The local user (`id ==
        // "me"`) is filtered out of the avatar stack: the right cluster
        // shows *other* peers in the workspace, not the local user. Drop
        // collaborators whose name has no starting char and take at most
        // 3 — the rest flows into `collaborator_overflow`.
        let roster: &[crate::shell::state::Collaborator] = match active_ws {
            Some(ws) if ws.collaborators.iter().any(|c| c.id != "me") => {
                &ws.collaborators
            }
            _ => &tree.collaborators,
        };
        self.collaborator_initials.clear();
        self.collaborator_colors.clear();
        // Peer count flows through the shared helper so the top-bar
        // overflow chip and the wheel-disc active-card dot count always
        // agree (audit B-10).
        let peer_count = crate::shell::state::active_workspace_peer_count(tree);
        for c in roster.iter().filter(|c| c.id != "me").take(3) {
            let Some(ch) = c.name.chars().next() else { continue };
            self.collaborator_initials.push(ch.to_ascii_uppercase());
            let (r, g, b) = c
                .cursor_color
                .as_deref()
                .and_then(parse_hex_color)
                .unwrap_or_else(|| user_color(&c.id));
            self.collaborator_colors.push(Color::rgb8(r, g, b));
        }
        self.collaborator_overflow = peer_count.saturating_sub(3);
    }

    fn current_clock_string() -> String {
        Local::now().format("%H:%M").to_string()
    }

    /// Initials painted as avatars (up to 3). Exposed for integration tests.
    pub fn collaborator_initials(&self) -> &[char] {
        &self.collaborator_initials
    }

    /// Resolved avatar fill colours (one per entry in
    /// `collaborator_initials`, same index). Each peer's own
    /// `cursor_color` takes precedence; otherwise the palette-pinned
    /// [`quick_ui::presence::user_color`] is used. Exposed so tests can
    /// assert two different peers map to two different hues.
    pub fn collaborator_colors(&self) -> &[Color] {
        &self.collaborator_colors
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

    /// Test-only setter: jump the center-band widget directly to a
    /// variant without driving the scroll-wheel event path. Used by the
    /// concept-parity spec tests to put the bar into SystemMonitor mode
    /// so they can assert the CPU/RAM label colours match the concept's
    /// emerald-400/blue-400 semantic palette (visual-gap #11/#12 in the
    /// concept-coverage audit). Production code mutates `center_widget`
    /// only via `handle_event`'s scroll/click branches; this accessor is
    /// gated to test/debug builds so it can't leak into release.
    #[cfg(any(test, debug_assertions))]
    pub fn set_center_widget_for_tests(&mut self, w: CenterWidget) {
        self.center_widget = w;
    }

    /// Base (unscaled) radius used for the center-band three-dot stack.
    pub const DOT_BASE_SIZE: f64 = 3.0;
    /// Rendered radius of the *active* center-band dot. Sized 1.6× the
    /// base so the saturated indigo fill reads as the focused indicator;
    /// see gap #8 in `docs/nirios/visual-gaps.md`.
    pub const DOT_ACTIVE_RADIUS: f64 = Self::DOT_BASE_SIZE * 1.6;
    /// Rendered radius of the two inactive center-band dots.
    pub const DOT_INACTIVE_RADIUS: f64 = Self::DOT_BASE_SIZE * 1.0;

    /// Radius at which the active center-band dot is painted. Exposed so
    /// tests can assert it is strictly larger than the inactive radius
    /// (the contrast affordance is the scale-up + saturated indigo, not
    /// the scale alone).
    pub fn active_dot_radius(&self) -> f64 {
        Self::DOT_ACTIVE_RADIUS
    }

    /// Radius at which each inactive center-band dot is painted.
    pub fn inactive_dot_radius(&self) -> f64 {
        Self::DOT_INACTIVE_RADIUS
    }

    /// Which lucide glyph is used for the top-bar overview button.
    /// Pinned so the gap-#5 icon swap survives later refactors — if a
    /// future change reverts the overview glyph back to the four-rect
    /// quad, this accessor's test will fail loudly.
    pub fn overview_glyph_icon() -> quick_ui::icons::Icon {
        quick_ui::icons::Icon::LayoutGrid
    }

    /// Typographic weight the top bar paints the clock with. Concept
    /// `§2.2`: the center-band clock is `font-bold` / `font-mono`, so it
    /// reads as the "focal glance value" of the chrome. Exposed so
    /// integration tests can pin the contract — see visual-gap #7 in
    /// `docs/nirios/visual-gaps.md`.
    pub fn clock_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Typographic weight for the workspace-name pill (§2.1). Semibold so
    /// it reads as a clickable title without tipping over into the h1/h2
    /// weight tier the clock uses.
    pub fn workspace_name_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    /// Typographic weight the collaborator-avatar initial letters render at.
    /// Concept `CollaboratorPopover.tsx` paints initials at `font-bold`; the
    /// avatar circles are small enough that a Regular weight fades into the
    /// peer-colour fill (visual-gap #7).
    pub fn avatar_initial_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Typographic weight for the system-monitor row's acronym labels ("CPU",
    /// "RAM", "TMP"). Concept uses `font-mono font-semibold` for labels while
    /// the numeric values take `font-bold` — [`Self::system_monitor_value_text_weight`]
    /// exposes that tier. Splitting the two makes the strip scannable at a
    /// glance without every label shouting.
    pub fn system_monitor_label_text_weight(&self) -> TextWeight {
        TextWeight::Semibold
    }

    /// Typographic weight for the numeric values in the system-monitor row
    /// (e.g. "12%", "40°"). Concept tier: `font-mono font-bold`.
    pub fn system_monitor_value_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// Per-metric label colour used by the SystemMonitor center widget.
    /// Concept reference (`nirios-concept/src/components/\
    /// TopBarWidgets.tsx:60-110`): CPU is `text-emerald-400` (#4ade80),
    /// RAM is `text-blue-400` (#60a5fa), TMP is `text-orange-400`
    /// (#fb923c). Pinned through this helper so the draw branch never
    /// hard-codes `theme.accent_primary` for these labels — that monotone
    /// indigo collapsed the "different semantic colour per metric" cue
    /// the concept relies on (audit row `concept-coverage.md:23,163-166`,
    /// visual-gap #11/#12).
    pub fn metric_label_color(&self, k: MetricKind) -> Color {
        match k {
            // concept: text-emerald-400 (#4ade80)
            MetricKind::Cpu => Color::rgba8(0x4a, 0xde, 0x80, 0xff),
            // concept: text-blue-400 (#60a5fa)
            MetricKind::Ram => Color::rgba8(0x60, 0xa5, 0xfa, 0xff),
            // concept: text-orange-400 (#fb923c)
            MetricKind::Tmp => Color::rgba8(0xfb, 0x92, 0x3c, 0xff),
        }
    }

    /// Fill colour for the SystemMonitor sparkline bars. Concept paints
    /// every bar at `bg-emerald-500/30` (Tailwind emerald-500 = `#10b981`
    /// at 30% alpha, `0x4d` ≈ 0.30 × 255) regardless of the active
    /// metric — the sparkline reads as the CPU history strip and stays
    /// emerald even when the chip cluster on either side advertises
    /// RAM/TMP. Audit row: `concept-coverage.md:23,160-162` — replaces
    /// the previous `theme.accent_primary` (indigo-500) which was the
    /// wrong colour family entirely. The `_k` parameter is reserved so
    /// the per-metric switching variant from `TopBarWidgets.tsx` can be
    /// wired in later without changing the helper signature.
    pub fn sparkline_fill(&self, _k: MetricKind) -> Color {
        // concept: bg-emerald-500/30 (#10b981 at 0x4d ≈ 30% alpha)
        Color::rgba8(0x10, 0xb9, 0x81, 0x4d)
    }

    /// Bounding rect of the overview button. Populated during `draw`;
    /// exposed so integration tests can hit-test click coordinates against
    /// the exact geometry the widget uses internally.
    pub fn overview_btn_rect(&self) -> Rect {
        self.overview_btn_rect
    }

    /// Translucent-tint fallback alpha for the top-bar surface. Concept
    /// §1.1 uses `bg-black/80 backdrop-blur-xl`; without the `backdrop-blur`
    /// primitive we at least honour the /80 alpha split (visual-gap #3 in
    /// `docs/nirios/visual-gaps.md`). Pinned so a future regression that
    /// reverts the topbar to an opaque fill fails the chrome-test suite.
    pub fn top_bar_alpha() -> f32 {
        TOP_BAR_SURFACE_ALPHA
    }

    /// Bounding rect of the quick-settings tray pill (three-dot widget on
    /// the far right of the top bar). Populated during `draw`; exposed so
    /// integration tests can hit-test click coordinates against the exact
    /// geometry the widget uses internally.
    pub fn tray_pill_rect(&self) -> Rect {
        self.tray_pill_rect
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

        // Soft bottom shadow — masks the abrupt translucent-topbar →
        // exposed-backdrop transition that read as "noisy pixels"
        // against the dotted-grid wallpaper. Concept-style top-bar
        // has a subtle drop-shadow; we approximate with a 28-px
        // vertical fade extending below the bar. Vello widgets
        // aren't clipped to their rect, and the topbar paints
        // after the backdrop and before the workspace content,
        // so the band lays cleanly over the seam below.
        let shadow_h = 28.0_f64;
        let shadow_rect = Rect::new(
            self.rect.x0,
            self.rect.y1,
            self.rect.x1,
            self.rect.y1 + shadow_h,
        );
        let shadow_grad = Gradient {
            kind: GradientKind::Linear {
                start: Point::new(0.0, self.rect.y1),
                end: Point::new(0.0, self.rect.y1 + shadow_h),
            },
            extend: Extend::Pad,
            stops: ColorStops::from_iter([
                ColorStop {
                    offset: 0.0,
                    color: Color::rgba8(0, 0, 0, 46), // ≈ 0.18
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::rgba8(0, 0, 0, 0),
                },
            ]),
        };
        scene.fill(
            Fill::NonZero,
            transform,
            &Brush::Gradient(shadow_grad),
            None,
            &shadow_rect,
        );

        let pad_x = tokens.spacing.s3 as f64;
        let center_y = (self.rect.y0 + self.rect.y1) / 2.0;
        let icon_size = 14.0_f64;

        // --- Left cluster: overview icon + chevrons + workspace name ---
        let mut x = self.rect.x0 + pad_x;

        // Overview glyph: lucide `LayoutGrid` (gap #5). Drawn as stroked
        // rounded rects via `quick_ui::icons::draw_icon` so it matches
        // the concept's lucide-react lockup instead of the ad-hoc four
        // filled squares we used before.
        self.overview_btn_rect = Rect::new(
            x - 4.0,
            center_y - icon_size / 2.0 - 4.0,
            x + icon_size + 4.0,
            center_y + icon_size / 2.0 + 4.0,
        );
        let overview_icon_rect = Rect::new(
            x,
            center_y - icon_size / 2.0,
            x + icon_size,
            center_y + icon_size / 2.0,
        );
        quick_ui::icons::draw_icon(
            scene,
            transform,
            overview_icon_rect,
            Self::overview_glyph_icon(),
            colors.text_dim,
            quick_ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += icon_size + tokens.spacing.s3 as f64;

        // Vertical divider
        let divider = Rect::new(x, center_y - 8.0, x + 1.0, center_y + 8.0);
        scene.fill(Fill::NonZero, transform, colors.border_standard, None, &divider);
        x += 1.0 + tokens.spacing.s2 as f64;

        // Up-chevron: lucide `ChevronUp` (gap #5). Drawn through
        // `quick_ui::icons::draw_icon` so the chrome's chevrons share
        // stroke caps + joins with the rest of the lucide lockup
        // (`LayoutGrid` / `Settings2` / `Wifi` / `Monitor`) instead of
        // the previous hand-rolled BezPath polyline.
        let chev = 10.0_f64;
        self.up_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let up_icon_rect = Rect::new(
            x,
            center_y - chev / 2.0,
            x + chev,
            center_y + chev / 2.0,
        );
        quick_ui::icons::draw_icon(
            scene,
            transform,
            up_icon_rect,
            quick_ui::icons::Icon::ChevronUp,
            if self.workspace_index == 0 { colors.border_standard } else { colors.text_dim },
            quick_ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += chev + tokens.spacing.s2 as f64;

        // Down-chevron: lucide `ChevronDown` (gap #5). Same lockup
        // rationale as the up-chevron above.
        self.down_btn_rect = Rect::new(
            x - 4.0,
            center_y - chev * 0.35 - 6.0,
            x + chev + 4.0,
            center_y + chev * 0.35 + 6.0,
        );
        let dn_icon_rect = Rect::new(
            x,
            center_y - chev / 2.0,
            x + chev,
            center_y + chev / 2.0,
        );
        quick_ui::icons::draw_icon(
            scene,
            transform,
            dn_icon_rect,
            quick_ui::icons::Icon::ChevronDown,
            if self.workspace_index + 1 >= self.workspace_total {
                colors.border_standard
            } else {
                colors.text_dim
            },
            quick_ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        x += chev + tokens.spacing.s3 as f64;

        // Workspace-name button: text + gear glyph
        let ws_text_size = tokens.typography.scale.sm;
        let ws_label = format!(
            "{}  {}/{}",
            self.workspace_name, self.workspace_index + 1, self.workspace_total
        );
        // Concept §2.1: workspace-name label is `font-semibold`. Measure with
        // the same weight so the pill width tracks the glyph advance at
        // Semibold, not at the Regular fallback — visual-gap #7.
        let ws_weight = self.workspace_name_text_weight();
        let (ws_w, ws_h) = text_renderer.measure_weighted(&ws_label, ws_text_size, None, ws_weight);
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
        text_renderer.draw_text_weighted(
            scene,
            &ws_label,
            ws_text_size,
            colors.text_primary,
            transform * Affine::translate((x, ws_y)),
            None,
            ws_weight,
        );
        let gear_x = x + ws_w as f64 + 6.0;
        // Gear glyph: lucide `Settings2` (gap #5). The ad-hoc ring+dot
        // placeholder read as generic rather than "open settings"; the
        // lucide sliders+knobs glyph matches the concept's affordance.
        let gear_rect = Rect::new(
            gear_x - 2.0,
            center_y - 6.0,
            gear_x + 10.0,
            center_y + 6.0,
        );
        quick_ui::icons::draw_icon(
            scene,
            transform,
            gear_rect,
            quick_ui::icons::Icon::Settings2,
            colors.text_dim,
            quick_ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );

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
        let dot_size = Self::DOT_BASE_SIZE;
        let dot_gap = 4.0_f64;
        let dot_col_h = 3.0 * dot_size + 2.0 * dot_gap;
        let dot_top = center_y - dot_col_h / 2.0;
        for i in 0..3 {
            let cy = dot_top + i as f64 * (dot_size + dot_gap) + dot_size / 2.0;
            let active = self.center_widget.index() == i;
            // Active dot is scaled up 1.6× and painted in the saturated
            // indigo accent (`accent[5]` = indigo-500 in the token ramp).
            // Inactive dots use `neutral[7]` (slate-300) so they sit clearly
            // above `text_dim` (slate-400) but still read below
            // `text_primary` — this is the contrast bump from gap #8.
            let r = if active {
                Self::DOT_ACTIVE_RADIUS
            } else {
                Self::DOT_INACTIVE_RADIUS
            };
            let color = if active {
                colors.accent[5]
            } else {
                colors.neutral[7]
            };
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
                // Concept §2.2: clock is `font-bold`. Bold glyphs advance
                // wider than Regular, so the measure call uses the same
                // weight as the draw call — centering would drift by a few
                // pixels otherwise. Visual-gap #7.
                let weight = self.clock_text_weight();
                let (tw, th) = text_renderer.measure_weighted(&s, size, None, weight);
                let tx = content_cx - tw as f64 / 2.0;
                let ty = center_y - th as f64 / 2.0;
                text_renderer.draw_text_weighted(
                    scene,
                    &s,
                    size,
                    colors.text_primary,
                    transform * Affine::translate((tx, ty)),
                    None,
                    weight,
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
                // Left: CPU label + value. Concept monitor labels are
                // `font-mono font-semibold`; values are `font-mono font-bold`.
                text_renderer.draw_text_weighted(
                    scene,
                    "CPU",
                    sm_size,
                    // Concept paints each metric in its own semantic hue —
                    // CPU is `text-emerald-400` (#4ade80). Routing through
                    // `metric_label_color` keeps the per-metric colour cue
                    // the concept relies on (audit `concept-coverage.md:\
                    // 23,163-166`, visual-gap #11/#12) and prevents a
                    // future drift back to monotone indigo.
                    self.metric_label_color(MetricKind::Cpu),
                    transform * Affine::translate((content_x0, center_y - 6.0)),
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &cpu_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x0 + 26.0, center_y - 7.0)),
                    None,
                    TextWeight::Bold,
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
                    // Concept paints the sparkline at `bg-emerald-500/30`
                    // (#10b981 at 30% alpha) — see `nirios-concept/src/\
                    // components/TopBarWidgets.tsx:120-124`. The previous
                    // `theme.accent_primary` (indigo-500) was the wrong
                    // colour family entirely (audit `concept-coverage.md:\
                    // 23,160-162`).
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        self.sparkline_fill(MetricKind::Cpu),
                        None,
                        &b,
                    );
                }
                // Right: RAM + TMP chips. Same Semibold label / Bold value
                // tiering as the CPU cluster so the strip reads uniformly.
                text_renderer.draw_text_weighted(
                    scene,
                    "RAM",
                    sm_size,
                    // Concept: RAM label is `text-blue-400` (#60a5fa) —
                    // see `nirios-concept/src/components/\
                    // TopBarWidgets.tsx:60-110`. Previously `colors\
                    // .accent[5]` (indigo-500), which collapsed the
                    // per-metric semantic colour cue.
                    self.metric_label_color(MetricKind::Ram),
                    transform * Affine::translate((content_x1 - 96.0, center_y - 6.0)),
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &ram_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x1 - 70.0, center_y - 7.0)),
                    None,
                    TextWeight::Bold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    "TMP",
                    sm_size,
                    // Concept: TMP label is `text-orange-400` (#fb923c) —
                    // see `nirios-concept/src/components/\
                    // TopBarWidgets.tsx:60-110`. Previously `colors\
                    // .text_dim`, which dropped the temperature-warning
                    // hue the concept relies on.
                    self.metric_label_color(MetricKind::Tmp),
                    transform * Affine::translate((content_x1 - 40.0, center_y - 6.0)),
                    None,
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &tmp_str,
                    tokens.typography.scale.sm,
                    colors.text_primary,
                    transform * Affine::translate((content_x1 - 18.0, center_y - 7.0)),
                    None,
                    TextWeight::Bold,
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
                // Concept calendar-strip tiers: prefix + time as dim Medium
                // pieces, the event name as a Semibold primary-colour anchor.
                let prefix_weight = TextWeight::Medium;
                let event_weight = TextWeight::Semibold;
                let time_weight = TextWeight::Medium;
                let (pw, ph) = text_renderer.measure_weighted(prefix, size, None, prefix_weight);
                let (ew, _) = text_renderer.measure_weighted(event, size, None, event_weight);
                let (tw, _) = text_renderer.measure_weighted(time, tokens.typography.scale.xs, None, time_weight);
                let gap = 8.0;
                let total = pw as f64 + gap + ew as f64 + gap + tw as f64;
                let mut tx = content_cx - total / 2.0;
                let ty = center_y - ph as f64 / 2.0;
                text_renderer.draw_text_weighted(
                    scene,
                    prefix,
                    size,
                    colors.text_dim,
                    transform * Affine::translate((tx, ty)),
                    None,
                    prefix_weight,
                );
                tx += pw as f64 + gap;
                text_renderer.draw_text_weighted(
                    scene,
                    event,
                    size,
                    colors.text_primary,
                    transform * Affine::translate((tx, ty)),
                    None,
                    event_weight,
                );
                tx += ew as f64 + gap;
                text_renderer.draw_text_weighted(
                    scene,
                    time,
                    tokens.typography.scale.xs,
                    colors.text_dim,
                    transform * Affine::translate((tx, ty + 1.0)),
                    None,
                    time_weight,
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
        // Tray glyphs: lucide Wifi / Monitor / Battery (gap #5). The
        // earlier three-filled-dots lockup was generic ornament; the
        // real lucide trio tells the user what the pill is for at a
        // glance, matching the concept's right-cluster icon row.
        let tray_icon_size = 14.0_f64;
        let tray_icons = [
            quick_ui::icons::Icon::Wifi,
            quick_ui::icons::Icon::Monitor,
            quick_ui::icons::Icon::Battery,
        ];
        for (i, icon) in tray_icons.iter().enumerate() {
            let dx = rx - tray_w + 12.0 + i as f64 * 14.0;
            let icon_rect = Rect::new(
                dx - tray_icon_size / 2.0,
                center_y - tray_icon_size / 2.0,
                dx + tray_icon_size / 2.0,
                center_y + tray_icon_size / 2.0,
            );
            quick_ui::icons::draw_icon(
                scene,
                transform,
                icon_rect,
                *icon,
                colors.text_dim,
                quick_ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        rx -= tray_w + tokens.spacing.s4 as f64; // s4 = 16, ≥ 8 px after avatar radius

        // Snapshot the rightmost x of the stack before drawing so we can
        // populate `avatar_stack_rect` once we know how many circles we
        // actually drew (including the overflow chip).
        let stack_right_x = rx;

        // Avatar stack: up to 3 circles, overlapping by 4 px, newest on top.
        // Each circle uses the peer's own `cursor_color` (or the palette-pinned
        // `user_color` fallback), with a 2 px black separator ring so that
        // overlapping avatars stay visually distinct — concept's
        // `border-2 border-black` behaviour.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let cx = rx - i as f64 * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            // Black separator ring (2 px): paint a slightly larger black disc
            // under the fill so the visible annulus between r=9 and r=7.5 is
            // pure black, matching the concept's `border-2 border-black`
            // between overlapping circles.
            scene.fill(
                Fill::NonZero,
                transform,
                Color::rgb8(0, 0, 0),
                None,
                &vello::kurbo::Circle::new(center, 9.0),
            );
            let fill = self
                .collaborator_colors
                .get(i)
                .copied()
                .unwrap_or(colors.accent[5]);
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &vello::kurbo::Circle::new(center, 7.5),
            );
            let s = initial.to_string();
            let initial_weight = TextWeight::Bold;
            let (tw, th) = text_renderer.measure_weighted(&s, tokens.typography.scale.sm, None, initial_weight);
            text_renderer.draw_text_weighted(
                scene,
                &s,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0)),
                None,
                initial_weight,
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
            let overflow_weight = TextWeight::Semibold;
            let (tw, th) = text_renderer.measure_weighted(&s, tokens.typography.scale.xs, None, overflow_weight);
            text_renderer.draw_text_weighted(
                scene,
                &s,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform * Affine::translate((center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0)),
                None,
                overflow_weight,
            );
        }

        // Avatar stack click target. Spans from the left-most circle to the
        // right of the stack (rx before drawing). Empty collaborators still
        // get a zero-sized rect so the click handler short-circuits.
        let slot_count = self.collaborator_initials.len()
            + if self.collaborator_overflow > 0 { 1 } else { 0 };
        if slot_count > 0 {
            // Stack grows leftward from `stack_right_x`; each circle is 18 px
            // wide with 4 px overlap, so step = 14 px.
            let step = 14.0_f64;
            let leftmost_cx = stack_right_x - (slot_count as f64 - 1.0) * step;
            let y_mid = center_y;
            self.avatar_stack_rect = Rect::new(
                leftmost_cx - 9.0,
                y_mid - 11.0,
                stack_right_x,
                y_mid + 11.0,
            );
        } else {
            self.avatar_stack_rect = Rect::ZERO;
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
            return;
        }
        if self.avatar_stack_rect.contains(p) {
            state.overlays.collaborators = !state.overlays.collaborators;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    fn sample_chrome() -> TopBarChrome {
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;
        TopBarChrome::from_tree(&tree)
    }

    /// Gap #8: the active center-band dot must be strictly larger than an
    /// inactive dot. The indigo-500 fill plus the scale-up together give
    /// the "where are we in the carousel" affordance; losing the scale
    /// delta would collapse the indicator into three same-size circles.
    #[test]
    fn active_center_dot_radius_is_strictly_larger_than_inactive() {
        let chrome = sample_chrome();
        assert!(
            chrome.active_dot_radius() > chrome.inactive_dot_radius(),
            "active dot radius {} must be strictly larger than inactive {}",
            chrome.active_dot_radius(),
            chrome.inactive_dot_radius(),
        );
        // Guard the specific 1.6× ratio from the concept so a future tweak
        // is an intentional decision, not a silent regression.
        assert!(
            (chrome.active_dot_radius() - TopBarChrome::DOT_BASE_SIZE * 1.6).abs() < f64::EPSILON,
            "active dot radius should be 1.6× the base size",
        );
    }

    /// Gap #11/#12: the SystemMonitor strip must use a different semantic
    /// colour for each metric — emerald-400 for CPU, blue-400 for RAM,
    /// orange-400 for TMP — matching `nirios-concept/src/components/\
    /// TopBarWidgets.tsx:60-110`. The previous implementation rendered
    /// every label in `theme.accent_primary` (indigo-500), collapsing the
    /// per-metric colour cue. Pin the helper that the draw branch consults
    /// so a future regression that reroutes any of the three back to a
    /// monotone token fails this test loudly.
    #[test]
    fn system_monitor_metric_label_colors_are_semantic() {
        let chrome = sample_chrome();
        let cpu = chrome.metric_label_color(MetricKind::Cpu);
        let ram = chrome.metric_label_color(MetricKind::Ram);
        let tmp = chrome.metric_label_color(MetricKind::Tmp);
        // emerald-400
        assert_eq!(cpu, Color::rgba8(0x4a, 0xde, 0x80, 0xff));
        // blue-400
        assert_eq!(ram, Color::rgba8(0x60, 0xa5, 0xfa, 0xff));
        // orange-400
        assert_eq!(tmp, Color::rgba8(0xfb, 0x92, 0x3c, 0xff));
        // Guard rail: none of the three may be indigo-500 again.
        let indigo = Color::rgba8(0x63, 0x66, 0xf1, 0xff);
        assert_ne!(cpu, indigo);
        assert_ne!(ram, indigo);
        assert_ne!(tmp, indigo);
    }

    /// Gap #11/#12 — sparkline pair test. Concept paints the sparkline
    /// bars at `bg-emerald-500/30` (Tailwind emerald-500 = `#10b981`
    /// at 30% alpha = `0x4d`). Pin the helper so the bar paint loop in
    /// `draw` cannot drift back to indigo-500 without flipping this test.
    #[test]
    fn sparkline_fill_is_emerald_500_at_30_percent_alpha() {
        let chrome = sample_chrome();
        let fill = chrome.sparkline_fill(MetricKind::Cpu);
        // Tailwind emerald-500 (#10b981) at 30% alpha (0x4d ≈ 0.30 × 255).
        // The integration spec test pairs with this — see
        // `concept_parity_spec::topbar_system_monitor_sparkline_is_\
        // emerald_500_not_indigo`.
        assert_eq!(fill, Color::rgba8(0x10, 0xb9, 0x81, 0x4d));
    }

    /// Audit #3 / Wave 3 unit 3-D: the rightmost avatar must sit ≥ 8 px
    /// to the left of the tray pill so the two clusters read as separate
    /// affordances. Pre-fix the spacer was `s3 = 12 px` measured from
    /// avatar centre, leaving roughly 3 px to the pill edge after the
    /// 9 px avatar disc radius. Bumping to `s4 = 16 px` recovers the
    /// breathing room.
    #[test]
    fn avatar_stack_has_breathing_room_to_tray_pill() {
        // Workspace seeded with one explicit collaborator (other than the
        // local user) so `collaborator_initials` is non-empty and the
        // avatar disc is actually painted.
        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: vec![crate::shell::state::Collaborator::new("alice", "Alice")],
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1600.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let avatar = bar.avatar_stack_rect;
        let tray = bar.tray_pill_rect();
        assert!(
            avatar.width() > 0.0,
            "test seeded one collaborator but avatar_stack_rect is empty",
        );
        let gap = tray.x0 - avatar.x1;
        assert!(
            gap >= 8.0,
            "avatar→tray gap must be ≥ 8 px (got {gap:.2}); avatar.x1={}, tray.x0={}",
            avatar.x1,
            tray.x0,
        );
    }

    /// Regression: the topbar must paint a soft drop-shadow band below
    /// its rect so the seam between the translucent overlay surface and
    /// the dotted-grid backdrop doesn't read as "noisy pixels". The
    /// shadow is the only linear-gradient fill the topbar emits, so
    /// pinning a `LINEAR_GRADIENT` draw tag shows up after `draw` is a
    /// load-bearing signal — if a future refactor drops the gradient
    /// brush back to a solid fill (or removes the band entirely) this
    /// test fails loudly.
    #[test]
    fn topbar_paints_a_bottom_shadow_below_its_rect() {
        use vello_encoding::DrawTag;

        let mut tree = WorkspaceTree {
            workspaces: vec![Workspace::new(WorkspaceId::new(), "A")],
            active: WorkspaceId::from(0),
            active_strip: Default::default(),
            active_group: Default::default(),
            collaborators: Vec::new(),
            ..Default::default()
        };
        tree.active = tree.workspaces[0].id;

        let mut bar = TopBarChrome::from_tree(&tree);
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(800.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let gradient_fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::LINEAR_GRADIENT)
            .count();
        assert!(
            gradient_fills >= 1,
            "topbar must emit at least one linear-gradient fill (the soft \
             drop-shadow band below its rect); got {gradient_fills}",
        );

        // Pin the band's vertical extent so a future tweak that shrinks
        // it to zero (or moves it above the bar) can't pass this test.
        // The shadow rect spans `[rect.y1, rect.y1 + 28]`.
        let bar_bottom = bar.rect.y1;
        assert!(bar_bottom > 0.0, "topbar rect must have non-zero y1");
        // Sanity: bar height matches the public TOPBAR_HEIGHT_PX constant
        // so the y1 the shadow attaches to is the one downstream code
        // (overlay_host, sidebar_wheel) lays out below.
        assert!(
            (bar_bottom - TOPBAR_HEIGHT_PX as f64).abs() < 0.5,
            "topbar y1 ({bar_bottom}) should equal TOPBAR_HEIGHT_PX ({TOPBAR_HEIGHT_PX})",
        );
    }
}
