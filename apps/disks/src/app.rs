//! Root widget for the Disks app.
//!
//! `DisksApp` is a single self-drawing "canvas" widget: it owns a parsed
//! cache of the [`Topology`] (the source of truth lives in the CRDT) and
//! paints the whole UI — drive list, partition map, capacity bars, SMART
//! summary and the action confirm modal — directly into the Vello scene.
//! All mutable UI state (selection, active tab, pending confirmation) is
//! written back into the Automerge document so it is undoable and
//! collaborative.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use taffy::style_helpers::percent;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};

use crate::doc::DisksDoc;
use crate::model::{
    fmt_hours, fmt_size, AttrSeverity, Drive, DriveKind, PartRole, Partition, SmartStatus,
    Topology,
};

// ---- layout constants (logical px) ---------------------------------
const PAD: f64 = 18.0;
const TOPBAR_H: f64 = 60.0;
const LIST_W: f64 = 320.0;
const CARD_H: f64 = 96.0;
const CARD_GAP: f64 = 12.0;
const HEADER_H: f64 = 116.0;
const TAB_H: f64 = 40.0;

/// Below this allocated width (logical px) the drive list stops fitting
/// beside a readable detail pane, so it collapses from a docked column into
/// an overlay flap summoned by the hamburger. Matches the shared toolkit's
/// `DEFAULT_BREAKPOINT`.
const BREAKPOINT: f64 = 900.0;
/// The full slide width of the overlay drive list (its column plus the
/// padding gutters either side), so the eased slide and scrim line up.
const SIDEBAR_W: f64 = PAD + LIST_W + PAD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Smart,
    Partitions,
    Tools,
}

