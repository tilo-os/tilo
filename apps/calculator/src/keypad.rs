//! The Calculator mode: a live display, a memory bar, a scientific keypad,
//! and a collapsible history sidebar. This is a plain renderer driven by the
//! [`CalcModel`]; it owns only layout + hover + sidebar-open caches, never
//! state.
//!
//! Two responsive behaviours live here:
//!  * the keypad picks a [`KeypadLayout`] (compact / standard / scientific)
//!    from its available width, so the pad stays usable from a narrow strip
//!    up to a wide window; and
//!  * the history sidebar follows the qOS Adwaita overlay-sidebar pattern: it
//!    docks beside the keypad when the window is wide, and collapses to a
//!    toggle that opens an overlay drawer (drawn over the keypad with a
//!    dismiss scrim) when the window is too narrow to dock it.

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
use crate::model::{CalcModel, Key, KeyRole, KeypadLayout};

/// Memory-bar buttons, left → right.
pub const MEM_KEYS: [Key; 4] = [Key::MemClear, Key::MemRecall, Key::MemAdd, Key::MemSub];

/// Below this body width the history sidebar can't dock without squeezing the
/// keypad, so it becomes a collapsible overlay drawer instead.
const SIDEBAR_DOCK_MIN_W: f64 = 760.0;
/// Docked sidebar width, clamped to a fraction of the body.
const SIDEBAR_W_FRAC: f64 = 0.30;
const SIDEBAR_W_MIN: f64 = 280.0;
const SIDEBAR_W_MAX: f64 = 380.0;
/// Overlay drawer width when the sidebar floats over the keypad.
const SIDEBAR_OVERLAY_W: f64 = 320.0;

pub struct CalcScreen {
    keys: Vec<Key>,
    /// The layout the cached `keys`/`key_rects` were built for, so we only
    /// rebuild the key list when the breakpoint actually changes.
    layout_kind: KeypadLayout,
    cols: usize,
    key_rects: Vec<Rect>,
    mem_rects: Vec<Rect>,
    history_rects: Vec<(usize, Rect)>,
    clear_hist_rect: Rect,
    /// Toggle pill that shows/hides the history sidebar.
    toggle_rect: Rect,
    pub hovered_key: Option<usize>,
    pub hovered_mem: Option<usize>,
    pub pressed_key: Option<usize>,
    pub history_scroll: f64,
    history_view: Rect,
    history_content_h: f64,
    /// The history panel's outer rect this frame (docked column or overlay
    /// drawer), and whether it is currently shown.
    sidebar_rect: Rect,
    sidebar_open: bool,
    /// Whether the sidebar is floating over the keypad (vs. docked beside it).
    sidebar_overlay: bool,
    /// Scrim region behind an open overlay drawer; a click here dismisses it.
    scrim_rect: Rect,
    /// Whether the user has explicitly toggled the sidebar this session. Until
    /// they do, the open/closed default tracks the breakpoint (docked-open
    /// when wide, collapsed when narrow) so resizing feels natural.
    user_toggled: bool,
}

impl Default for CalcScreen {
    fn default() -> Self {
        let layout_kind = KeypadLayout::Scientific;
        Self {
            keys: layout_kind.keys(),
            layout_kind,
            cols: layout_kind.cols(),
            key_rects: Vec::new(),
            mem_rects: Vec::new(),
            history_rects: Vec::new(),
            clear_hist_rect: Rect::ZERO,
            toggle_rect: Rect::ZERO,
            hovered_key: None,
            hovered_mem: None,
            pressed_key: None,
            history_scroll: 0.0,
            history_view: Rect::ZERO,
            history_content_h: 0.0,
            sidebar_rect: Rect::ZERO,
            sidebar_open: true,
            sidebar_overlay: false,
            scrim_rect: Rect::ZERO,
            user_toggled: false,
        }
    }
}

