//! The root `SoftwareStore` widget.
//!
//! This is a single self-laying-out, self-drawing `Widget`: rather than
//! composing dozens of child widgets it computes its own layout maths in
//! `draw`, paints rich cards / detail pages / lists directly into the
//! Vello scene, and records a flat list of interactive *hit targets* that
//! `handle_event` tests pointer clicks against. The mutable state
//! (installed set, in-flight installs) lives in the Automerge document;
//! the catalog and ephemeral navigation state live in widget fields.
//!
//! Frame contract (the five passes):
//!   * `sync`         — read the install snapshot out of the CRDT, seed
//!                      defaults on first run, advance / finalise installs.
//!   * `build_layout` — one full-bleed leaf node (we do our own layout).
//!   * `apply_layout` — capture the viewport rect.
//!   * `draw`         — paint the active screen; rebuild `hit_targets`.
//!   * `handle_event` — pointer + keyboard, mutating the CRDT directly.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::catalog::sample_catalog;
use crate::doc::StoreDoc;
use crate::draw::{self, alpha, mix};
use crate::model::{
    AppEntry, Category, Catalog, InstallSnapshot, InstallState, Permission, Source,
};
use crate::probe::ProbeResult;

/// How long a simulated install / update takes to fill its progress bar.
const INSTALL_DURATION: Duration = Duration::from_millis(2600);
/// How long an "Opening …" toast lingers.
const TOAST_DURATION: Duration = Duration::from_millis(2200);

const HEADER_H: f64 = 64.0;
const BODY_PAD_X: f64 = 28.0;
const BODY_PAD_TOP: f64 = 20.0;
const GAP: f64 = 16.0;

/// Which top-level screen the body shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Browse,
    Installed,
    Updates,
}

/// One interactive region, rebuilt every `draw` and tested in
/// `handle_event`.
#[derive(Clone, Debug)]
struct HitTarget {
    rect: Rect,
    action: Action,
}

#[derive(Clone, Debug, PartialEq)]
enum Action {
    OpenDetail(String),
    Back,
    SetTab(Tab),
    SetCategory(Option<Category>),
    FocusSearch,
    ClearSearch,
    Install(String),
    Update(String),
    Cancel(String),
    Remove(String),
    Open(String),
    UpdateAll,
}

/// Visual style for a pill button.
#[derive(Clone, Copy, PartialEq)]
enum BtnStyle {
    Primary,
    Neutral,
    Ghost,
    Danger,
}

pub struct SoftwareStore {
    catalog: Catalog,
    /// Cached install state read out of the CRDT each `sync`.
    snapshot: InstallSnapshot,
    tab: Tab,
    /// `Some(app_id)` when the detail screen is open over the active tab.
    detail: Option<String>,
    /// Active category filter on the Browse screen (None = all).
    active_category: Option<Category>,
    search_query: String,
    search_focused: bool,
    /// Vertical scroll offset of the body, in logical px.
    scroll: f64,
    /// Total content height computed during the last `draw`, used to
    /// clamp scrolling.
    content_height: f64,
    /// Local wall-clock start time per in-flight install, used to animate
    /// the progress bar smoothly between syncs.
    install_start: HashMap<String, Instant>,
    /// Set true by `draw` when an install's wall-clock progress reaches
    /// 1.0, so `needs_commit` can nudge one finalising `sync`.
    finalize_pending: bool,
    /// Transient bottom-of-screen toast (text, shown-at).
    toast: Option<(String, Instant)>,
    /// Best-effort live system probe, merged once in `sync`. `None` in
    /// the `--shots` path so the screenshot stays deterministic.
    probe: Option<ProbeResult>,
    probe_merged: bool,

    rect: Rect,
    node: Option<NodeId>,
    mouse: Point,
    hit_targets: Vec<HitTarget>,
    dirty: bool,
}

impl SoftwareStore {
    /// Construct the store with the bundled sample catalog. `probe`
    /// carries optional live system data (live app only).
    pub fn new(probe: Option<ProbeResult>) -> Self {
        Self {
            catalog: Catalog::new(sample_catalog()),
            snapshot: InstallSnapshot::default(),
            tab: Tab::Browse,
            detail: None,
            active_category: None,
            search_query: String::new(),
            search_focused: false,
            scroll: 0.0,
            content_height: 0.0,
            install_start: HashMap::new(),
            finalize_pending: false,
            toast: None,
            probe,
            probe_merged: false,
            rect: Rect::ZERO,
            node: None,
            mouse: Point::ZERO,
            hit_targets: Vec::new(),
            dirty: true,
        }
    }

    /// Apply an initial-view override from the `SOFTWARE_STORE_SHOT_VIEW`
    /// environment variable. Used only by the `--shots` path to capture
    /// screens other than Browse for visual review. Accepted forms:
    /// `installed`, `updates`, `search:<query>`, `detail:<app-id>`.
    pub fn apply_shot_view_env(&mut self) {
        let Ok(spec) = std::env::var("SOFTWARE_STORE_SHOT_VIEW") else { return };
        let spec = spec.trim();
        if spec == "installed" {
            self.tab = Tab::Installed;
        } else if spec == "updates" {
            self.tab = Tab::Updates;
        } else if let Some(q) = spec.strip_prefix("search:") {
            self.search_query = q.to_string();
            self.tab = Tab::Browse;
        } else if let Some(id) = spec.strip_prefix("detail:") {
            self.detail = Some(id.to_string());
        }
    }

    fn body_rect(&self) -> Rect {
        Rect::new(self.rect.x0, self.rect.y0 + HEADER_H, self.rect.x1, self.rect.y1)
    }

    fn max_scroll(&self) -> f64 {
        let viewport = self.body_rect().height();
        (self.content_height - viewport).max(0.0)
    }

    fn reset_scroll(&mut self) {
        self.scroll = 0.0;
    }

    /// The set of updatable apps, used by the Updates badge + Update All.
    fn updatable_ids(&self) -> Vec<String> {
        self.catalog
            .updatable(&self.snapshot)
            .iter()
            .map(|a| a.id.clone())
            .collect()
    }

    /// Begin a (simulated) install / update of `id`.
    fn start_install(&mut self, state: &mut AutoCommit, id: &str) {
        StoreDoc::set_installing(state, id, 0.0);
        self.install_start.insert(id.to_string(), Instant::now());
        self.snapshot = StoreDoc::read_snapshot(state);
        // Optional real install action, gated behind an env opt-in so the
        // default UX is the safe simulated progress.
        if std::env::var("SOFTWARE_STORE_REAL_INSTALL").is_ok() {
            self.spawn_real_install(id);
        }
        self.dirty = true;
    }