impl Tab {
    fn all() -> [Tab; 4] {
        [Tab::Overview, Tab::Smart, Tab::Partitions, Tab::Tools]
    }
    fn key(&self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Smart => "smart",
            Tab::Partitions => "partitions",
            Tab::Tools => "tools",
        }
    }
    fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Smart => "SMART & Health",
            Tab::Partitions => "Partitions",
            Tab::Tools => "Tools",
        }
    }
    fn from_key(k: &str) -> Tab {
        match k {
            "smart" => Tab::Smart,
            "partitions" => Tab::Partitions,
            "tools" => Tab::Tools,
            _ => Tab::Overview,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Mount,
    Unmount,
    Format,
    Benchmark,
}

impl ActionKind {
    fn key(&self) -> &'static str {
        match self {
            ActionKind::Mount => "mount",
            ActionKind::Unmount => "unmount",
            ActionKind::Format => "format",
            ActionKind::Benchmark => "benchmark",
        }
    }
    fn label(&self) -> &'static str {
        match self {
            ActionKind::Mount => "Mount",
            ActionKind::Unmount => "Unmount",
            ActionKind::Format => "Format…",
            ActionKind::Benchmark => "Benchmark",
        }
    }
    fn from_key(k: &str) -> Option<ActionKind> {
        match k {
            "mount" => Some(ActionKind::Mount),
            "unmount" => Some(ActionKind::Unmount),
            "format" => Some(ActionKind::Format),
            "benchmark" => Some(ActionKind::Benchmark),
            _ => None,
        }
    }
    /// Destructive actions require an explicit confirm dialog.
    fn destructive(&self) -> bool {
        matches!(self, ActionKind::Format)
    }
    fn confirm_title(&self, drive: &str) -> String {
        match self {
            ActionKind::Format => format!("Format {drive}?"),
            ActionKind::Unmount => format!("Unmount {drive}?"),
            ActionKind::Mount => format!("Mount {drive}?"),
            ActionKind::Benchmark => format!("Benchmark {drive}?"),
        }
    }
    fn confirm_body(&self, drive: &str) -> String {
        match self {
            ActionKind::Format => format!(
                "Formatting erases ALL data on {drive}. This cannot be undone. \
                 (Demo build — no changes are written to disk.)"
            ),
            ActionKind::Unmount => format!(
                "Detach {drive} from the filesystem. Programs using it may lose access."
            ),
            ActionKind::Mount => format!("Attach {drive} so its files become available."),
            ActionKind::Benchmark => format!(
                "Run a read/write throughput benchmark on {drive}. May briefly slow the system."
            ),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    action: ActionKind,
    drive: usize,
}

/// Computed interactive rectangles for one frame. Pure function of the
/// viewport + current state, so `draw` and `handle_event` agree without
/// any ordering hazard.
struct Layout {
    /// How the drive list relates to the detail at this width: a permanent
    /// docked column (wide) or an overlay flap over the content (narrow).
    split: SplitLayout,
    /// The drive-list panel rect — its column plus gutters. In overlay mode
    /// this slides in from the left edge under the eased reveal.
    sidebar: Rect,
    /// Dimming scrim behind the open overlay flap (`ZERO` when not overlaid).
    scrim: Rect,
    /// Hamburger toggle in the top bar (only painted/hit in overlay mode).
    hamburger: Option<Rect>,
    refresh: Rect,
    cards: Vec<Rect>,
    header: Rect,
    actions: Vec<(Rect, ActionKind)>,
    tabs: Vec<(Rect, Tab)>,
    content: Rect,
    modal: Option<ModalLayout>,
    tool_buttons: Vec<(Rect, ActionKind)>,
}

#[derive(Clone, Copy)]
struct ModalLayout {
    card: Rect,
    cancel: Rect,
    confirm: Rect,
}

pub struct DisksApp {
    topo: Topology,
    topo_json_cache: String,
    selected: usize,
    tab: Tab,
    pending: Option<Pending>,
    status: String,
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    hover_card: Option<usize>,
    hover_action: Option<ActionKind>,
    hover_tab: Option<Tab>,
    /// Eased open/closed state of the drive-list flap. Open by default when
    /// the list is docked (wide window); closed by default when it collapses
    /// to an overlay (narrow window). The same kernel the shared `NavSplit`
    /// widget drives — adopted directly here because the whole app is one
    /// self-drawing canvas rather than a tree of `ui` elements.
    flap: FlapState,
    /// Whether a hamburger toggle is hovered (overlay mode only).
    hover_menu: bool,
    /// `false` until the first layout pass resolves the width. Used to settle
    /// the flap to its resolved default on the very first frame (so a cold
    /// start at a narrow width opens straight to the detail, with no flash of
    /// the list sliding out).
    laid_out: bool,
}

impl DisksApp {
    pub fn new() -> Self {
        Self {
            topo: Topology::default(),
            topo_json_cache: String::new(),
            selected: 0,
            tab: Tab::Overview,
            pending: None,
            status: String::new(),
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            hover_card: None,
            hover_action: None,
            hover_tab: None,
            flap: FlapState::open_when_docked(),
            hover_menu: false,
            laid_out: false,
        }
    }

    /// The drive-list layout at the current width: a docked column beside the
    /// detail (wide) or an overlay flap over it (narrow).
    fn split_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), BREAKPOINT)
    }

    /// Force the drive-list flap to a settled open/closed state with no
    /// animation. The single-frame `--shots` renderer calls this so a narrow
    /// capture shows the overlay flap fully revealed over its scrim (rather
    /// than a frame caught mid-slide).
    pub fn snap_sidebar(&mut self, open: bool) {
        self.flap.snap(open);
        self.dirty = true;
    }

    /// Whether the drive list is currently open (resolving the user's
    /// preference against the layout default).
    fn list_open(&self) -> bool {
        self.flap.is_open(self.split_layout())
    }

    fn drive(&self) -> Option<&Drive> {
        self.topo.drives.get(self.selected)
    }

    // ---- layout --------------------------------------------------------
    fn compute_layout(&self) -> Layout {
        let v = self.rect;
        let refresh = Rect::new(v.x1 - PAD - 110.0, (TOPBAR_H - 32.0) / 2.0, v.x1 - PAD, (TOPBAR_H + 32.0) / 2.0);

        // Responsive split: in a wide window the drive list is a permanent
        // column beside the detail; in a narrow window it collapses to an
        // overlay flap that the hamburger summons over the full-width detail.
        let split = self.split_layout();
        let (sidebar, scrim, hamburger, list_x0, detail_x0) = match split {
            SplitLayout::Docked => {
                // The list column eases in/out by the reveal so toggling it is
                // smooth; the detail follows the list's trailing edge.
                let inset = SIDEBAR_W * self.flap.reveal();
                let sidebar = Rect::new(v.x0, v.y0, v.x0 + inset, v.y1);
                let detail_x0 = v.x0 + inset + if inset > 0.5 { 0.0 } else { PAD };
                (sidebar, Rect::ZERO, None, v.x0 + PAD, detail_x0.max(v.x0 + PAD))
            }
            SplitLayout::Overlay => {
                // Detail spans the full width; the list slides in from the
                // left over a scrim. The hamburger lives in the top bar.
                let slide = self.flap.slide();
                let panel_x0 = v.x0 - SIDEBAR_W * (1.0 - slide);
                let sidebar = Rect::new(panel_x0, v.y0, panel_x0 + SIDEBAR_W, v.y1);
                let scrim = if self.flap.reveal() > 0.001 { v } else { Rect::ZERO };
                let ham = Rect::new(PAD, (TOPBAR_H - 30.0) / 2.0, PAD + 38.0, (TOPBAR_H + 30.0) / 2.0);
                (sidebar, scrim, Some(ham), panel_x0 + PAD, v.x0 + PAD)
            }
        };

        // Drive list cards — anchored to the (possibly sliding) list column.
        let mut cards = Vec::new();
        let mut cy = TOPBAR_H + PAD;
        for _ in &self.topo.drives {
            cards.push(Rect::new(list_x0, cy, list_x0 + LIST_W, cy + CARD_H));
            cy += CARD_H + CARD_GAP;
        }

        let dx0 = detail_x0;
        let dx1 = v.x1 - PAD;
        let region_top = TOPBAR_H + PAD;
        let header = Rect::new(dx0, region_top, dx1, region_top + HEADER_H);

        // Action buttons right-aligned along the header's lower edge.
        let mut actions = Vec::new();
        let btn_h = 32.0;
        let btn_y = header.y1 - 16.0 - btn_h;
        let mut bx = dx1 - 16.0;
        for a in [
            ActionKind::Benchmark,
            ActionKind::Format,
            ActionKind::Unmount,
            ActionKind::Mount,
        ] {
            let w = (a.label().len() as f64) * 8.0 + 28.0;
            let r = Rect::new(bx - w, btn_y, bx, btn_y + btn_h);
            actions.push((r, a));
            bx -= w + 8.0;
        }

        // Tab bar.
        let tab_top = header.y1 + PAD;
        let mut tabs = Vec::new();
        let mut tx = dx0;
        for t in Tab::all() {
            let w = (t.title().len() as f64) * 8.2 + 28.0;
            tabs.push((Rect::new(tx, tab_top, tx + w, tab_top + TAB_H), t));
            tx += w + 6.0;
        }

        let content = Rect::new(dx0, tab_top + TAB_H + PAD, dx1, v.y1 - PAD);

        // Tools-tab big buttons (only meaningful on that tab, but cheap).
        let mut tool_buttons = Vec::new();
        if self.tab == Tab::Tools {
            let tools = [
                ActionKind::Mount,
                ActionKind::Unmount,
                ActionKind::Benchmark,
                ActionKind::Format,
            ];
            let col_w = (content.width() - 16.0) / 2.0;
            let row_h = 92.0;
            for (i, a) in tools.iter().enumerate() {
                let col = (i % 2) as f64;
                let row = (i / 2) as f64;
                let x = content.x0 + col * (col_w + 16.0);
                let y = content.y0 + row * (row_h + 14.0);
                tool_buttons.push((Rect::new(x, y, x + col_w, y + row_h), *a));
            }
        }

        // Confirm modal.
        let modal = self.pending.map(|_| {
            let w = 460.0;
            let h = 210.0;
            let cx = (v.x0 + v.x1) / 2.0;
            let cy = (v.y0 + v.y1) / 2.0;
            let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
            let by = card.y1 - 18.0 - 36.0;
            let confirm = Rect::new(card.x1 - 18.0 - 130.0, by, card.x1 - 18.0, by + 36.0);
            let cancel = Rect::new(confirm.x0 - 12.0 - 110.0, by, confirm.x0 - 12.0, by + 36.0);
            ModalLayout { card, cancel, confirm }
        });

        Layout {
            split,
            sidebar,
            scrim,
            hamburger,
            refresh,
            cards,
            header,
            actions,
            tabs,
            content,
            modal,
            tool_buttons,
        }
    }

    // ---- drawing primitives -------------------------------------------
    fn txt(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        x: f64,
        top: f64,
        max_w: Option<f32>,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(scene, s, size, color, Affine::translate((x, top)), max_w, weight);
    }

    fn txt_right(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        right: f64,
        top: f64,
        weight: TextWeight,
    ) {
        let (w, _) = tr.measure_weighted(s, size, None, weight);
        self.txt(scene, tr, s, size, color, right - w as f64, top, None, weight);
    }

    fn txt_center(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        cx: f64,
        top: f64,
        weight: TextWeight,
    ) {
        let (w, _) = tr.measure_weighted(s, size, None, weight);
        self.txt(scene, tr, s, size, color, cx - w as f64 / 2.0, top, None, weight);
    }

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(Fill::NonZero, t, c, None, &RoundedRect::from_rect(r, radius));
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(&Stroke::new(w), t, c, None, &RoundedRect::from_rect(r, radius));
    }

    /// Track + filled bar. `frac` in 0..1.
    fn bar(scene: &mut Scene, t: Affine, r: Rect, frac: f64, track: Color, fill: Color) {
        let radius = (r.height() / 2.0).min(6.0);
        Self::fill_rr(scene, t, r, radius, track);
        let frac = frac.clamp(0.0, 1.0);
        if frac > 0.0 {
            let w = (r.width() * frac).max(radius * 2.0).min(r.width());
            let fr = Rect::new(r.x0, r.y0, r.x0 + w, r.y1);
            Self::fill_rr(scene, t, fr, radius, fill);
        }
    }

    // ---- top bar -------------------------------------------------------
    fn draw_topbar(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, TOPBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new(Point::new(bar.x0, bar.y1), Point::new(bar.x1, bar.y1)),
        );

        // Hamburger toggle (narrow window only): summons the drive-list flap.
        // It nudges the title rightward so the two never collide.
        let title_x = if let Some(hb) = lay.hamburger {
            let hov = self.hover_menu;
            Self::fill_rr(scene, t, hb, 8.0, if hov { c.neutral[3] } else { c.neutral[2] });
            Self::stroke_rr(scene, t, hb, 8.0, c.border_standard, 1.0);
            let lx = hb.x0 + 11.0;
            for i in 0..3 {
                let ly = hb.y0 + 10.0 + i as f64 * 5.0;
                scene.stroke(
                    &Stroke::new(2.0),
                    t,
                    c.text_primary,
                    None,
                    &Line::new(Point::new(lx, ly), Point::new(hb.x1 - 11.0, ly)),
                );
            }
            hb.x1 + 12.0
        } else {
            PAD
        };
        self.txt(scene, tr, "Disks", 22.0, c.text_primary, title_x, 14.0, None, TextWeight::Bold);
        let summary = format!(
            "{} drives · {} total · {} in use",
            self.topo.drives.len(),
            fmt_size(self.topo.total_size()),
            fmt_size(self.topo.total_used()),
        );
        self.txt(scene, tr, &summary, 13.0, c.text_dim, title_x + 74.0, 21.0, None, TextWeight::Regular);

        // Refresh button.
        let r = lay.refresh;
        let hov = r.contains(self.last_mouse);
        Self::fill_rr(scene, t, r, 8.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, r, 8.0, c.border_standard, 1.0);
        self.txt_center(scene, tr, "↻ Rescan", 13.0, c.text_primary, (r.x0 + r.x1) / 2.0, r.y0 + 8.0, TextWeight::Medium);
    }

    // ---- drive list ----------------------------------------------------
    fn draw_drive_list(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        for (i, drive) in self.topo.drives.iter().enumerate() {
            let Some(&r) = lay.cards.get(i) else { continue };
            let selected = i == self.selected;
            let hover = self.hover_card == Some(i);
            let bg = if selected {
                c.accent[2]
            } else if hover {
                c.neutral[3]
            } else {
                c.surfaces.raised
            };
            Self::fill_rr(scene, t, r, 12.0, bg);
            Self::stroke_rr(scene, t, r, 12.0, if selected { c.border_focus } else { c.border_standard }, if selected { 1.6 } else { 1.0 });

            // Kind badge.
            let badge = Rect::new(r.x0 + 14.0, r.y0 + 14.0, r.x0 + 14.0 + 52.0, r.y0 + 14.0 + 22.0);
            Self::fill_rr(scene, t, badge, 6.0, kind_color(theme, drive.kind));
            self.txt_center(scene, tr, drive.kind.tag(), 12.0, Color::WHITE, (badge.x0 + badge.x1) / 2.0, badge.y0 + 4.0, TextWeight::Bold);

            // Model + name + size.
            let tx = r.x0 + 14.0;
            self.txt(scene, tr, &drive.model, 15.0, c.text_primary, tx, r.y0 + 42.0, Some((r.width() - 28.0) as f32), TextWeight::Semibold);
            let sub = format!("/dev/{}  ·  {}", drive.name, fmt_size(drive.size));
            self.txt(scene, tr, &sub, 12.0, c.text_dim, tx, r.y0 + 63.0, None, TextWeight::Regular);

            // Mini usage bar across drive.
            let used_frac = if drive.size > 0 {
                drive.used_total() as f64 / drive.size as f64
            } else {
                0.0
            };
            let barr = Rect::new(tx, r.y1 - 16.0, r.x1 - 14.0, r.y1 - 10.0);
            Self::bar(scene, t, barr, used_frac, c.neutral[4], usage_color(theme, used_frac));

            // Health dot.
            let dot_c = status_color(theme, drive.smart_status());
            let dot = Rect::new(r.x1 - 26.0, r.y0 + 16.0, r.x1 - 14.0, r.y0 + 28.0);
            Self::fill_rr(scene, t, dot, 6.0, dot_c);
        }
    }

    // ---- detail header -------------------------------------------------
    fn draw_header(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout, drive: &Drive) {
        let c = &theme.tokens.colors;
        let h = lay.header;
        Self::fill_rr(scene, t, h, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, h, 14.0, c.border_standard, 1.0);

        let tx = h.x0 + 20.0;
        self.txt(scene, tr, &drive.model, 22.0, c.text_primary, tx, h.y0 + 16.0, None, TextWeight::Bold);

        // kind badge next to model is overkill; show meta line.
        let meta = format!(
            "{}  ·  /dev/{}  ·  {}{}",
            drive.kind.long(),
            drive.name,
            fmt_size(drive.size),
            drive
                .serial
                .as_ref()
                .map(|s| format!("  ·  S/N {s}"))
                .unwrap_or_default(),
        );
        self.txt(scene, tr, &meta, 13.0, c.text_dim, tx, h.y0 + 46.0, Some((h.width() - 40.0) as f32), TextWeight::Regular);

        // Health + connection chips.
        let chip_y = h.y0 + 70.0;
        let status = drive.smart_status();
        self.draw_chip(scene, tr, theme, t, tx, chip_y, &format!("● {}", status.label()), status_color(theme, status));
        let tran = drive.tran.clone().unwrap_or_else(|| "—".into()).to_uppercase();
        self.draw_chip(scene, tr, theme, t, tx + 132.0, chip_y, &tran, c.neutral[5]);
        if drive.removable {
            self.draw_chip(scene, tr, theme, t, tx + 132.0 + 86.0, chip_y, "REMOVABLE", c.semantic.info);
        }

        // Action buttons.
        for (r, a) in &lay.actions {
            let hov = self.hover_action == Some(*a);
            let (bg, fg) = if a.destructive() {
                (if hov { c.semantic.danger } else { with_alpha(c.semantic.danger, 40) }, if hov { Color::WHITE } else { c.semantic.danger })
            } else {
                (if hov { c.accent[4] } else { c.neutral[2] }, c.text_primary)
            };
            Self::fill_rr(scene, t, *r, 8.0, bg);
            Self::stroke_rr(scene, t, *r, 8.0, c.border_standard, 1.0);
            self.txt_center(scene, tr, a.label(), 13.0, fg, (r.x0 + r.x1) / 2.0, r.y0 + 8.0, TextWeight::Medium);
        }
    }

    fn draw_chip(&self, scene: &mut Scene, tr: &mut TextRenderer, _theme: &Theme, t: Affine, x: f64, y: f64, label: &str, color: Color) {
        let (w, _) = tr.measure_weighted(label, 12.0, None, TextWeight::Semibold);
        let r = Rect::new(x, y, x + w as f64 + 22.0, y + 24.0);
        Self::fill_rr(scene, t, r, 12.0, with_alpha(color, 38));
        Self::stroke_rr(scene, t, r, 12.0, with_alpha(color, 120), 1.0);
        self.txt(scene, tr, label, 12.0, color, x + 11.0, y + 5.0, None, TextWeight::Semibold);
    }

    // ---- tab bar -------------------------------------------------------
    fn draw_tabs(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        for (r, tab) in &lay.tabs {
            let active = *tab == self.tab;
            let hover = self.hover_tab == Some(*tab);
            if active {
                Self::fill_rr(scene, t, *r, 9.0, c.accent[5]);
            } else if hover {
                Self::fill_rr(scene, t, *r, 9.0, c.neutral[3]);
            }
            let fg = if active { Color::WHITE } else { c.text_dim };
            self.txt_center(scene, tr, tab.title(), 13.0, fg, (r.x0 + r.x1) / 2.0, r.y0 + 11.0, if active { TextWeight::Semibold } else { TextWeight::Medium });
        }
    }

    // ---- content: overview --------------------------------------------
    fn draw_overview(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, area: Rect, drive: &Drive) {
        let c = &theme.tokens.colors;
        // Partition map section.
        self.txt(scene, tr, "PARTITION MAP", 11.0, c.text_dim, area.x0, area.y0, None, TextWeight::Semibold);
        let map = Rect::new(area.x0, area.y0 + 20.0, area.x1, area.y0 + 20.0 + 58.0);
        self.draw_partition_map(scene, tr, theme, t, map, drive);
        // Legend.
        let legend_y = map.y1 + 12.0;
        self.draw_legend(scene, tr, theme, t, area.x0, legend_y, area.x1, drive);

        // Two columns below: capacity (left) + SMART summary (right).
        let cols_top = legend_y + 56.0;
        let gap = 16.0;
        let left_w = (area.width() - gap) * 0.56;
        let left = Rect::new(area.x0, cols_top, area.x0 + left_w, area.y1);
        let right = Rect::new(left.x1 + gap, cols_top, area.x1, area.y1);

        self.draw_capacity(scene, tr, theme, t, left, drive);
        self.draw_smart_card(scene, tr, theme, t, right, drive);
    }

    fn draw_partition_map(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, map: Rect, drive: &Drive) {
        let c = &theme.tokens.colors;
        // Background track for the whole device.
        Self::fill_rr(scene, t, map, 8.0, c.neutral[2]);

        let total = drive.size.max(1) as f64;
        let mut x = map.x0;
        let inner_pad = 0.0_f64;
        let n_segs = drive.partitions.len();
        for (i, p) in drive.partitions.iter().enumerate() {
            let frac = p.size as f64 / total;
            let mut w = frac * map.width();
            // Keep tiny partitions visible.
            if w < 6.0 {
                w = 6.0;
            }
            let is_last_seg = i == n_segs - 1 && drive.free_tail() == 0;
            let seg = Rect::new(x + inner_pad, map.y0 + 4.0, x + w - 2.0, map.y1 - 4.0);
            let col = role_color(theme, p.role());
            let radius = if i == 0 || is_last_seg { 6.0 } else { 3.0 };
            Self::fill_rr(scene, t, seg, radius, col);
            // Inner used overlay (darker band at bottom showing fill).
            if let Some(f) = p.used_frac() {
                let uh = (seg.height() - 6.0) * f;
                let used_r = Rect::new(seg.x0, seg.y1 - 3.0 - uh, seg.x1, seg.y1 - 3.0);
                Self::fill_rr(scene, t, used_r, 2.0, with_alpha(Color::BLACK, 70));
            }
            // Label inside if wide enough.
            if seg.width() > 64.0 {
                self.txt(scene, tr, &p.display_label(), 11.0, Color::WHITE, seg.x0 + 8.0, seg.y0 + 8.0, Some((seg.width() - 12.0) as f32), TextWeight::Semibold);
                self.txt(scene, tr, &fmt_size(p.size), 11.0, with_alpha(Color::WHITE, 200), seg.x0 + 8.0, seg.y0 + 24.0, None, TextWeight::Regular);
            }
            x += w;
        }
        // Free tail.
        let tail = drive.free_tail();
        if tail > 0 && x < map.x1 - 2.0 {
            let seg = Rect::new(x, map.y0 + 4.0, map.x1 - 2.0, map.y1 - 4.0);
            Self::fill_rr(scene, t, seg, 6.0, role_color(theme, PartRole::Free));
            if seg.width() > 64.0 {
                self.txt(scene, tr, "Free", 11.0, c.text_dim, seg.x0 + 8.0, seg.y0 + 8.0, None, TextWeight::Semibold);
                self.txt(scene, tr, &fmt_size(tail), 11.0, c.text_dim, seg.x0 + 8.0, seg.y0 + 24.0, None, TextWeight::Regular);
            }
        }
    }

    fn draw_legend(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, x0: f64, y: f64, x1: f64, drive: &Drive) {
        let c = &theme.tokens.colors;
        let mut x = x0;
        let row2_y = y + 22.0;
        let mut row = 0;
        let mut place = |label: String, color: Color, sub: String, this: &Self, scene: &mut Scene, tr: &mut TextRenderer| {
            let yy = if row < 4 { y } else { row2_y };
            if x > x1 - 150.0 {
                // wrap
            }
            let sw = Rect::new(x, yy + 1.0, x + 12.0, yy + 13.0);
            Self::fill_rr(scene, t, sw, 3.0, color);
            this.txt(scene, tr, &label, 11.5, c.text_primary, x + 18.0, yy, None, TextWeight::Medium);
            this.txt(scene, tr, &sub, 11.0, c.text_dim, x + 18.0, yy + 14.0, None, TextWeight::Regular);
            let advance = (label.len().max(sub.len()) as f64) * 6.5 + 34.0;
            x += advance.max(120.0);
            row += 1;
        };
        for p in &drive.partitions {
            place(p.display_label(), role_color(theme, p.role()), format!("{}  {}", p.role().legend(), fmt_size(p.size)), self, scene, tr);
        }
        if drive.free_tail() > 0 {
            place("Free".into(), role_color(theme, PartRole::Free), fmt_size(drive.free_tail()), self, scene, tr);
        }
    }

    fn draw_capacity(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, area: Rect, drive: &Drive) {
        let c = &theme.tokens.colors;
        self.txt(scene, tr, "FILESYSTEMS & USAGE", 11.0, c.text_dim, area.x0, area.y0, None, TextWeight::Semibold);
        let mut y = area.y0 + 24.0;
        let mounted: Vec<&Partition> = drive
            .partitions
            .iter()
            .filter(|p| p.mountpoint.is_some())
            .collect();
        if mounted.is_empty() {
            self.txt(scene, tr, "No mounted filesystems.", 13.0, c.text_dim, area.x0, y, None, TextWeight::Regular);
            return;
        }
        for p in mounted {
            if y > area.y1 - 40.0 {
                break;
            }
            let mp = p.mountpoint.clone().unwrap_or_default();
            self.txt(scene, tr, &p.display_label(), 13.5, c.text_primary, area.x0, y, None, TextWeight::Semibold);
            self.txt_right(scene, tr, &mp, 12.0, c.text_dim, area.x1, y + 1.0, TextWeight::Regular);
            let barr = Rect::new(area.x0, y + 22.0, area.x1, y + 32.0);
            let frac = p.used_frac().unwrap_or(0.0);
            Self::bar(scene, t, barr, frac, c.neutral[3], usage_color(theme, frac));
            let detail = match p.used {
                Some(u) => format!(
                    "{} used of {}  ·  {} free  ·  {}",
                    fmt_size(u),
                    fmt_size(p.size),
                    fmt_size(p.size.saturating_sub(u)),
                    p.fstype.clone().unwrap_or_default(),
                ),
                None => format!("{}  ·  {}", fmt_size(p.size), p.fstype.clone().unwrap_or_default()),
            };
            self.txt(scene, tr, &detail, 11.5, c.text_dim, area.x0, y + 36.0, None, TextWeight::Regular);
            y += 62.0;
        }
    }

    fn draw_smart_card(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, area: Rect, drive: &Drive) {
        let c = &theme.tokens.colors;
        Self::fill_rr(scene, t, area, 12.0, c.neutral[2]);
        Self::stroke_rr(scene, t, area, 12.0, c.border_standard, 1.0);
        let pad = 16.0;
        let x = area.x0 + pad;
        let status = drive.smart_status();
        self.txt(scene, tr, "SMART HEALTH", 11.0, c.text_dim, x, area.y0 + pad, None, TextWeight::Semibold);

        // Status pill.
        let sc = status_color(theme, status);
        let pill = Rect::new(x, area.y0 + pad + 22.0, x + 150.0, area.y0 + pad + 22.0 + 30.0);
        Self::fill_rr(scene, t, pill, 8.0, with_alpha(sc, 45));
        Self::stroke_rr(scene, t, pill, 8.0, sc, 1.2);
        self.txt(scene, tr, status.label(), 15.0, sc, x + 12.0, pill.y0 + 7.0, None, TextWeight::Bold);

        let smart = match &drive.smart {
            Some(s) => s,
            None => {
                self.txt(scene, tr, "No SMART data available.", 12.0, c.text_dim, x, pill.y1 + 14.0, None, TextWeight::Regular);
                return;
            }
        };

        // Temperature gauge.
        let mut yy = pill.y1 + 18.0;
        if let Some(temp) = smart.temp_c {
            self.txt(scene, tr, "Temperature", 12.0, c.text_dim, x, yy, None, TextWeight::Regular);
            self.txt_right(scene, tr, &format!("{temp} °C"), 13.0, c.text_primary, area.x1 - pad, yy, TextWeight::Semibold);
            let frac = ((temp as f64 - 20.0) / 50.0).clamp(0.0, 1.0);
            let barr = Rect::new(x, yy + 18.0, area.x1 - pad, yy + 26.0);
            Self::bar(scene, t, barr, frac, c.neutral[3], temp_color(theme, temp));
            yy += 40.0;
        }

        // Stat rows.
        let stat = |label: &str, val: String, this: &Self, scene: &mut Scene, tr: &mut TextRenderer, yy: &mut f64| {
            if *yy > area.y1 - 22.0 {
                return;
            }
            this.txt(scene, tr, label, 12.0, c.text_dim, x, *yy, None, TextWeight::Regular);
            this.txt_right(scene, tr, &val, 12.5, c.text_primary, area.x1 - pad, *yy, TextWeight::Semibold);
            *yy += 22.0;
        };
        if let Some(h) = smart.power_on_hours {
            stat("Power-on time", format!("{} ({} h)", fmt_hours(h), h), self, scene, tr, &mut yy);
        }
        if let Some(pc) = smart.power_cycles {
            stat("Power cycles", pc.to_string(), self, scene, tr, &mut yy);
        }
        if let Some(re) = smart.reallocated {
            stat("Reallocated sectors", re.to_string(), self, scene, tr, &mut yy);
        }
        if let Some(pu) = smart.pct_used {
            stat("Wear (NVMe used)", format!("{pu} %"), self, scene, tr, &mut yy);
        }
        if let Some(w) = smart.written_gb {
            stat("Host writes", fmt_size((w as u64) * 1_000_000_000), self, scene, tr, &mut yy);
        }
    }

    // ---- content: SMART tab -------------------------------------------
    fn draw_smart_tab(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, area: Rect, drive: &Drive) {
        let c = &theme.tokens.colors;
        let status = drive.smart_status();
        let sc = status_color(theme, status);
        // Banner.
        let banner = Rect::new(area.x0, area.y0, area.x1, area.y0 + 64.0);
        Self::fill_rr(scene, t, banner, 12.0, with_alpha(sc, 38));
        Self::stroke_rr(scene, t, banner, 12.0, with_alpha(sc, 120), 1.0);
        self.txt(scene, tr, &format!("● Overall self-assessment: {}", status.label()), 17.0, sc, banner.x0 + 18.0, banner.y0 + 12.0, None, TextWeight::Bold);
        let sub = match status {
            SmartStatus::Passed => "All monitored attributes are within nominal thresholds.",
            SmartStatus::Warning => "One or more attributes need attention — back up important data.",
            SmartStatus::Failing => "Drive reports imminent failure. Replace it as soon as possible.",
            SmartStatus::Unknown => "This device does not expose SMART telemetry.",
        };
        self.txt(scene, tr, sub, 12.5, c.text_dim, banner.x0 + 18.0, banner.y0 + 38.0, None, TextWeight::Regular);

        let smart = match &drive.smart {
            Some(s) => s,
            None => return,
        };

        // Stat tiles row.
        let tiles_top = banner.y1 + 16.0;
        let tile_h = 78.0;
        let tiles: Vec<(String, String)> = vec![
            ("Temperature".into(), smart.temp_c.map(|v| format!("{v} °C")).unwrap_or("—".into())),
            ("Power-on hours".into(), smart.power_on_hours.map(|v| v.to_string()).unwrap_or("—".into())),
            ("Power cycles".into(), smart.power_cycles.map(|v| v.to_string()).unwrap_or("—".into())),
            ("Reallocated".into(), smart.reallocated.map(|v| v.to_string()).unwrap_or("—".into())),
            ("Wear used".into(), smart.pct_used.map(|v| format!("{v} %")).unwrap_or("—".into())),
        ];
        let n = tiles.len() as f64;
        let gap = 12.0;
        let tw = (area.width() - gap * (n - 1.0)) / n;
        for (i, (label, val)) in tiles.iter().enumerate() {
            let tx = area.x0 + i as f64 * (tw + gap);
            let r = Rect::new(tx, tiles_top, tx + tw, tiles_top + tile_h);
            Self::fill_rr(scene, t, r, 10.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, r, 10.0, c.border_standard, 1.0);
            self.txt(scene, tr, label, 11.0, c.text_dim, r.x0 + 12.0, r.y0 + 12.0, None, TextWeight::Medium);
            self.txt(scene, tr, val, 22.0, c.text_primary, r.x0 + 12.0, r.y0 + 34.0, None, TextWeight::Bold);
        }

        // Attribute table.
        let table_top = tiles_top + tile_h + 18.0;
        self.txt(scene, tr, "SMART ATTRIBUTES", 11.0, c.text_dim, area.x0, table_top, None, TextWeight::Semibold);
        let header_y = table_top + 20.0;
        let id_x = area.x0 + 4.0;
        let name_x = area.x0 + 54.0;
        let val_x = area.x1 - 220.0;
        let stat_x = area.x1 - 90.0;
        self.txt(scene, tr, "ID", 11.0, c.text_dim, id_x, header_y, None, TextWeight::Semibold);
        self.txt(scene, tr, "ATTRIBUTE", 11.0, c.text_dim, name_x, header_y, None, TextWeight::Semibold);
        self.txt(scene, tr, "VALUE", 11.0, c.text_dim, val_x, header_y, None, TextWeight::Semibold);
        self.txt(scene, tr, "STATUS", 11.0, c.text_dim, stat_x, header_y, None, TextWeight::Semibold);
        let mut ry = header_y + 22.0;
        for (i, a) in smart.attrs.iter().enumerate() {
            if ry > area.y1 - 20.0 {
                break;
            }
            if i % 2 == 0 {
                Self::fill_rr(scene, t, Rect::new(area.x0, ry - 4.0, area.x1, ry + 20.0), 4.0, with_alpha(Color::WHITE, 8));
            }
            self.txt(scene, tr, &a.id.to_string(), 12.0, c.text_dim, id_x, ry, None, TextWeight::Regular);
            self.txt(scene, tr, &a.name, 12.5, c.text_primary, name_x, ry, Some((val_x - name_x - 12.0) as f32), TextWeight::Regular);
            self.txt(scene, tr, &a.value, 12.5, c.text_primary, val_x, ry, None, TextWeight::Medium);
            let (sl, scl) = match a.severity {
                AttrSeverity::Ok => ("OK", c.semantic.success),
                AttrSeverity::Warn => ("WARN", c.semantic.warning),
                AttrSeverity::Crit => ("CRIT", c.semantic.danger),
            };
            self.txt(scene, tr, sl, 11.5, scl, stat_x, ry, None, TextWeight::Bold);
            ry += 24.0;
        }
    }

    // ---- content: partitions tab --------------------------------------
    fn draw_partitions_tab(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, area: Rect, drive: &Drive) {
        let c = &theme.tokens.colors;
        let name_x = area.x0 + 6.0;
        let label_x = area.x0 + 140.0;
        let fs_x = area.x0 + 290.0;
        let size_x = area.x0 + 380.0;
        let mount_x = area.x0 + 470.0;
        let used_x = area.x1 - 6.0;
        let hy = area.y0;
        self.txt(scene, tr, "DEVICE", 11.0, c.text_dim, name_x, hy, None, TextWeight::Semibold);
        self.txt(scene, tr, "LABEL", 11.0, c.text_dim, label_x, hy, None, TextWeight::Semibold);
        self.txt(scene, tr, "FS", 11.0, c.text_dim, fs_x, hy, None, TextWeight::Semibold);
        self.txt(scene, tr, "SIZE", 11.0, c.text_dim, size_x, hy, None, TextWeight::Semibold);
        self.txt(scene, tr, "MOUNT", 11.0, c.text_dim, mount_x, hy, None, TextWeight::Semibold);
        self.txt_right(scene, tr, "USED", 11.0, c.text_dim, used_x, hy, TextWeight::Semibold);

        let mut ry = hy + 24.0;
        for (i, p) in drive.partitions.iter().enumerate() {
            let rowr = Rect::new(area.x0, ry - 6.0, area.x1, ry + 30.0);
            if i % 2 == 0 {
                Self::fill_rr(scene, t, rowr, 6.0, with_alpha(Color::WHITE, 8));
            }
            // role swatch
            let sw = Rect::new(area.x0 - 0.0, ry + 2.0, area.x0 + 4.0, ry + 18.0);
            Self::fill_rr(scene, t, sw, 2.0, role_color(theme, p.role()));
            self.txt(scene, tr, &p.name, 13.0, c.text_primary, name_x, ry, None, TextWeight::Medium);
            self.txt(scene, tr, &p.label.clone().unwrap_or_else(|| "—".into()), 12.5, c.text_dim, label_x, ry, Some((fs_x - label_x - 8.0) as f32), TextWeight::Regular);
            self.txt(scene, tr, &p.fstype.clone().unwrap_or_else(|| "—".into()), 12.5, c.text_dim, fs_x, ry, None, TextWeight::Regular);
            self.txt(scene, tr, &fmt_size(p.size), 12.5, c.text_primary, size_x, ry, None, TextWeight::Regular);
            self.txt(scene, tr, &p.mountpoint.clone().unwrap_or_else(|| "not mounted".into()), 12.5, c.text_dim, mount_x, ry, Some((used_x - mount_x - 70.0) as f32), TextWeight::Regular);
            // usage mini-bar + pct on the right
            if let Some(f) = p.used_frac() {
                let barr = Rect::new(used_x - 64.0, ry + 6.0, used_x, ry + 14.0);
                Self::bar(scene, t, barr, f, c.neutral[3], usage_color(theme, f));
                self.txt_right(scene, tr, &format!("{:.0}%", f * 100.0), 11.0, c.text_dim, used_x, ry - 8.0, TextWeight::Regular);
            } else {
                self.txt_right(scene, tr, "—", 12.0, c.text_dim, used_x, ry, TextWeight::Regular);
            }
            ry += 38.0;
        }
    }

    // ---- content: tools tab -------------------------------------------
    fn draw_tools_tab(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, _area: Rect, lay: &Layout) {
        let c = &theme.tokens.colors;
        for (r, a) in &lay.tool_buttons {
            let hov = self.hover_action == Some(*a);
            let accent = if a.destructive() { c.semantic.danger } else { c.accent[5] };
            Self::fill_rr(scene, t, *r, 12.0, if hov { c.neutral[3] } else { c.surfaces.raised });
            Self::stroke_rr(scene, t, *r, 12.0, if hov { accent } else { c.border_standard }, if hov { 1.5 } else { 1.0 });
            // accent stripe
            Self::fill_rr(scene, t, Rect::new(r.x0, r.y0, r.x0 + 5.0, r.y1), 12.0, accent);
            self.txt(scene, tr, a.label().trim_end_matches('…'), 16.0, c.text_primary, r.x0 + 18.0, r.y0 + 16.0, None, TextWeight::Semibold);
            let desc = match a {
                ActionKind::Mount => "Attach the selected filesystem so its files are reachable.",
                ActionKind::Unmount => "Safely detach the filesystem before removing the device.",
                ActionKind::Benchmark => "Measure sequential read / write throughput and access time.",
                ActionKind::Format => "Erase and create a new filesystem (destructive).",
            };
            self.txt(scene, tr, desc, 12.0, c.text_dim, r.x0 + 18.0, r.y0 + 42.0, Some((r.width() - 36.0) as f32), TextWeight::Regular);
            self.txt(scene, tr, "Click to configure →", 11.5, accent, r.x0 + 18.0, r.y1 - 22.0, None, TextWeight::Medium);
        }
    }

    // ---- modal ---------------------------------------------------------
    fn draw_modal(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, ml: &ModalLayout, pending: Pending) {
        let c = &theme.tokens.colors;
        // Scrim.
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let card = ml.card;
        Self::fill_rr(scene, t, card, 16.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 16.0, c.border_standard, 1.0);
        let drive_name = self
            .topo
            .drives
            .get(pending.drive)
            .map(|d| format!("/dev/{}", d.name))
            .unwrap_or_else(|| "device".into());
        let danger = pending.action.destructive();
        let title_c = if danger { c.semantic.danger } else { c.text_primary };
        self.txt(scene, tr, &pending.action.confirm_title(&drive_name), 19.0, title_c, card.x0 + 22.0, card.y0 + 22.0, None, TextWeight::Bold);
        self.txt(scene, tr, &pending.action.confirm_body(&drive_name), 13.0, c.text_dim, card.x0 + 22.0, card.y0 + 56.0, Some((card.width() - 44.0) as f32), TextWeight::Regular);

        // Cancel.
        let cancel_hov = ml.cancel.contains(self.last_mouse);
        Self::fill_rr(scene, t, ml.cancel, 9.0, if cancel_hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, ml.cancel, 9.0, c.border_standard, 1.0);
        self.txt_center(scene, tr, "Cancel", 14.0, c.text_primary, (ml.cancel.x0 + ml.cancel.x1) / 2.0, ml.cancel.y0 + 9.0, TextWeight::Medium);

        // Confirm.
        let confirm_hov = ml.confirm.contains(self.last_mouse);
        let cc = if danger { c.semantic.danger } else { c.accent[5] };
        Self::fill_rr(scene, t, ml.confirm, 9.0, if confirm_hov { cc } else { with_alpha(cc, 220) });
        let label = if danger { "Erase device" } else { "Confirm" };
        self.txt_center(scene, tr, label, 14.0, Color::WHITE, (ml.confirm.x0 + ml.confirm.x1) / 2.0, ml.confirm.y0 + 9.0, TextWeight::Semibold);
    }

    // ---- status line ---------------------------------------------------
    fn draw_statusline(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, _t: Affine) {
        if self.status.is_empty() {
            return;
        }
        let c = &theme.tokens.colors;
        self.txt(scene, tr, &self.status, 12.0, c.semantic.info, PAD, self.rect.y1 - 22.0, None, TextWeight::Medium);
    }

    // ---- interaction ---------------------------------------------------
    fn trigger_action(&mut self, state: &mut AutoCommit, action: ActionKind) {
        if action.destructive() {
            self.pending = Some(Pending { action, drive: self.selected });
            DisksDoc::set_pending(state, &format!("{}:{}", action.key(), self.selected));
        } else {
            // Non-destructive demo actions: just report on the status line.
            let name = self
                .drive()
                .map(|d| format!("/dev/{}", d.name))
                .unwrap_or_default();
            let msg = match action {
                ActionKind::Mount => format!("Mounted {name} (demo — udisks call stubbed)"),
                ActionKind::Unmount => format!("Unmounted {name} (demo — udisks call stubbed)"),
                ActionKind::Benchmark => format!("Benchmark queued for {name} (demo)"),
                ActionKind::Format => unreachable!(),
            };
            self.status = msg.clone();
            DisksDoc::set_status(state, &msg);
        }
    }

    fn confirm_pending(&mut self, state: &mut AutoCommit) {
        if let Some(p) = self.pending.take() {
            let name = self
                .topo
                .drives
                .get(p.drive)
                .map(|d| format!("/dev/{}", d.name))
                .unwrap_or_default();
            let msg = format!("{} on {name} confirmed (demo build — not executed)", p.action.label().trim_end_matches('…'));
            self.status = msg.clone();
            DisksDoc::set_status(state, &msg);
        }
        DisksDoc::set_pending(state, "");
    }

    fn cancel_pending(&mut self, state: &mut AutoCommit) {
        self.pending = None;
        DisksDoc::set_pending(state, "");
    }

    fn select(&mut self, state: &mut AutoCommit, idx: usize) {
        if idx < self.topo.drives.len() {
            self.selected = idx;
            DisksDoc::set_selected(state, idx);
            self.dirty = true;
        }
    }

    fn set_tab(&mut self, state: &mut AutoCommit, tab: Tab) {
        self.tab = tab;
        DisksDoc::set_tab(state, tab.key());
        self.dirty = true;
    }
}