impl CalcScreen {
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// Pick the keypad layout for `keypad_w` and rebuild the key list if the
    /// breakpoint changed. Cheap when unchanged (the common case).
    fn ensure_layout(&mut self, keypad_w: f64) {
        let want = KeypadLayout::for_width(keypad_w);
        if want != self.layout_kind {
            self.layout_kind = want;
            self.cols = want.cols();
            self.keys = want.keys();
            self.pressed_key = None;
            self.hovered_key = None;
        }
    }

    /// Resolve the sidebar mode (docked vs. overlay) and whether it's shown,
    /// from the body width and any explicit user toggle. Returns the keypad's
    /// usable width (the body minus a docked sidebar).
    fn resolve_sidebar(&mut self, area: Rect, pad: f64) -> f64 {
        let can_dock = area.width() >= SIDEBAR_DOCK_MIN_W;
        self.sidebar_overlay = !can_dock;

        // Default open-state tracks the breakpoint until the user overrides it:
        // docked layouts default open, narrow (overlay) layouts default closed
        // so a small window is never forced to give up keypad space.
        if !self.user_toggled {
            self.sidebar_open = can_dock;
        } else if can_dock {
            // Re-widening past the dock threshold re-docks; an overlay that was
            // left open stays open as a docked column, which reads naturally.
        }

        if can_dock && self.sidebar_open {
            let w = (area.width() * SIDEBAR_W_FRAC).clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
            self.sidebar_rect = Rect::new(area.x1 - w, area.y0, area.x1, area.y1);
            area.width() - w - pad
        } else {
            // Overlay (or collapsed): keypad gets the whole body width.
            if self.sidebar_overlay && self.sidebar_open {
                let w = SIDEBAR_OVERLAY_W.min(area.width() - 40.0);
                self.sidebar_rect = Rect::new(area.x1 - w, area.y0, area.x1, area.y1);
                self.scrim_rect = Rect::new(area.x0, area.y0, area.x1 - w, area.y1);
            } else {
                self.sidebar_rect = Rect::ZERO;
                self.scrim_rect = Rect::ZERO;
            }
            area.width() - pad * 2.0
        }
    }

    /// Recompute all geometry for the given body `area`. Called at the top of
    /// both draw and event handling so hit-tests match the last paint.
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        let pad = 16.0;
        let keypad_w = self.resolve_sidebar(area, pad);
        self.ensure_layout(keypad_w);

        // Left column = display + memory bar + keypad. Its right edge is the
        // body minus a docked sidebar (overlay/collapsed → full width).
        let left_x1 = if self.sidebar_overlay || !self.sidebar_open {
            area.x1 - pad
        } else {
            self.sidebar_rect.x0 - pad
        };
        let left = Rect::new(area.x0 + pad, area.y0 + pad, left_x1, area.y1 - pad);

        // Header row above the display holds the sidebar toggle on the right.
        let toggle_w = 132.0;
        let toggle_h = 28.0;
        self.toggle_rect = Rect::new(left.x1 - toggle_w, left.y0, left.x1, left.y0 + toggle_h);
        let header_h = toggle_h + 8.0;

        let display_h = 124.0;
        let mem_h = 44.0;
        let gap = 12.0;
        let display_top = left.y0 + header_h;
        let mem_top = display_top + display_h + gap;
        let grid_top = mem_top + mem_h + gap;

        // Memory bar: 4 buttons on the left.
        self.mem_rects.clear();
        let mem_btn_w = 64.0;
        let mem_gap = 8.0;
        for i in 0..4 {
            let x0 = left.x0 + i as f64 * (mem_btn_w + mem_gap);
            self.mem_rects.push(Rect::new(x0, mem_top, x0 + mem_btn_w, mem_top + mem_h));
        }

        // Keypad grid (column count is layout-dependent).
        self.key_rects.clear();
        let cols = self.cols;
        let rows = self.keys.len().div_ceil(cols);
        let cell_gap = 8.0;
        let grid = Rect::new(left.x0, grid_top, left.x1, left.y1);
        let cw = (grid.width() - cell_gap * (cols as f64 - 1.0)) / cols as f64;
        let ch = (grid.height() - cell_gap * (rows as f64 - 1.0)) / rows as f64;
        for idx in 0..self.keys.len() {
            let r = idx / cols;
            let c = idx % cols;
            let x0 = grid.x0 + c as f64 * (cw + cell_gap);
            let y0 = grid.y0 + r as f64 * (ch + cell_gap);
            self.key_rects.push(Rect::new(x0, y0, x0 + cw, y0 + ch));
        }