    /// Cancel an in-flight install.
    fn cancel_install(&mut self, state: &mut AutoCommit, id: &str) {
        StoreDoc::clear_installing(state, id);
        self.install_start.remove(id);
        self.snapshot = StoreDoc::read_snapshot(state);
        self.dirty = true;
    }

    /// Gated, best-effort real install via flatpak/podman in a detached
    /// thread. Never blocks the UI; failures are silent (the simulated
    /// progress is the source of truth for the bar).
    fn spawn_real_install(&self, id: &str) {
        let Some(app) = self.catalog.get(id) else { return };
        let id = id.to_string();
        let source = app.source;
        std::thread::spawn(move || {
            let _ = match source {
                Source::Flatpak => std::process::Command::new("flatpak")
                    .args(["install", "-y", "--noninteractive", "flathub", &id])
                    .status(),
                Source::Oci => std::process::Command::new("podman")
                    .args(["pull", &id])
                    .status(),
            };
        });
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    /// Apply a hit-target action. Mutates the CRDT directly for install
    /// state; everything else is local navigation.
    fn apply(&mut self, action: Action, state: &mut AutoCommit) {
        match action {
            Action::OpenDetail(id) => {
                self.detail = Some(id);
                self.reset_scroll();
            }
            Action::Back => {
                self.detail = None;
                self.reset_scroll();
            }
            Action::SetTab(t) => {
                if self.tab != t || self.detail.is_some() {
                    self.tab = t;
                    self.detail = None;
                    self.reset_scroll();
                }
            }
            Action::SetCategory(c) => {
                self.active_category = c;
                self.reset_scroll();
            }
            Action::FocusSearch => self.search_focused = true,
            Action::ClearSearch => {
                self.search_query.clear();
                self.search_focused = false;
            }
            Action::Install(id) | Action::Update(id) => self.start_install(state, &id),
            Action::Cancel(id) => self.cancel_install(state, &id),
            Action::Remove(id) => {
                StoreDoc::remove_installed(state, &id);
                self.install_start.remove(&id);
                self.snapshot = StoreDoc::read_snapshot(state);
            }
            Action::Open(id) => {
                let name = self
                    .catalog
                    .get(&id)
                    .map(|a| a.name.clone())
                    .unwrap_or(id.clone());
                self.show_toast(format!("Opening {name}…"));
                if std::env::var("SOFTWARE_STORE_REAL_INSTALL").is_ok() {
                    let _ = std::process::Command::new("flatpak").args(["run", &id]).spawn();
                }
            }
            Action::UpdateAll => {
                for id in self.updatable_ids() {
                    self.start_install(state, &id);
                }
            }
        }
        self.dirty = true;
    }

    /// Derive the install state for a catalog app, preferring the smooth
    /// local wall-clock progress over the CRDT-stored value while an
    /// install is in flight locally.
    fn live_state(&self, app: &AppEntry) -> InstallState {
        if let Some(start) = self.install_start.get(&app.id) {
            let p = (start.elapsed().as_secs_f64() / INSTALL_DURATION.as_secs_f64()).min(1.0);
            return InstallState::Installing(p);
        }
        self.catalog.state_of(app, &self.snapshot)
    }
}

// ===================================================================
// Drawing
// ===================================================================

impl SoftwareStore {
    fn draw_header(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) {
        let c = &theme.tokens.colors;
        let header = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + HEADER_H);
        // Opaque header background (paints over any scrolled body).
        draw::fill_rect(scene, t, header, c.surfaces.base);
        // Bottom hairline.
        draw::fill_rect(
            scene,
            t,
            Rect::new(header.x0, header.y1 - 1.0, header.x1, header.y1),
            c.border_standard,
        );

        let cy = header.center().y;
        let mut x = header.x0 + BODY_PAD_X;

        // Back chevron when in detail view.
        if self.detail.is_some() {
            let back_rect = Rect::new(x, cy - 16.0, x + 64.0, cy + 16.0);
            let hov = back_rect.contains(self.mouse);
            draw::fill_round(scene, t, back_rect, 8.0, if hov { c.surface_white_10 } else { c.surfaces.raised });
            draw::text_w(scene, tr, t, "‹  Back", theme.tokens.typography.scale.sm, TextWeight::Medium, c.text_primary, x + 12.0, cy - 8.0, None);
            targets.push(HitTarget { rect: back_rect, action: Action::Back });
            x = back_rect.x1 + 14.0;
        }

        // Wordmark: an accent app-tile + "Software".
        let tile = Rect::new(x, cy - 14.0, x + 28.0, cy + 14.0);
        draw::fill_round(scene, t, tile, 8.0, c.accent[5]);
        draw::text_w(scene, tr, t, "◍", 16.0, TextWeight::Bold, Color::WHITE, tile.center().x - 6.0, tile.center().y - 9.0, None);
        let name_x = tile.x1 + 10.0;
        draw::text_w(scene, tr, t, "Software", theme.tokens.typography.scale.lg, TextWeight::Bold, c.text_primary, name_x, cy - 11.0, None);

        // Right cluster: nav tabs, then search pill.
        let pad_r = header.x1 - BODY_PAD_X;

        // Tabs anchored at the right edge: measure widths, then lay out
        // left-to-right from a computed origin.
        let up_count = self.catalog.updatable(&self.snapshot).len();
        let inst_count = self.catalog.installed(&self.snapshot).len();
        let tabs: [(Tab, String); 3] = [
            (Tab::Browse, "Browse".into()),
            (Tab::Installed, format!("Installed · {inst_count}")),
            (Tab::Updates, "Updates".into()),
        ];
        let tsize = theme.tokens.typography.scale.sm;
        let mut widths = Vec::new();
        for (tab, label) in &tabs {
            let mut w = draw::measure(tr, label, tsize).0 + 28.0;
            if *tab == Tab::Updates && up_count > 0 {
                w += 22.0;
            }
            widths.push(w);
        }
        let total_tabs: f64 = widths.iter().sum();
        let tabs_origin = pad_r - total_tabs;
        let mut tab_x = tabs_origin;
        for (i, (tab, label)) in tabs.iter().enumerate() {
            let w = widths[i];
            let r = Rect::new(tab_x, cy - 16.0, tab_x + w, cy + 16.0);
            let active = self.detail.is_none() && self.tab == *tab && self.search_query.is_empty();
            let hov = r.contains(self.mouse);
            let bg = if active {
                c.accent[5]
            } else if hov {
                c.surface_white_10
            } else {
                Color::TRANSPARENT
            };
            if bg != Color::TRANSPARENT {
                draw::fill_round(scene, t, r, theme.tokens.radii.full, bg);
            }
            let fg = if active { Color::WHITE } else { c.text_dim };
            draw::text_w(scene, tr, t, label, tsize, TextWeight::Medium, fg, r.x0 + 14.0, cy - 8.0, None);
            // Updates badge.
            if *tab == Tab::Updates && up_count > 0 {
                let bw = draw::measure(tr, label, tsize).0;
                let badge_c = Point::new(r.x0 + 14.0 + bw + 12.0, cy);
                let bg_badge = if active { Color::WHITE } else { c.semantic.warning };
                let badge = Circle::new(badge_c, 9.0);
                scene.fill(vello::peniko::Fill::NonZero, t, bg_badge, None, &badge);
                let txt = up_count.to_string();
                let (nw, nh) = draw::measure(tr, &txt, theme.tokens.typography.scale.xs);
                let nfg = if active { c.accent[5] } else { Color::WHITE };
                draw::text_w(scene, tr, t, &txt, theme.tokens.typography.scale.xs, TextWeight::Bold, nfg, badge_c.x - nw / 2.0, badge_c.y - nh / 2.0, None);
            }
            targets.push(HitTarget { rect: r, action: Action::SetTab(*tab) });
            tab_x += w;
        }