// ---- theme-derived colour helpers --------------------------------------
fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

fn kind_color(theme: &Theme, kind: DriveKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        DriveKind::Nvme => c.accent[5],
        DriveKind::Ssd => c.semantic.info,
        DriveKind::Hdd => c.neutral[5],
        DriveKind::Usb => c.semantic.success,
        DriveKind::Other => c.neutral[4],
    }
}

fn role_color(theme: &Theme, role: PartRole) -> Color {
    let c = &theme.tokens.colors;
    match role {
        PartRole::Efi => c.semantic.info,
        PartRole::Boot => c.accent[7],
        PartRole::Root => c.accent[5],
        PartRole::Home => c.semantic.success,
        PartRole::Swap => c.semantic.warning,
        PartRole::Data => c.accent[6],
        PartRole::Free => c.neutral[3],
    }
}

fn usage_color(theme: &Theme, frac: f64) -> Color {
    let c = &theme.tokens.colors;
    if frac >= 0.9 {
        c.semantic.danger
    } else if frac >= 0.7 {
        c.semantic.warning
    } else {
        c.semantic.success
    }
}

fn temp_color(theme: &Theme, temp: i64) -> Color {
    let c = &theme.tokens.colors;
    if temp >= 60 {
        c.semantic.danger
    } else if temp >= 48 {
        c.semantic.warning
    } else {
        c.semantic.info
    }
}