        // History list geometry (only meaningful when the sidebar is shown).
        self.layout_history(model);
    }

    fn layout_history(&mut self, model: &CalcModel) {
        self.history_rects.clear();
        if !self.sidebar_open || self.sidebar_rect == Rect::ZERO {
            self.history_view = Rect::ZERO;
            return;
        }
        let right = self.sidebar_rect;
        let header_h = 48.0;
        let row_h = 56.0;
        self.clear_hist_rect =
            Rect::new(right.x1 - 76.0, right.y0 + 12.0, right.x1 - 16.0, right.y0 + 36.0);
        self.history_view = Rect::new(right.x0, right.y0 + header_h, right.x1, right.y1);
        self.history_content_h = model.history.len() as f64 * row_h;
        let max_scroll = (self.history_content_h - self.history_view.height()).max(0.0);
        self.history_scroll = self.history_scroll.clamp(0.0, max_scroll);

        let mut y = self.history_view.y0 - self.history_scroll;
        for (i, _) in model.history.iter().enumerate() {
            let r = Rect::new(self.history_view.x0 + 12.0, y, self.history_view.x1 - 12.0, y + row_h);
            if r.y1 > self.history_view.y0 && r.y0 < self.history_view.y1 {
                self.history_rects.push((i, r));
            }
            y += row_h;
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        area: Rect,
    ) {
        self.layout(area, model);
        let c = &theme.tokens.colors;
        let pad = 16.0;

        let left_x1 = if self.sidebar_overlay || !self.sidebar_open {
            area.x1 - pad
        } else {
            self.sidebar_rect.x0 - pad
        };
        let left = Rect::new(area.x0 + pad, area.y0 + pad, left_x1, area.y1 - pad);
        let header_h = 36.0;
        let display_top = left.y0 + header_h;

        // ---- Header: sidebar toggle ---------------------------------------
        self.draw_toggle(scene, t, tr, theme);

        // ---- Display panel -------------------------------------------------
        let display = Rect::new(left.x0, display_top, left.x1, display_top + 124.0);
        draw::fill_rrect(scene, t, display, theme.radius_standard, c.neutral[0]);
        draw::stroke_rrect(scene, t, display, theme.radius_standard, c.border_standard, 1.0);

        // status line: angle mode + Ans
        let status = Rect::new(display.x0 + 16.0, display.y0 + 8.0, display.x1 - 16.0, display.y0 + 30.0);
        let mode_txt = if model.deg { "DEG" } else { "RAD" };
        draw::text_in(scene, tr, t, status, mode_txt, 12.0, c.accent[6], Align::Left, TextWeight::Semibold);
        let ans_txt = format!("Ans = {}", engine::format_number(model.ans));
        draw::text_in(scene, tr, t, status, &ans_txt, 12.0, c.text_dim, Align::Right, TextWeight::Regular);

        // expression line
        let expr_rect = Rect::new(display.x0 + 16.0, display.y0 + 30.0, display.x1 - 16.0, display.y0 + 78.0);
        let expr_shown = if model.expr.trim().is_empty() {
            "0".to_string()
        } else {
            engine::prettify(&model.expr)
        };
        let expr_color = if model.expr.trim().is_empty() { c.text_dim } else { c.text_primary };
        draw::text_in(scene, tr, t, expr_rect, &expr_shown, 30.0, expr_color, Align::Right, TextWeight::Medium);

        // result / preview line
        let res_rect = Rect::new(display.x0 + 16.0, display.y0 + 78.0, display.x1 - 16.0, display.y1 - 6.0);
        let (res_txt, res_color, weight) = match engine::evaluate(&model.expr, &model.ctx()) {
            Ok(v) if !model.expr.trim().is_empty() => {
                let col = if model.after_equals { c.accent[5] } else { c.text_dim };
                (format!("= {}", engine::format_number(v)), col, TextWeight::Bold)
            }
            _ => (String::new(), c.text_dim, TextWeight::Regular),
        };
        draw::text_in(scene, tr, t, res_rect, &res_txt, 34.0, res_color, Align::Right, weight);

        // ---- Memory bar ----------------------------------------------------
        for (i, key) in MEM_KEYS.iter().enumerate() {
            let r = self.mem_rects[i];
            let hovered = self.hovered_mem == Some(i);
            let active = matches!(key, Key::MemRecall | Key::MemClear) && model.memory != 0.0;
            let fill = if hovered { c.neutral[3] } else { c.neutral[2] };
            draw::fill_rrect(scene, t, r, 8.0, fill);
            let txt_col = if active { c.accent[6] } else { c.text_dim };
            draw::text_in(scene, tr, t, r, key.label(), 13.0, txt_col, Align::Center, TextWeight::Semibold);
        }
        let mem_label = Rect::new(self.mem_rects[3].x1 + 12.0, self.mem_rects[3].y0, left.x1, self.mem_rects[3].y1);
        let mem_txt = format!("M = {}", engine::format_number(model.memory));
        let mem_col = if model.memory != 0.0 { c.accent[6] } else { c.text_dim };
        draw::text_in(scene, tr, t, mem_label, &mem_txt, 14.0, mem_col, Align::Right, TextWeight::Medium);

        // ---- Keypad --------------------------------------------------------
        for (i, key) in self.keys.iter().enumerate() {
            let r = self.key_rects[i];
            let hovered = self.hovered_key == Some(i);
            let pressed = self.pressed_key == Some(i);
            let (fill, text_col) = key_colors(theme, key.role(), hovered, pressed);
            draw::fill_rrect(scene, t, r, 10.0, fill);
            if key.role() == KeyRole::Accent {
                draw::stroke_rrect(scene, t, r, 10.0, c.accent[6], 1.0);
            }
            let size = match key.role() {
                KeyRole::Digit | KeyRole::Operator | KeyRole::Accent => 22.0,
                _ => 16.0,
            };
            draw::text_in(scene, tr, t, r, key.label(), size, text_col, Align::Center, TextWeight::Medium);
        }

        // ---- History sidebar ----------------------------------------------
        // Overlay mode draws a dismiss scrim over the keypad first, then the
        // drawer floats on top; docked mode just paints the column in place.
        if self.sidebar_open && self.sidebar_rect != Rect::ZERO {
            if self.sidebar_overlay {
                draw::fill_rect(scene, t, self.scrim_rect, draw::with_alpha(Color::BLACK, 110));
            }
            self.draw_history(scene, t, tr, theme, model);
        }
    }

    fn draw_toggle(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let r = self.toggle_rect;
        let on = self.sidebar_open;
        let fill = if on { draw::mix(c.surfaces.raised, c.accent[5], 0.18) } else { c.neutral[2] };
        draw::fill_rrect(scene, t, r, 8.0, fill);
        draw::stroke_rrect(scene, t, r, 8.0, if on { c.accent[5] } else { c.border_standard }, 1.0);
        let label = if on { "Hide history" } else { "Show history" };
        let col = if on { c.accent[6] } else { c.text_dim };
        draw::text_in(scene, tr, t, r, label, 12.0, col, Align::Center, TextWeight::Semibold);
    }

    fn draw_history(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, model: &CalcModel) {
        let c = &theme.tokens.colors;
        let right = self.sidebar_rect;

        // A docked column blends with the base; a floating drawer reads as a
        // raised surface with its own left edge + shadow-ish border.
        let panel_bg = if self.sidebar_overlay { c.surfaces.raised } else { c.surfaces.base };
        draw::fill_rect(scene, t, right, panel_bg);
        draw::line(scene, t, Point::new(right.x0, right.y0), Point::new(right.x0, right.y1), c.border_standard, if self.sidebar_overlay { 1.5 } else { 1.0 });

        let header = Rect::new(right.x0 + 16.0, right.y0, right.x1 - 16.0, right.y0 + 48.0);
        draw::text_in(scene, tr, t, header, "History", 16.0, c.text_primary, Align::Left, TextWeight::Semibold);
        draw::fill_rrect(scene, t, self.clear_hist_rect, 6.0, c.neutral[2]);
        draw::text_in(scene, tr, t, self.clear_hist_rect, "Clear", 12.0, c.text_dim, Align::Center, TextWeight::Medium);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &self.history_view,
        );
        if model.history.is_empty() {
            draw::text_in(
                scene,
                tr,
                t,
                self.history_view,
                "No calculations yet",
                14.0,
                c.text_dim,
                Align::Center,
                TextWeight::Regular,
            );
        }
        for (i, r) in &self.history_rects {
            let entry = &model.history[*i];
            let row = Rect::new(r.x0, r.y0 + 4.0, r.x1, r.y1 - 4.0);
            draw::fill_rrect(scene, t, row, 8.0, c.neutral[1]);
            let expr_r = Rect::new(row.x0 + 12.0, row.y0 + 6.0, row.x1 - 12.0, row.y0 + 26.0);
            draw::text_in(
                scene,
                tr,
                t,
                expr_r,
                &engine::prettify(&entry.expr),
                13.0,
                c.text_dim,
                Align::Left,
                TextWeight::Regular,
            );
            let res_r = Rect::new(row.x0 + 12.0, row.y0 + 24.0, row.x1 - 12.0, row.y1 - 4.0);
            draw::text_in(
                scene,
                tr,
                t,
                res_r,
                &format!("= {}", entry.result),
                18.0,
                c.text_primary,
                Align::Right,
                TextWeight::Semibold,
            );
        }
        scene.pop_layer();
    }

    // -- hit-testing ---------------------------------------------------------

    pub fn key_at(&self, p: Point) -> Option<usize> {
        // An open overlay drawer captures clicks over it (so a key behind the
        // scrim isn't pressed through the drawer).
        if self.sidebar_open && self.sidebar_overlay && self.sidebar_rect.contains(p) {
            return None;
        }
        self.key_rects.iter().position(|r| r.contains(p))
    }
    pub fn mem_at(&self, p: Point) -> Option<usize> {
        if self.sidebar_open && self.sidebar_overlay && self.sidebar_rect.contains(p) {
            return None;
        }
        self.mem_rects.iter().position(|r| r.contains(p))
    }
    /// History index under `p`, if the click hit a row inside the viewport.
    pub fn history_at(&self, p: Point) -> Option<usize> {
        if !self.sidebar_open || !self.history_view.contains(p) {
            return None;
        }
        self.history_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }
    pub fn clear_history_at(&self, p: Point) -> bool {
        self.sidebar_open && self.clear_hist_rect.contains(p)
    }
    pub fn history_viewport(&self) -> Rect {
        if self.sidebar_open { self.history_view } else { Rect::ZERO }
    }

    /// Whether `p` hit the show/hide-history toggle pill.
    pub fn toggle_at(&self, p: Point) -> bool {
        self.toggle_rect.contains(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open overlay drawer.
    pub fn scrim_at(&self, p: Point) -> bool {
        self.sidebar_open && self.sidebar_overlay && self.scrim_rect.contains(p)
    }
    /// Flip the sidebar open/closed and remember that the user chose this, so
    /// the breakpoint default no longer overrides them.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        self.user_toggled = true;
    }
    /// Force the overlay drawer shut (e.g. a scrim tap). Records the override.
    pub fn close_sidebar(&mut self) {
        self.sidebar_open = false;
        self.user_toggled = true;
    }
    pub fn sidebar_open(&self) -> bool {
        self.sidebar_open
    }
}