        // Search pill, to the left of the tabs.
        let search_w = 280.0_f64;
        let search_r = Rect::new(tabs_origin - GAP - search_w, cy - 16.0, tabs_origin - GAP, cy + 16.0);
        if search_r.x0 > name_x + 120.0 {
            let focused = self.search_focused;
            let border = if focused { c.border_focus } else { c.border_standard };
            draw::fill_round(scene, t, search_r, theme.tokens.radii.full, c.surfaces.raised);
            draw::stroke_round(scene, t, search_r, theme.tokens.radii.full, border, if focused { 2.0 } else { 1.0 });
            // Magnifier glyph.
            draw::text(scene, tr, t, "⌕", 16.0, c.text_dim, search_r.x0 + 12.0, cy - 9.0, None);
            let tx = search_r.x0 + 32.0;
            if self.search_query.is_empty() && !focused {
                draw::text(scene, tr, t, "Search apps…", tsize, c.text_dim, tx, cy - 8.0, None);
            } else {
                draw::text(scene, tr, t, &self.search_query, tsize, c.text_primary, tx, cy - 8.0, None);
                if focused {
                    let cw = draw::measure(tr, &self.search_query, tsize).0;
                    let caret = Rect::new(tx + cw + 1.0, cy - 9.0, tx + cw + 2.5, cy + 9.0);
                    draw::fill_rect(scene, t, caret, c.accent[5]);
                }
                // Clear (×) affordance.
                if !self.search_query.is_empty() {
                    let clr = Rect::new(search_r.x1 - 26.0, cy - 11.0, search_r.x1 - 6.0, cy + 11.0);
                    let hov = clr.contains(self.mouse);
                    draw::text(scene, tr, t, "✕", 13.0, if hov { c.text_primary } else { c.text_dim }, clr.center().x - 5.0, cy - 7.0, None);
                    targets.push(HitTarget { rect: clr, action: Action::ClearSearch });
                }
            }
            targets.push(HitTarget { rect: search_r, action: Action::FocusSearch });
        }
    }

    /// Draw an icon tile for `app` at `rect`, theme-derived colour.
    fn icon(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, rect: Rect, app: &AppEntry) {
        let base = theme.tokens.colors.accent[app.icon_tone.min(10)];
        let sheen = mix(base, Color::WHITE, 0.5);
        draw::icon_tile(scene, tr, t, rect, base, sheen, &app.monogram, Color::WHITE);
    }

    /// A pill button; records a hit target. `hovered` is precomputed by
    /// the caller (it owns `self.mouse`).
    #[allow(clippy::too_many_arguments)]
    fn pill_button(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        label: &str,
        style: BtnStyle,
        hovered: bool,
        action: Action,
    ) {
        let c = &theme.tokens.colors;
        let (bg, fg, border) = match style {
            BtnStyle::Primary => (
                if hovered { c.accent[4] } else { c.accent[5] },
                Color::WHITE,
                None,
            ),
            BtnStyle::Neutral => (
                if hovered { c.surface_white_20 } else { c.surface_white_10 },
                c.text_primary,
                Some(c.border_standard),
            ),
            BtnStyle::Ghost => (
                if hovered { c.surface_white_10 } else { Color::TRANSPARENT },
                c.text_dim,
                Some(c.border_standard),
            ),
            BtnStyle::Danger => (
                if hovered { alpha(c.semantic.danger, 0.22) } else { Color::TRANSPARENT },
                c.semantic.danger,
                Some(alpha(c.semantic.danger, 0.5)),
            ),
        };
        let radius = theme.tokens.radii.full;
        if bg != Color::TRANSPARENT {
            draw::fill_round(scene, t, rect, radius, bg);
        }
        if let Some(b) = border {
            draw::stroke_round(scene, t, rect, radius, b, 1.0);
        }
        let size = theme.tokens.typography.scale.sm;
        let (lw, lh) = draw::measure(tr, label, size);
        draw::text_w(
            scene,
            tr,
            t,
            label,
            size,
            TextWeight::Semibold,
            fg,
            rect.center().x - lw / 2.0,
            rect.center().y - lh / 2.0,
            None,
        );
        targets.push(HitTarget { rect, action });
    }