fn status_color(theme: &Theme, status: SmartStatus) -> Color {
    let c = &theme.tokens.colors;
    match status {
        SmartStatus::Passed => c.semantic.success,
        SmartStatus::Warning => c.semantic.warning,
        SmartStatus::Failing => c.semantic.danger,
        SmartStatus::Unknown => c.neutral[5],
    }
}

impl Widget for DisksApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if DisksDoc::is_empty(state) {
                let topo = if std::env::var("DISKS_LIVE").is_ok() {
                    crate::probe::probe().unwrap_or_else(Topology::sample)
                } else {
                    Topology::sample()
                };
                DisksDoc::seed(state, &topo);
            }
            self.seeded = true;
        }

        if let Some(json) = DisksDoc::topology_json(state) {
            if json != self.topo_json_cache {
                if let Ok(t) = serde_json::from_str::<Topology>(&json) {
                    self.topo = t;
                    self.topo_json_cache = json;
                    self.dirty = true;
                }
            }
        }

        let sel = DisksDoc::selected(state).min(self.topo.drives.len().saturating_sub(1));
        if sel != self.selected {
            self.selected = sel;
            self.dirty = true;
        }
        let tab = Tab::from_key(&DisksDoc::tab(state));
        if tab != self.tab {
            self.tab = tab;
            self.dirty = true;
        }
        let pending = DisksDoc::pending(state);
        let parsed = parse_pending(&pending);
        if parsed.is_some() != self.pending.is_some() {
            self.pending = parsed;
            self.dirty = true;
        }
        let status = DisksDoc::status(state);
        if status != self.status {
            self.status = status;
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: percent(1.0),
                    height: percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        // Now that the width is known, reconcile the flap so a window resize
        // across the breakpoint drops a stale toggle and returns to the new
        // layout's default (wide ⇒ list docked-open, narrow ⇒ overlay-closed).
        let split = self.split_layout();
        self.flap.reconcile(split);
        // Settle the very first frame to its resolved default so a cold start
        // at any width paints a static layout (no list sliding in/out).
        if !self.laid_out {
            self.flap.snap(self.flap.is_open(split));
            self.laid_out = true;
        }
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Advance the eased slide every frame so the flap opens/collapses
        // smoothly. Mirrors the shared `NavSplit`'s fixed-step tick.
        let animating = self.flap.tick(self.split_layout(), 1.0 / 60.0);
        if animating {
            self.dirty = true;
        }
        animating
    }

    fn needs_animation_frame(&self) -> bool {
        self.flap.is_animating(self.split_layout())
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        let lay = self.compute_layout();
        let tr = text_renderer;

        // Detail pane (drawn first; in overlay mode the flap sits on top).
        if let Some(drive) = self.drive() {
            self.draw_header(scene, tr, theme, transform, &lay, drive);
            self.draw_tabs(scene, tr, theme, transform, &lay);
            match self.tab {
                Tab::Overview => self.draw_overview(scene, tr, theme, transform, lay.content, drive),
                Tab::Smart => self.draw_smart_tab(scene, tr, theme, transform, lay.content, drive),
                Tab::Partitions => self.draw_partitions_tab(scene, tr, theme, transform, lay.content, drive),
                Tab::Tools => self.draw_tools_tab(scene, tr, theme, transform, lay.content, &lay),
            }
        } else {
            self.txt(scene, tr, "No drives detected.", 16.0, c.text_dim, lay.content.x0, lay.content.y0, None, TextWeight::Regular);
        }

        self.draw_statusline(scene, tr, theme, transform);

        // Dimming scrim behind the open overlay flap (eased with the reveal).
        if lay.scrim.width() > 0.0 {
            let alpha = (self.flap.reveal() * 0.5 * 255.0) as u8;
            scene.fill(Fill::NonZero, transform, with_alpha(Color::BLACK, alpha), None, &lay.scrim);
        }

        // Drive-list flap. In overlay mode it floats above the content with an
        // opaque backdrop so the list reads cleanly; in docked mode it is the
        // permanent left column and is only hidden mid-collapse.
        if self.flap.reveal() > 0.001 {
            if lay.split == SplitLayout::Overlay {
                let panel = lay.sidebar.intersect(self.rect);
                scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &panel);
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    c.border_standard,
                    None,
                    &Line::new(Point::new(panel.x1, panel.y0), Point::new(panel.x1, panel.y1)),
                );
            }
            self.draw_drive_list(scene, tr, theme, transform, &lay);
        }

        // Top bar last so the hamburger + title stay above the flap panel.
        self.draw_topbar(scene, tr, theme, transform, &lay);

        if let (Some(ml), Some(p)) = (lay.modal, self.pending) {
            self.draw_modal(scene, tr, theme, transform, &ml, p);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::{Key, NamedKey};

        let lay = self.compute_layout();

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                let prev = (self.hover_card, self.hover_action, self.hover_tab, self.hover_menu);
                // The drive list only receives hover when it's actually shown.
                let list_shown = self.list_open();
                self.hover_card = if list_shown {
                    lay.cards.iter().position(|r| r.contains(p))
                } else {
                    None
                };
                self.hover_action = lay
                    .actions
                    .iter()
                    .chain(lay.tool_buttons.iter())
                    .find(|(r, _)| r.contains(p))
                    .map(|(_, a)| *a);
                self.hover_tab = lay.tabs.iter().find(|(r, _)| r.contains(p)).map(|(_, t)| *t);
                self.hover_menu = lay.hamburger.is_some_and(|hb| hb.contains(p));
                if prev != (self.hover_card, self.hover_action, self.hover_tab, self.hover_menu) {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                // Modal first — it is fully blocking.
                if let Some(ml) = lay.modal {
                    if ml.confirm.contains(p) {
                        self.confirm_pending(state);
                    } else if ml.cancel.contains(p) {
                        self.cancel_pending(state);
                    }
                    // Clicks anywhere else while modal is open are swallowed.
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // Hamburger toggles the overlay flap (narrow window only).
                if lay.hamburger.is_some_and(|hb| hb.contains(p)) {
                    self.flap.toggle(self.split_layout());
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // While the overlay flap is open, a click on the scrim (i.e.
                // outside the sliding list panel) dismisses it.
                if lay.split == SplitLayout::Overlay
                    && self.list_open()
                    && lay.scrim.width() > 0.0
                    && !lay.sidebar.contains(p)
                {
                    self.flap.close();
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                if lay.refresh.contains(p) {
                    self.do_refresh(state);
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // Drive cards are only clickable when the list is shown.
                if self.list_open() {
                    if let Some(i) = lay.cards.iter().position(|r| r.contains(p)) {
                        self.select(state, i);
                        // Selecting from the overlay flap dismisses it so the
                        // detail is immediately visible (single-pane feel).
                        if lay.split == SplitLayout::Overlay {
                            self.flap.close();
                        }
                        *handled = true;
                        self.dirty = true;
                        return;
                    }
                }
                if let Some((_, tab)) = lay.tabs.iter().find(|(r, _)| r.contains(p)) {
                    self.set_tab(state, *tab);
                    *handled = true;
                    return;
                }
                if let Some((_, a)) = lay
                    .actions
                    .iter()
                    .chain(lay.tool_buttons.iter())
                    .find(|(r, _)| r.contains(p))
                {
                    self.trigger_action(state, *a);
                    *handled = true;
                    self.dirty = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                // Modal key handling.
                if self.pending.is_some() {
                    match logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.cancel_pending(state);
                            *handled = true;
                            self.dirty = true;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.confirm_pending(state);
                            *handled = true;
                            self.dirty = true;
                        }
                        _ => {}
                    }
                    return;
                }
                // Esc dismisses an open overlay flap before any other binding.
                if matches!(logical_key, Key::Named(NamedKey::Escape))
                    && lay.split == SplitLayout::Overlay
                    && self.list_open()
                {
                    self.flap.close();
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        let n = self.topo.drives.len();
                        if n > 0 {
                            self.select(state, (self.selected + 1) % n);
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let n = self.topo.drives.len();
                        if n > 0 {
                            self.select(state, (self.selected + n - 1) % n);
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let tabs = Tab::all();
                        let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
                        self.set_tab(state, tabs[(i + 1) % tabs.len()]);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        let tabs = Tab::all();
                        let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
                        self.set_tab(state, tabs[(i + tabs.len() - 1) % tabs.len()]);
                        *handled = true;
                    }
                    Key::Character(s) => {
                        match s.as_str() {
                            "1" => { self.set_tab(state, Tab::Overview); *handled = true; }
                            "2" => { self.set_tab(state, Tab::Smart); *handled = true; }
                            "3" => { self.set_tab(state, Tab::Partitions); *handled = true; }
                            "4" => { self.set_tab(state, Tab::Tools); *handled = true; }
                            "r" | "R" => { self.do_refresh(state); *handled = true; }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "DisksApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl DisksApp {
    /// Re-probe the live system (no-op fallback to sample). Used by the
    /// Rescan button + `r` key.
    fn do_refresh(&mut self, state: &mut AutoCommit) {
        let topo = if std::env::var("DISKS_LIVE").is_ok() {
            crate::probe::probe().unwrap_or_else(|| self.topo.clone())
        } else {
            // In demo/offscreen mode, just confirm on the status line.
            self.status = "Rescanned storage devices".to_string();
            DisksDoc::set_status(state, &self.status);
            return;
        };
        DisksDoc::write_topology(state, &topo);
        self.status = format!("Rescanned — {} devices", topo.drives.len());
        DisksDoc::set_status(state, &self.status);
        self.dirty = true;
    }
}

fn parse_pending(s: &str) -> Option<Pending> {
    let (a, idx) = s.split_once(':')?;
    let action = ActionKind::from_key(a)?;
    let drive = idx.parse::<usize>().ok()?;
    Some(Pending { action, drive })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_app() -> (DisksApp, AutoCommit) {
        let mut app = DisksApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    #[test]
    fn sync_seeds_sample_topology() {
        let (app, _doc) = seeded_app();
        assert_eq!(app.topo.drives.len(), 4);
        assert_eq!(app.selected, 0);
        assert_eq!(app.tab, Tab::Overview);
    }

    #[test]
    fn select_persists_to_doc_and_back() {
        let (mut app, mut doc) = seeded_app();
        app.select(&mut doc, 2);
        assert_eq!(DisksDoc::selected(&doc), 2);
        // A fresh app syncing the same doc adopts the selection.
        let mut app2 = DisksApp::new();
        let root = automerge::ROOT;
        app2.sync(&mut doc, &root);
        assert_eq!(app2.selected, 2);
    }

    #[test]
    fn destructive_action_opens_modal() {
        let (mut app, mut doc) = seeded_app();
        app.trigger_action(&mut doc, ActionKind::Format);
        assert!(app.pending.is_some());
        assert!(DisksDoc::pending(&doc).starts_with("format:"));
    }

    #[test]
    fn non_destructive_action_sets_status_no_modal() {
        let (mut app, mut doc) = seeded_app();
        app.trigger_action(&mut doc, ActionKind::Mount);
        assert!(app.pending.is_none());
        assert!(!DisksDoc::status(&doc).is_empty());
    }

    #[test]
    fn confirm_clears_pending() {
        let (mut app, mut doc) = seeded_app();
        app.trigger_action(&mut doc, ActionKind::Format);
        app.confirm_pending(&mut doc);
        assert!(app.pending.is_none());
        assert_eq!(DisksDoc::pending(&doc), "");
    }

    #[test]
    fn layout_has_one_card_per_drive() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let lay = app.compute_layout();
        assert_eq!(lay.cards.len(), 4);
        assert_eq!(lay.tabs.len(), 4);
        assert_eq!(lay.actions.len(), 4);
    }

    /// Lay the app out at a given width through the real `build_layout` +
    /// `apply_layout` passes, so these tests exercise the production
    /// responsiveness path (reconcile + first-frame settle) rather than a
    /// synthetic shortcut.
    fn at_width(width: f64) -> DisksApp {
        let (mut app, _doc) = seeded_app();
        layout_at(&mut app, width);
        app
    }

    fn layout_at(app: &mut DisksApp, width: f64) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width as f32),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn wide_window_docks_the_list_beside_the_detail() {
        let app = at_width(1280.0);
        assert_eq!(app.split_layout(), SplitLayout::Docked);
        assert!(app.list_open(), "docked list is open by default");
        let lay = app.compute_layout();
        assert!(lay.hamburger.is_none(), "no hamburger when docked");
        assert_eq!(lay.scrim.width(), 0.0, "no scrim when docked");
        // The detail header begins to the right of the docked list column.
        assert!(
            lay.header.x0 >= PAD + LIST_W,
            "detail is inset past the docked list, got x0={}",
            lay.header.x0
        );
    }

    #[test]
    fn narrow_window_collapses_list_to_a_closed_overlay() {
        let app = at_width(640.0);
        assert_eq!(app.split_layout(), SplitLayout::Overlay);
        assert!(!app.list_open(), "overlay list is closed by default");
        let lay = app.compute_layout();
        assert!(lay.hamburger.is_some(), "narrow window shows a hamburger");
        assert_eq!(lay.scrim.width(), 0.0, "closed flap paints no scrim");
        // Detail uses the full width (minus the usual padding) when collapsed.
        assert!(
            (lay.content.width() - (app.rect.width() - 2.0 * PAD)).abs() < 1.0,
            "collapsed detail spans the full width"
        );
        // The list panel is slid fully off the left edge while closed.
        assert!(lay.sidebar.x1 <= app.rect.x0 + 0.5, "closed flap is off-screen left");
    }

    #[test]
    fn hamburger_summons_overlay_flap_with_scrim() {
        let mut app = at_width(640.0);
        app.flap.snap(true); // settle open deterministically
        assert!(app.list_open());
        let lay = app.compute_layout();
        assert!(lay.scrim.width() > 0.0, "open overlay flap dims the content behind a scrim");
        // Fully revealed, the list panel sits flush at the left edge.
        assert!(
            (lay.sidebar.x0 - app.rect.x0).abs() < 1.0,
            "revealed overlay list is flush left, got x0={}",
            lay.sidebar.x0
        );
        // Cards ride along with the panel.
        assert_eq!(lay.cards.len(), 4);
        assert!((lay.cards[0].x0 - (app.rect.x0 + PAD)).abs() < 1.0);
    }

    #[test]
    fn crossing_breakpoint_restores_the_layout_default() {
        // User hides the docked list, then the window narrows: the stale
        // preference is dropped and the overlay default (closed) applies;
        // widening again returns to the docked default (open).
        let mut app = at_width(1280.0);
        app.flap.close();
        assert!(!app.list_open());
        layout_at(&mut app, 640.0);
        assert!(!app.list_open(), "narrow default is closed");
        layout_at(&mut app, 1280.0);
        assert!(app.list_open(), "widening restores the docked-open default");
    }
}