fn key_colors(theme: &Theme, role: KeyRole, hovered: bool, pressed: bool) -> (Color, Color) {
    let c = &theme.tokens.colors;
    let (base, text) = match role {
        KeyRole::Digit => (c.neutral[2], c.text_primary),
        KeyRole::Operator => (c.accent[2], c.accent[8]),
        // A visible card that sits between the base background and the digit
        // keys, so the scientific column reads as buttons (not bare text)
        // while staying a clear visual tier below the number pad.
        KeyRole::Function => (draw::mix(c.surfaces.base, c.neutral[2], 0.55), c.neutral[7]),
        KeyRole::Accent => (c.accent[5], Color::WHITE),
        KeyRole::Warm => (draw::mix(c.neutral[2], c.semantic.danger, 0.28), c.text_primary),
    };
    let fill = if pressed {
        draw::mix(base, Color::BLACK, 0.18)
    } else if hovered {
        draw::mix(base, Color::WHITE, 0.10)
    } else {
        base
    };
    (fill, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CalcModel;

    fn body(w: f64) -> Rect {
        Rect::new(0.0, 60.0, w, 860.0)
    }

    #[test]
    fn wide_window_docks_sidebar_open_by_default() {
        let mut s = CalcScreen::default();
        let m = CalcModel::seeded();
        s.layout(body(1200.0), &m);
        assert!(s.sidebar_open(), "wide window should default to history shown");
        assert!(!s.sidebar_overlay, "wide window should dock, not overlay");
        assert!(s.sidebar_rect.width() > 0.0);
    }

    #[test]
    fn narrow_window_collapses_sidebar_by_default() {
        let mut s = CalcScreen::default();
        let m = CalcModel::seeded();
        s.layout(body(520.0), &m);
        assert!(!s.sidebar_open(), "narrow window must NOT force the sidebar open");
        assert!(s.sidebar_overlay, "narrow window uses the overlay drawer mode");
        assert_eq!(s.history_viewport(), Rect::ZERO, "collapsed → no history viewport");
    }

    #[test]
    fn narrow_window_opens_as_overlay_with_scrim() {
        let mut s = CalcScreen::default();
        let m = CalcModel::seeded();
        s.layout(body(520.0), &m);
        // user taps "Show history"
        s.toggle_sidebar();
        s.layout(body(520.0), &m);
        assert!(s.sidebar_open());
        assert!(s.sidebar_overlay, "still an overlay at this width");
        assert!(s.scrim_rect.width() > 0.0, "overlay must lay down a dismiss scrim");
        // a scrim tap dismisses it
        let scrim_pt = Point::new(s.scrim_rect.x0 + 10.0, s.scrim_rect.center().y);
        assert!(s.scrim_at(scrim_pt));
        s.close_sidebar();
        s.layout(body(520.0), &m);
        assert!(!s.sidebar_open());
    }

    #[test]
    fn overlay_drawer_captures_clicks_over_it() {
        let mut s = CalcScreen::default();
        let m = CalcModel::seeded();
        s.layout(body(520.0), &m);
        s.toggle_sidebar();
        s.layout(body(520.0), &m);
        // a point inside the drawer is not a keypad/mem press
        let over = Point::new(s.sidebar_rect.center().x, s.sidebar_rect.center().y);
        assert_eq!(s.key_at(over), None, "drawer must not press the key behind it");
        assert_eq!(s.mem_at(over), None);
    }

    #[test]
    fn user_choice_overrides_breakpoint_default_when_wide() {
        let mut s = CalcScreen::default();
        let m = CalcModel::seeded();
        s.layout(body(1200.0), &m);
        assert!(s.sidebar_open());
        // user explicitly hides it; that choice sticks across redraws
        s.toggle_sidebar();
        s.layout(body(1200.0), &m);
        assert!(!s.sidebar_open(), "explicit hide must survive a relayout");
    }

    #[test]
    fn keypad_layout_follows_width() {
        let mut s = CalcScreen::default();
        let m = CalcModel::seeded();
        // very narrow body → compact pad (4 cols)
        s.layout(body(440.0), &m);
        assert_eq!(s.cols, KeypadLayout::Compact.cols());
        // wide body, sidebar docked → keypad still gets enough width to be
        // scientific or at least standard (never compact at this size)
        s.layout(body(1400.0), &m);
        assert!(s.cols >= KeypadLayout::Standard.cols());
        assert_eq!(s.key_rects.len(), s.keys.len());
    }
}