    /// Render the install/open/update action control for `app` at `rect`.
    /// Either a button or — when installing — a progress bar.
    fn action_control(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
        allow_remove: bool,
    ) {
        let c = &theme.tokens.colors;
        match self.live_state(app) {
            InstallState::Available => {
                Self::pill_button(scene, tr, t, theme, targets, rect, "Install", BtnStyle::Primary, rect.contains(self.mouse), Action::Install(app.id.clone()));
            }
            InstallState::Installing(p) => {
                // Track.
                let radius = theme.tokens.radii.full;
                draw::fill_round(scene, t, rect, radius, c.surface_white_10);
                // Fill.
                let fill_w = (rect.width() * p).max(rect.height());
                let fill = Rect::new(rect.x0, rect.y0, rect.x0 + fill_w, rect.y1);
                draw::fill_round(scene, t, fill, radius, c.accent[5]);
                let label = format!("Installing… {:.0}%", p * 100.0);
                let size = theme.tokens.typography.scale.xs;
                let (lw, lh) = draw::measure(tr, &label, size);
                draw::text_w(scene, tr, t, &label, size, TextWeight::Semibold, Color::WHITE, rect.center().x - lw / 2.0, rect.center().y - lh / 2.0, None);
                // Cancel target spans the whole control.
                targets.push(HitTarget { rect, action: Action::Cancel(app.id.clone()) });
            }
            InstallState::Installed => {
                if allow_remove {
                    let half = rect.width() / 2.0 - 4.0;
                    let open_r = Rect::new(rect.x0, rect.y0, rect.x0 + half, rect.y1);
                    let rem_r = Rect::new(rect.x1 - half, rect.y0, rect.x1, rect.y1);
                    Self::pill_button(scene, tr, t, theme, targets, open_r, "Open", BtnStyle::Neutral, open_r.contains(self.mouse), Action::Open(app.id.clone()));
                    Self::pill_button(scene, tr, t, theme, targets, rem_r, "Remove", BtnStyle::Danger, rem_r.contains(self.mouse), Action::Remove(app.id.clone()));
                } else {
                    Self::pill_button(scene, tr, t, theme, targets, rect, "Open", BtnStyle::Neutral, rect.contains(self.mouse), Action::Open(app.id.clone()));
                }
            }
            InstallState::UpdateAvailable(_) => {
                if allow_remove {
                    let half = rect.width() / 2.0 - 4.0;
                    let up_r = Rect::new(rect.x0, rect.y0, rect.x0 + half, rect.y1);
                    let open_r = Rect::new(rect.x1 - half, rect.y0, rect.x1, rect.y1);
                    Self::pill_button(scene, tr, t, theme, targets, up_r, "Update", BtnStyle::Primary, up_r.contains(self.mouse), Action::Update(app.id.clone()));
                    Self::pill_button(scene, tr, t, theme, targets, open_r, "Open", BtnStyle::Neutral, open_r.contains(self.mouse), Action::Open(app.id.clone()));
                } else {
                    Self::pill_button(scene, tr, t, theme, targets, rect, "Update", BtnStyle::Primary, rect.contains(self.mouse), Action::Update(app.id.clone()));
                }
            }
        }
    }

    /// A standard browse/search grid card.
    fn draw_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
    ) {
        let c = &theme.tokens.colors;
        let hov = rect.contains(self.mouse);
        draw::fill_round(scene, t, rect, theme.tokens.radii.lg, c.surfaces.raised);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.lg, if hov { c.border_focus } else { c.border_standard }, 1.0);

        let pad = 14.0;
        let icon_sz = 46.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x0 + pad + icon_sz, rect.y0 + pad + icon_sz);
        self.icon(scene, tr, t, theme, icon_r, app);

        let text_x = icon_r.x1 + 12.0;
        let text_w = rect.x1 - pad - text_x;
        draw::text_w(scene, tr, t, &app.name, theme.tokens.typography.scale.base, TextWeight::Bold, c.text_primary, text_x, rect.y0 + pad, None);
        let dev = draw::ellipsize(tr, &app.developer, theme.tokens.typography.scale.xs, text_w);
        draw::text(scene, tr, t, &dev, theme.tokens.typography.scale.xs, c.text_dim, text_x, rect.y0 + pad + 20.0, None);

        // Summary (wrapped, 2 lines max region).
        let sum_y = icon_r.y1 + 12.0;
        let summary = draw::ellipsize(tr, &app.summary, theme.tokens.typography.scale.sm, text_w + icon_sz + 12.0);
        draw::text(scene, tr, t, &summary, theme.tokens.typography.scale.sm, c.text_dim, rect.x0 + pad, sum_y, Some((rect.width() - pad * 2.0) as f32));

        // Bottom row: rating + action button.
        let row_y = rect.y1 - pad - 26.0;
        draw::rating_stars(scene, t, rect.x0 + pad, row_y + 9.0, app.rating, 6.0, 2.0, c.semantic.warning, c.surface_white_20);
        draw::text(scene, tr, t, &app.rating_label(), theme.tokens.typography.scale.xs, c.text_dim, rect.x0 + pad + 70.0, row_y + 3.0, None);

        let btn_w = 96.0_f64;
        let btn = Rect::new(rect.x1 - pad - btn_w, row_y, rect.x1 - pad, row_y + 26.0);
        self.action_control(scene, tr, t, theme, targets, btn, app, false);

        // Whole-card click → detail (registered first so buttons, pushed
        // later, win the reverse hit-test).
        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    /// A large featured/hero card.
    fn draw_hero(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
    ) {
        let c = &theme.tokens.colors;
        let accent = c.accent[app.icon_tone.min(10)];
        let hov = rect.contains(self.mouse);
        // Banner: deep accent base with a lighter diagonal-ish sheen band.
        draw::fill_round(scene, t, rect, theme.tokens.radii.xl, mix(accent, c.surfaces.base, 0.35));
        let band = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + rect.height() * 0.5);
        let band_round = vello::kurbo::RoundedRect::new(
            band.x0,
            band.y0,
            band.x1,
            band.y1,
            vello::kurbo::RoundedRectRadii::new(theme.tokens.radii.xl as f64, theme.tokens.radii.xl as f64, 0.0, 0.0),
        );
        scene.fill(vello::peniko::Fill::NonZero, t, alpha(mix(accent, Color::WHITE, 0.25), 0.35), None, &band_round);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.xl, if hov { alpha(Color::WHITE, 0.5) } else { c.border_white_20 }, 1.0);

        let pad = 18.0;
        // "Editor's Choice" eyebrow.
        draw::text_w(scene, tr, t, "FEATURED", theme.tokens.typography.scale.xs, TextWeight::Bold, alpha(Color::WHITE, 0.8), rect.x0 + pad, rect.y0 + pad, None);

        let icon_sz = 64.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad + 22.0, rect.x0 + pad + icon_sz, rect.y0 + pad + 22.0 + icon_sz);
        self.icon(scene, tr, t, theme, icon_r, app);

        let tx = icon_r.x1 + 14.0;
        draw::text_w(scene, tr, t, &app.name, theme.tokens.typography.scale.xl, TextWeight::Bold, Color::WHITE, tx, icon_r.y0, None);
        let dev = draw::ellipsize(tr, &app.developer, theme.tokens.typography.scale.sm, rect.x1 - pad - tx);
        draw::text(scene, tr, t, &dev, theme.tokens.typography.scale.sm, alpha(Color::WHITE, 0.8), tx, icon_r.y0 + 26.0, None);
        draw::rating_stars(scene, t, tx, icon_r.y0 + 52.0, app.rating, 6.0, 2.0, Color::WHITE, alpha(Color::WHITE, 0.3));

        // Summary across the bottom.
        let sum = draw::ellipsize(tr, &app.summary, theme.tokens.typography.scale.sm, rect.width() - pad * 2.0 - 110.0);
        draw::text(scene, tr, t, &sum, theme.tokens.typography.scale.sm, alpha(Color::WHITE, 0.92), rect.x0 + pad, rect.y1 - pad - 34.0, None);

        // Action button bottom-right.
        let btn_w = 110.0_f64;
        let btn = Rect::new(rect.x1 - pad - btn_w, rect.y1 - pad - 30.0, rect.x1 - pad, rect.y1 - pad);
        self.action_control(scene, tr, t, theme, targets, btn, app, false);

        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    /// Draw one category chip at `*x`, advancing `*x` past it.
    #[allow(clippy::too_many_arguments)]
    fn draw_chip(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        x: &mut f64,
        y: f64,
        label: &str,
        cat: Option<Category>,
    ) {
        let c = &theme.tokens.colors;
        let size = theme.tokens.typography.scale.sm;
        let w = draw::measure(tr, label, size).0 + 28.0;
        let r = Rect::new(*x, y, *x + w, y + 32.0);
        let active = self.active_category == cat;
        let hov = r.contains(self.mouse);
        let bg = if active { c.accent[5] } else if hov { c.surface_white_10 } else { c.surfaces.raised };
        draw::fill_round(scene, t, r, theme.tokens.radii.full, bg);
        if !active {
            draw::stroke_round(scene, t, r, theme.tokens.radii.full, c.border_standard, 1.0);
        }
        let fg = if active { Color::WHITE } else { c.text_dim };
        draw::text_w(scene, tr, t, label, size, TextWeight::Medium, fg, r.x0 + 14.0, r.center().y - size as f64 / 2.0 - 1.0, None);
        targets.push(HitTarget { rect: r, action: Action::SetCategory(cat) });
        *x += w + 10.0;
    }

    /// Category chip row. Returns the y after the row.
    fn draw_chips(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        x0: f64,
        y: f64,
    ) -> f64 {
        let mut x = x0;
        self.draw_chip(scene, tr, t, theme, targets, &mut x, y, "All", None);
        for cat in Category::ALL {
            self.draw_chip(scene, tr, t, theme, targets, &mut x, y, cat.label(), Some(cat));
        }
        y + 32.0
    }

    /// Section header text. Returns y after it.
    fn section(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, x: f64, y: f64, title: &str) -> f64 {
        draw::text_w(scene, tr, t, title, theme.tokens.typography.scale.lg, TextWeight::Bold, theme.tokens.colors.text_primary, x, y, None);
        y + 30.0
    }

    /// Draw a grid of cards. Returns the y after the grid.
    fn draw_grid(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        x0: f64,
        y0: f64,
        width: f64,
        apps: &[&AppEntry],
    ) -> f64 {
        if apps.is_empty() {
            draw::text(scene, tr, t, "No apps here yet.", theme.tokens.typography.scale.sm, theme.tokens.colors.text_dim, x0, y0, None);
            return y0 + 30.0;
        }
        let card_min = 280.0_f64;
        let cols = (((width + GAP) / (card_min + GAP)).floor() as usize).max(1);
        let card_w = (width - GAP * (cols as f64 - 1.0)) / cols as f64;
        let card_h = 132.0_f64;
        let mut y = y0;
        for (i, app) in apps.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx = x0 + col as f64 * (card_w + GAP);
            let cy = y0 + row as f64 * (card_h + GAP);
            let r = Rect::new(cx, cy, cx + card_w, cy + card_h);
            self.draw_card(scene, tr, t, theme, targets, r, app);
            y = cy + card_h;
        }
        y
    }

    /// The Browse screen body. Returns content height (absolute bottom y
    /// relative to body top before scroll).
    fn draw_browse(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        // Searching → results grid only.
        if !self.search_query.is_empty() {
            let results = self.catalog.search(&self.search_query, self.active_category);
            let header = format!("{} result{} for \u{201c}{}\u{201d}", results.len(), if results.len() == 1 { "" } else { "s" }, self.search_query);
            y = self.section(scene, tr, t, theme, x0, y, &header);
            y += 6.0;
            y = self.draw_chips(scene, tr, t, theme, targets, x0, y);
            y += 16.0;
            y = self.draw_grid(scene, tr, t, theme, targets, x0, y, width, &results);
            return y + 24.0 - (body.y0 - self.scroll);
        }

        // Category chips.
        y = self.draw_chips(scene, tr, t, theme, targets, x0, y);
        y += 22.0;

        // Featured heroes (only when no category filter).
        if self.active_category.is_none() {
            y = self.section(scene, tr, t, theme, x0, y, "Editor's Choice");
            y += 4.0;
            let featured = self.catalog.featured();
            let n = featured.len().min(3).max(1);
            let hero_w = (width - GAP * (n as f64 - 1.0)) / n as f64;
            let hero_h = 188.0;
            for (i, app) in featured.iter().take(n).enumerate() {
                let hx = x0 + i as f64 * (hero_w + GAP);
                let r = Rect::new(hx, y, hx + hero_w, y + hero_h);
                self.draw_hero(scene, tr, t, theme, targets, r, app);
            }
            y += hero_h + 28.0;
        }

        // Category grids.
        let cats: Vec<Category> = match self.active_category {
            Some(c) => vec![c],
            None => Category::ALL.to_vec(),
        };
        for cat in cats {
            let apps = self.catalog.in_category(cat);
            if apps.is_empty() {
                continue;
            }
            y = self.section(scene, tr, t, theme, x0, y, cat.label());
            y += 6.0;
            y = self.draw_grid(scene, tr, t, theme, targets, x0, y, width, &apps);
            y += 30.0;
        }

        // Return content height (distance from body top to bottom of
        // content, ignoring scroll).
        (y + self.scroll) - body.y0 + 12.0
    }

    /// Detail screen. Returns content height.
    fn draw_detail(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        app: &AppEntry,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let top = body.y0 + BODY_PAD_TOP - self.scroll;

        // ---- Header block ----
        let icon_sz = 96.0;
        let icon_r = Rect::new(x0, top, x0 + icon_sz, top + icon_sz);
        self.icon(scene, tr, t, theme, icon_r, app);

        let info_x = icon_r.x1 + 20.0;
        draw::text_w(scene, tr, t, &app.name, ty.x2l, TextWeight::Bold, c.text_primary, info_x, top, None);
        draw::text(scene, tr, t, &app.developer, ty.base, c.text_dim, info_x, top + 38.0, None);
        draw::text(scene, tr, t, app.source.label(), ty.xs, c.text_dim, info_x, top + 60.0, None);
        // Rating.
        let rw = draw::rating_stars(scene, t, info_x, top + 84.0, app.rating, 8.0, 3.0, c.semantic.warning, c.surface_white_20);
        draw::text_w(scene, tr, t, &app.rating_label(), ty.sm, TextWeight::Semibold, c.text_primary, info_x + rw + 12.0, top + 78.0, None);
        draw::text(scene, tr, t, &app.rating_count_label(), ty.xs, c.text_dim, info_x + rw + 12.0, top + 95.0, None);

        // Primary action (top-right).
        let btn_w = 200.0_f64;
        let btn_h = 44.0_f64;
        let btn = Rect::new(body.x1 - BODY_PAD_X - btn_w, top + 4.0, body.x1 - BODY_PAD_X, top + 4.0 + btn_h);
        self.action_control(scene, tr, t, theme, targets, btn, app, false);
        // Version + size under the button.
        let meta = format!("v{}  ·  {}", app.latest_version, app.size_label());
        let mw = draw::measure(tr, &meta, ty.xs).0;
        draw::text(scene, tr, t, &meta, ty.xs, c.text_dim, btn.x1 - mw, btn.y1 + 8.0, None);
        if let InstallState::UpdateAvailable(from) = self.live_state(app) {
            let upd = format!("Update available: {} → {}", from, app.latest_version);
            let uw = draw::measure(tr, &upd, ty.xs).0;
            draw::text_w(scene, tr, t, &upd, ty.xs, TextWeight::Medium, c.semantic.warning, btn.x1 - uw, btn.y1 + 24.0, None);
        }

        let mut y = top + icon_sz + 30.0;

        // ---- Screenshot strip ----
        if !app.shots.is_empty() {
            y = self.section(scene, tr, t, theme, x0, y, "Screenshots");
            y += 6.0;
            let n = app.shots.len();
            let shot_w = (width - GAP * (n as f64 - 1.0)) / n as f64;
            let shot_h = (shot_w * 0.62).min(190.0);
            for (i, sh) in app.shots.iter().enumerate() {
                let sx = x0 + i as f64 * (shot_w + GAP);
                let r = Rect::new(sx, y, sx + shot_w, y + shot_h);
                let tone_a = c.accent[sh.tone_a.min(10)];
                let tone_b = mix(c.neutral[sh.tone_b.min(10)], c.surfaces.base, 0.2);
                draw::screenshot_thumb(scene, tr, t, r, tone_a, tone_b, c.border_standard, &sh.caption, c.text_dim, ty.xs);
            }
            y += shot_h + 30.0;
        }

        // ---- About ----
        y = self.section(scene, tr, t, theme, x0, y, "About");
        y += 4.0;
        for para in app.description.split("\n\n") {
            let (_, h) = tr.measure(para, ty.sm, Some(width as f32));
            draw::text(scene, tr, t, para, ty.sm, mix(c.text_primary, c.text_dim, 0.25), x0, y, Some(width as f32));
            y += h as f64 + 12.0;
        }
        y += 14.0;

        // ---- Permissions ----
        y = self.section(scene, tr, t, theme, x0, y, "Permissions & sandbox");
        y += 8.0;
        for perm in &app.permissions {
            self.draw_permission_row(scene, tr, t, theme, x0, y, width, perm);
            y += 30.0;
        }
        y += 14.0;

        // ---- Information grid ----
        y = self.section(scene, tr, t, theme, x0, y, "Information");
        y += 8.0;
        let rows = [
            ("Version", app.latest_version.clone()),
            ("Download size", app.size_label()),
            ("Category", app.category.label().to_string()),
            ("Developer", app.developer.clone()),
            ("Source", app.source.label().to_string()),
            ("App ID", app.id.clone()),
        ];
        for (k, v) in rows {
            draw::text(scene, tr, t, k, ty.sm, c.text_dim, x0, y, None);
            draw::text_w(scene, tr, t, &v, ty.sm, TextWeight::Medium, c.text_primary, x0 + 160.0, y, None);
            // Divider.
            draw::fill_rect(scene, t, Rect::new(x0, y + 22.0, x0 + width, y + 23.0), alpha(c.border_standard, 0.5));
            y += 32.0;
        }

        (y + self.scroll) - body.y0 + 20.0
    }

    fn draw_permission_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x: f64,
        y: f64,
        width: f64,
        perm: &Permission,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let dot_c = if perm.sensitive { c.semantic.warning } else { c.semantic.success };
        let dot = Circle::new(Point::new(x + 5.0, y + 9.0), 4.0);
        scene.fill(vello::peniko::Fill::NonZero, t, dot_c, None, &dot);
        draw::text(scene, tr, t, &perm.label, ty.sm, c.text_primary, x + 18.0, y, Some((width - 120.0) as f32));
        if perm.sensitive {
            let tag = "sensitive";
            let tw = draw::measure(tr, tag, ty.xs).0;
            let r = Rect::new(x + width - tw - 16.0, y - 1.0, x + width, y + 17.0);
            draw::fill_round(scene, t, r, theme.tokens.radii.full, alpha(c.semantic.warning, 0.18));
            draw::text_w(scene, tr, t, tag, ty.xs, TextWeight::Medium, c.semantic.warning, r.x0 + 8.0, y + 1.0, None);
        }
    }

    /// A row in the Installed / Updates lists.
    fn draw_list_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
        show_update_arrow: bool,
        allow_remove: bool,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let hov = rect.contains(self.mouse);
        draw::fill_round(scene, t, rect, theme.tokens.radii.lg, c.surfaces.raised);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.lg, if hov { c.border_focus } else { c.border_standard }, 1.0);

        let pad = 14.0;
        let icon_sz = rect.height() - pad * 2.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x0 + pad + icon_sz, rect.y1 - pad);
        self.icon(scene, tr, t, theme, icon_r, app);

        let tx = icon_r.x1 + 14.0;
        draw::text_w(scene, tr, t, &app.name, ty.base, TextWeight::Bold, c.text_primary, tx, rect.y0 + pad + 2.0, None);
        let sub = if show_update_arrow {
            if let InstallState::UpdateAvailable(from) = self.live_state(app) {
                format!("{}  →  {}   ·   {}", from, app.latest_version, app.size_label())
            } else {
                format!("v{}   ·   {}", app.latest_version, app.size_label())
            }
        } else {
            let installed_v = self.snapshot.installed.get(&app.id).cloned().unwrap_or_else(|| app.latest_version.clone());
            format!("v{}   ·   {}   ·   {}", installed_v, app.size_label(), app.category.label())
        };
        let sub_color = if show_update_arrow { mix(c.text_dim, c.semantic.warning, 0.5) } else { c.text_dim };
        draw::text(scene, tr, t, &sub, ty.sm, sub_color, tx, rect.center().y + 2.0, None);

        // Action(s) at the right.
        let btn_w = if allow_remove { 200.0_f64 } else { 130.0_f64 };
        let btn_h = 34.0_f64;
        let btn = Rect::new(rect.x1 - pad - btn_w, rect.center().y - btn_h / 2.0, rect.x1 - pad, rect.center().y + btn_h / 2.0);
        self.action_control(scene, tr, t, theme, targets, btn, app, allow_remove);

        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    fn draw_installed(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        let installed = self.catalog.installed(&self.snapshot);
        y = self.section(scene, tr, t, theme, x0, y, &format!("Installed apps · {}", installed.len()));
        y += 12.0;

        if installed.is_empty() {
            y = self.empty_state(scene, tr, t, theme, x0, y, width, "Nothing installed yet", "Browse the store and install your first app.");
        } else {
            let row_h = 72.0;
            for app in &installed {
                let r = Rect::new(x0, y, x0 + width, y + row_h);
                self.draw_list_row(scene, tr, t, theme, targets, r, app, false, true);
                y += row_h + 12.0;
            }
        }
        (y + self.scroll) - body.y0 + 12.0
    }

    fn draw_updates(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        let updates = self.catalog.updatable(&self.snapshot);
        // Header row with Update All on the right.
        draw::text_w(scene, tr, t, &format!("Updates · {}", updates.len()), theme.tokens.typography.scale.lg, TextWeight::Bold, c.text_primary, x0, y, None);
        if !updates.is_empty() {
            let any_installing = updates.iter().any(|a| matches!(self.live_state(a), InstallState::Installing(_)));
            let btn_w = 150.0_f64;
            let btn = Rect::new(x0 + width - btn_w, y - 6.0, x0 + width, y + 28.0);
            if any_installing {
                Self::pill_button(scene, tr, t, theme, targets, btn, "Updating…", BtnStyle::Ghost, false, Action::SetTab(Tab::Updates));
            } else {
                Self::pill_button(scene, tr, t, theme, targets, btn, "Update All", BtnStyle::Primary, btn.contains(self.mouse), Action::UpdateAll);
            }
        }
        y += 36.0;

        if updates.is_empty() {
            y = self.empty_state(scene, tr, t, theme, x0, y, width, "You're all up to date", "Every installed app is running its latest version.");
        } else {
            let row_h = 72.0;
            for app in &updates {
                let r = Rect::new(x0, y, x0 + width, y + row_h);
                self.draw_list_row(scene, tr, t, theme, targets, r, app, true, false);
                y += row_h + 12.0;
            }
        }
        (y + self.scroll) - body.y0 + 12.0
    }

    #[allow(clippy::too_many_arguments)]
    fn empty_state(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, x0: f64, y: f64, width: f64, title: &str, subtitle: &str) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let box_h = 140.0;
        let r = Rect::new(x0, y, x0 + width, y + box_h);
        draw::fill_round(scene, t, r, theme.tokens.radii.lg, alpha(c.surfaces.raised, 0.5));
        draw::stroke_round(scene, t, r, theme.tokens.radii.lg, c.border_standard, 1.0);
        let (tw, _) = draw::measure(tr, title, ty.lg);
        draw::text_w(scene, tr, t, title, ty.lg, TextWeight::Bold, c.text_primary, r.center().x - tw / 2.0, r.center().y - 18.0, None);
        let (sw, _) = draw::measure(tr, subtitle, ty.sm);
        draw::text(scene, tr, t, subtitle, ty.sm, c.text_dim, r.center().x - sw / 2.0, r.center().y + 6.0, None);
        y + box_h
    }

    fn draw_toast(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme) {
        let Some((msg, _)) = &self.toast else { return };
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let (tw, _) = draw::measure(tr, msg, ty.sm);
        let w = tw + 40.0;
        let h = 40.0;
        let r = Rect::new(self.rect.center().x - w / 2.0, self.rect.y1 - h - 24.0, self.rect.center().x + w / 2.0, self.rect.y1 - 24.0);
        draw::fill_round(scene, t, r, theme.tokens.radii.full, c.accent[5]);
        draw::text_w(scene, tr, t, msg, ty.sm, TextWeight::Semibold, Color::WHITE, r.x0 + 20.0, r.center().y - ty.sm as f64 / 2.0 - 1.0, None);
    }

    /// Vertical scrollbar indicator on the right edge of the body.
    fn draw_scrollbar(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let max = self.max_scroll();
        if max <= 0.5 {
            return;
        }
        let body = self.body_rect();
        let track_h = body.height() - 12.0;
        let frac_visible = (body.height() / self.content_height).clamp(0.05, 1.0);
        let thumb_h = (track_h * frac_visible).max(28.0);
        let frac = (self.scroll / max).clamp(0.0, 1.0);
        let thumb_y = body.y0 + 6.0 + (track_h - thumb_h) * frac;
        let x1 = body.x1 - 5.0;
        let r = Rect::new(x1 - 4.0, thumb_y, x1, thumb_y + thumb_h);
        draw::fill_round(scene, t, r, theme.tokens.radii.full, alpha(theme.tokens.colors.text_dim, 0.5));
    }
}

// ===================================================================
// Widget impl
// ===================================================================

impl Widget for SoftwareStore {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // One-time default seed so a fresh document (and the --shots
        // path) is populated: a few installed apps, two a version behind
        // (Updates tab), and one mid-install (progress bar visible).
        if !StoreDoc::is_seeded(state) {
            let current = [
                ("org.mozilla.firefox", "129.0"),
                ("md.obsidian.Obsidian", "1.6.7"),
                ("org.signal.Signal", "7.20.0"),
                ("org.zealdocs.Zeal", "0.7.1"),
            ];
            let behind = [
                ("org.gimp.GIMP", "2.10.36"),
                ("com.visualstudio.code", "1.90.0"),
            ];
            let installing = [("org.libreoffice.LibreOffice", 0.45)];
            StoreDoc::seed_defaults(state, &current, &behind, &installing);
        }

        // Merge live probe (real flatpak/podman state) once, over the
        // seed. Only present in the live app — never in --shots.
        if !self.probe_merged {
            if let Some(probe) = self.probe.clone() {
                for (id, ver) in &probe.flatpak_versions {
                    if let Some(app) = self.catalog.get(id) {
                        let v = if ver.is_empty() { app.latest_version.clone() } else { ver.clone() };
                        StoreDoc::set_installed(state, id, &v);
                    }
                }
            }
            self.probe_merged = true;
        }

        // Advance / finalise any locally-tracked installs.
        let mut done: Vec<String> = Vec::new();
        for (id, start) in &self.install_start {
            let p = start.elapsed().as_secs_f64() / INSTALL_DURATION.as_secs_f64();
            if p >= 1.0 {
                done.push(id.clone());
            } else {
                StoreDoc::set_installing(state, id, p);
            }
        }
        for id in done {
            if let Some(app) = self.catalog.get(&id) {
                let v = app.latest_version.clone();
                StoreDoc::set_installed(state, &id, &v);
            }
            self.install_start.remove(&id);
        }
        self.finalize_pending = false;

        self.snapshot = StoreDoc::read_snapshot(state);
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
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
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
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Expire the toast.
        if let Some((_, at)) = &self.toast {
            if at.elapsed() > TOAST_DURATION {
                self.toast = None;
            }
        }

        let c = &theme.tokens.colors;
        // Full background.
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        let mut targets: Vec<HitTarget> = Vec::new();

        // Clamp scroll against the previous frame's content height.
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
        if self.scroll < 0.0 {
            self.scroll = 0.0;
        }

        // Body (under the header). We borrow the catalog immutably and
        // build the target list; nothing here mutates `self` other than
        // the local-only `finalize_pending` flag (set after the borrow).
        let content_height;
        let mut any_complete = false;
        {
            // Detect installs that reached 100% this frame so `needs_commit`
            // can nudge a finalising `sync`.
            for (_, start) in &self.install_start {
                if start.elapsed().as_secs_f64() / INSTALL_DURATION.as_secs_f64() >= 1.0 {
                    any_complete = true;
                }
            }

            content_height = if let Some(id) = self.detail.clone() {
                if let Some(app) = self.catalog.get(&id) {
                    self.draw_detail(scene, tr, t, theme, &mut targets, app)
                } else {
                    0.0
                }
            } else {
                match self.tab {
                    Tab::Browse => self.draw_browse(scene, tr, t, theme, &mut targets),
                    Tab::Installed => self.draw_installed(scene, tr, t, theme, &mut targets),
                    Tab::Updates => self.draw_updates(scene, tr, t, theme, &mut targets),
                }
            };
        }
        self.content_height = content_height;

        // Header paints last so it occludes any scrolled-up body content.
        self.draw_header(scene, tr, t, theme, &mut targets);

        self.draw_scrollbar(scene, t, theme);
        self.draw_toast(scene, tr, t, theme);

        self.hit_targets = targets;
        self.finalize_pending = any_complete;
        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Repaint so hover states track the pointer.
                self.dirty = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Reverse iterate so later-drawn (topmost) targets win.
                let hit = self
                    .hit_targets
                    .iter()
                    .rev()
                    .find(|h| h.rect.contains(self.mouse))
                    .map(|h| h.action.clone());
                if let Some(action) = hit {
                    // Clicking anywhere that isn't the search box unfocuses it.
                    if action != Action::FocusSearch {
                        self.search_focused = false;
                    }
                    self.apply(action, state);
                    *handled = true;
                } else {
                    // Click on empty space dismisses search focus.
                    if self.search_focused {
                        self.search_focused = false;
                        self.dirty = true;
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                match &key_event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if self.detail.is_some() {
                            self.detail = None;
                            self.reset_scroll();
                        } else if !self.search_query.is_empty() {
                            self.search_query.clear();
                        } else {
                            self.search_focused = false;
                        }
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) if self.search_focused => {
                        self.search_query.pop();
                        self.reset_scroll();
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) if self.search_focused => {
                        // Open the first search result.
                        if let Some(first) = self.catalog.search(&self.search_query, self.active_category).first() {
                            let id = first.id.clone();
                            self.detail = Some(id);
                            self.reset_scroll();
                        }
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.scroll = (self.scroll + 60.0).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.scroll = (self.scroll - 60.0).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.scroll = (self.scroll + self.body_rect().height() * 0.9).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.scroll = (self.scroll - self.body_rect().height() * 0.9).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Character(s) if ctrl && s.eq_ignore_ascii_case("f") => {
                        self.search_focused = true;
                        self.tab = Tab::Browse;
                        self.detail = None;
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Character(s) => {
                        // `/` focuses search when not already typing.
                        if !self.search_focused && s.as_str() == "/" {
                            self.search_focused = true;
                            self.tab = Tab::Browse;
                            self.detail = None;
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                        if self.search_focused && !ctrl {
                            for ch in s.chars() {
                                if !ch.is_control() {
                                    self.search_query.push(ch);
                                }
                            }
                            self.reset_scroll();
                            self.dirty = true;
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SoftwareStore"
    }

    fn test_id(&self) -> Option<&str> {
        Some("software_store")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || !self.install_start.is_empty() || self.toast.is_some()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        !self.install_start.is_empty() || self.toast.is_some()
    }

    fn needs_commit(&self) -> bool {
        // Rising-edge nudge: when an install finishes, return true so the
        // App schedules one finalising `sync` (which writes the installed
        // version and clears the in-flight record).
        self.finalize_pending
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
        // Root rect is the whole window — always paint, never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SoftwareStore {
        SoftwareStore::new(None)
    }

    #[test]
    fn sync_seeds_and_populates_snapshot() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        // Seeded: at least the four "current" installs + two behind.
        assert!(s.snapshot.installed.len() >= 6, "seed installed: {:?}", s.snapshot.installed);
        assert!(s.snapshot.installing.contains_key("org.libreoffice.LibreOffice"));
    }

    #[test]
    fn updates_tab_has_entries_after_seed() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let ups = s.catalog.updatable(&s.snapshot);
        assert!(ups.len() >= 2, "expected >=2 updatable after seed, got {}", ups.len());
    }

    #[test]
    fn install_action_marks_installing_then_finalizes() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        // PCSX2 starts not installed.
        let id = "net.pcsx2.PCSX2";
        assert_eq!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot), InstallState::Available);
        s.apply(Action::Install(id.into()), &mut doc);
        s.sync(&mut doc, &automerge::ROOT);
        // Mid-install it should be Installing (progress < 1 right away).
        assert!(matches!(s.live_state(s.catalog.get(id).unwrap()), InstallState::Installing(_)));
        // Force completion: backdate the start instant beyond the duration.
        s.install_start.insert(id.into(), Instant::now() - INSTALL_DURATION - Duration::from_secs(1));
        s.sync(&mut doc, &automerge::ROOT);
        assert_eq!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot), InstallState::Installed);
    }

    #[test]
    fn remove_uninstalls() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let id = "org.mozilla.firefox";
        assert!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot).is_installed_or_update());
        s.apply(Action::Remove(id.into()), &mut doc);
        assert_eq!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot), InstallState::Available);
    }

    #[test]
    fn search_focus_and_typing_filters() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        s.apply(Action::FocusSearch, &mut doc);
        assert!(s.search_focused);
        s.search_query = "firefox".into();
        let results = s.catalog.search(&s.search_query, s.active_category);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "org.mozilla.firefox");
    }

    #[test]
    fn update_all_starts_all_updatable() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let before = s.catalog.updatable(&s.snapshot).len();
        assert!(before >= 2);
        s.apply(Action::UpdateAll, &mut doc);
        s.sync(&mut doc, &automerge::ROOT);
        // Every previously-updatable app is now in flight.
        assert_eq!(s.install_start.len(), before);
    }
}
