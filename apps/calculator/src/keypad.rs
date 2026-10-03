//! The Calculator mode: a live display, a memory bar, a scientific keypad,
//! and a collapsible history sidebar. This is a plain renderer driven by the
//! [`CalcModel`]; it owns only layout + hover + sidebar-open caches, never
//! state.
//!
//! Two responsive behaviours live here:
//!  * the keypad picks a [`KeypadLayout`] (compact / standard / scientific)
//!    from its available width, so the pad stays usable from a narrow strip
//!    up to a wide window; and
//!  * the history sidebar follows the Tilo Adwaita overlay-sidebar pattern: it
//!    docks beside the keypad when the window is wide, and collapses to a
//!    toggle that opens an overlay drawer (drawn over the keypad with a
//!    dismiss scrim) when the window is too narrow to dock it.

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{SidebarDrawer, SidebarSide};
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
/// The shortest a key gets before the chrome around the pad gives way.
const KEY_MIN_H: f64 = ui::tokens::Metrics::BASE.min_pointer_target as f64;
/// The narrowest a key column gets beside the display.
const KEY_MIN_W: f64 = ui::tokens::Spacing::BASE.s12 as f64;
// Display card: status line, expression line and result line in their fonts.
const DISPLAY_H: f64 = 124.0;
// The display without its status line: the expression and result lines at
// their short font sizes.
const DISPLAY_SHORT_H: f64 = 72.0;
const MEM_H: f64 = ui::tokens::Metrics::BASE.panel_row_height as f64;
// The memory row beside a short display, between the pointer minimum and a row.
const MEM_SHORT_H: f64 = 36.0;
// Fits the widest memory label ("MR") with room for its press state.
const MEM_BTN_W: f64 = 64.0;
const MEM_GAP: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// The narrowest a memory button gets in a short window.
const MEM_BTN_MIN_W: f64 = ui::tokens::Metrics::BASE.panel_row_height as f64;
// The sidebar toggle pill's width, which `SidebarDrawer::draw_toggle` does not expose.
const TOGGLE_W: f64 = 46.0;
const CHROME_GAP: f64 = ui::tokens::Spacing::BASE.s3 as f64;
/// The narrowest column beside the pad: display over four narrowed memory
/// buttons and the sidebar toggle.
const SIDE_CHROME_MIN_W: f64 = 4.0 * MEM_BTN_MIN_W + 3.0 * MEM_GAP + CHROME_GAP + TOGGLE_W;
/// The narrowest a four-column pad beside the display may be.
const SIDE_PAD_MIN_W: f64 = 4.0 * KEY_MIN_W + 3.0 * MEM_GAP;

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
    /// Expression line used to anchor the platform IME candidate window.
    expression_rect: Rect,
    display_rect: Rect,
    pub hovered_key: Option<usize>,
    pub hovered_mem: Option<usize>,
    pub pressed_key: Option<usize>,
    pub history_scroll: f64,
    history_view: Rect,
    history_content_h: f64,
    /// The history panel's outer rect this frame (docked column or overlay
    /// drawer). Derived from the shared [`SidebarDrawer`].
    sidebar_rect: Rect,
    /// The shared collapsible sidebar-drawer that owns the docked-vs-overlay
    /// decision, the eased reveal, the scrim, and the user-toggle memory. The
    /// history is a standard right-anchored sidebar.
    drawer: SidebarDrawer,
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
            expression_rect: Rect::ZERO,
            display_rect: Rect::ZERO,
            hovered_key: None,
            hovered_mem: None,
            pressed_key: None,
            history_scroll: 0.0,
            history_view: Rect::ZERO,
            history_content_h: 0.0,
            sidebar_rect: Rect::ZERO,
            drawer: SidebarDrawer::new()
                .side(SidebarSide::Right)
                .breakpoint(SIDEBAR_DOCK_MIN_W)
                .docked_width(SIDEBAR_W_MIN)
                .overlay_width(SIDEBAR_OVERLAY_W),
        }
    }
}

impl CalcScreen {
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// Bench/test helper: screen-space center of keypad key `i`, once the pad
    /// has been laid out. Used by the headless interaction bench + tests.
    #[doc(hidden)]
    pub fn key_center(&self, i: usize) -> Option<Point> {
        self.key_rects.get(i).map(ui::kurbo::Rect::center)
    }
    /// Key geometry for the app's custom-canvas accessibility tree.
    pub fn key_rect(&self, i: usize) -> Option<Rect> {
        self.key_rects.get(i).copied()
    }

    /// Bench/test helper: index of the first keypad key whose label is `label`.
    #[doc(hidden)]
    pub fn key_index_for_label(&self, label: &str) -> Option<usize> {
        self.keys.iter().position(|k| k.label() == label)
    }

    /// Test helper: screen-space center of memory-bar button `i`, once laid out.
    #[doc(hidden)]
    pub fn mem_center(&self, i: usize) -> Option<Point> {
        self.mem_rects.get(i).map(ui::kurbo::Rect::center)
    }
    /// Memory-button geometry for the app's custom-canvas accessibility tree.
    pub fn mem_rect(&self, i: usize) -> Option<Rect> {
        self.mem_rects.get(i).copied()
    }

    /// Test helper: screen-space center of the history row for entry `i`, if it
    /// is currently laid out (sidebar shown and the row is in the viewport).
    #[doc(hidden)]
    pub fn history_row_center(&self, i: usize) -> Option<Point> {
        self.history_rects
            .iter()
            .find(|(idx, _)| *idx == i)
            .map(|(_, r)| r.center())
    }
    /// Geometry for a visible history row, if it is in the scroll viewport.
    pub fn history_row_rect(&self, i: usize) -> Option<Rect> {
        self.history_rects
            .iter()
            .find(|(idx, _)| *idx == i)
            .map(|(_, rect)| *rect)
    }

    /// Test helper: screen-space center of the "Clear" history button, if the
    /// sidebar is currently shown.
    #[doc(hidden)]
    pub fn clear_history_center(&self) -> Option<Point> {
        (self.drawer.is_open() && self.clear_hist_rect != Rect::ZERO)
            .then(|| self.clear_hist_rect.center())
    }
    pub fn clear_history_rect(&self) -> Rect {
        self.clear_hist_rect
    }
    pub fn toggle_rect(&self) -> Rect {
        self.toggle_rect
    }

    pub fn expression_ime_rect(&self) -> Option<Rect> {
        (self.expression_rect != Rect::ZERO).then(|| {
            Rect::new(
                self.expression_rect.x1 - 1.0,
                self.expression_rect.y0,
                self.expression_rect.x1,
                self.expression_rect.y1,
            )
        })
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

    /// Resolve the sidebar mode (docked vs. overlay) and whether it's shown via
    /// the shared [`SidebarDrawer`], returning the keypad's usable width (the
    /// body minus a docked sidebar). The docked / overlay widths are reconfigured
    /// per-frame so the docked column stays a fraction of the body.
    fn resolve_sidebar(&mut self, area: Rect, pad: f64) -> f64 {
        // Docked width tracks the body (a fraction, clamped); overlay width is a
        // fixed drawer that never eats the whole narrow window.
        self.drawer
            .set_docked_width((area.width() * SIDEBAR_W_FRAC).clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX));
        self.drawer
            .set_overlay_width(SIDEBAR_OVERLAY_W.min((area.width() - 40.0).max(120.0)));

        let content = self.drawer.resolve(area);
        // The panel's drawn rect (docked column or overlay drawer). Driven by
        // the eased reveal so BOTH opening and collapsing slide; zero once fully
        // hidden so `layout_history` skips it.
        self.sidebar_rect =
            if self.drawer.reveal() > 0.001 && self.drawer.sidebar_rect().width() > 0.5 {
                self.drawer.sidebar_rect()
            } else {
                Rect::ZERO
            };

        if self.drawer.is_docked() {
            // Keypad gets the (eased) content column beside the docked history.
            (content.width() - pad).max(0.0)
        } else {
            // Overlay (or collapsed): keypad gets the whole body width.
            (area.width() - pad * 2.0).max(0.0)
        }
    }

    /// Advance the drawer's eased reveal one frame (draw path only).
    pub fn tick_drawer(&mut self, reduce_motion: bool) {
        self.drawer.tick(reduce_motion);
    }

    /// Whether the history drawer is mid-slide (drives the app's animation
    /// frame pump).
    pub fn drawer_animating(&self) -> bool {
        self.drawer.is_animating()
    }

    /// Recompute all geometry for the given body `area`. Called at the top of
    /// both draw and event handling so hit-tests match the last paint.
    ///
    /// The keys keep at least [`KEY_MIN_H`] for as long as the chrome can give
    /// way: a short window first drops the display's status line and folds
    /// the sidebar toggle into the memory row, then, when wide enough, moves
    /// the display and memory bar beside the pad so the keys get the full
    /// height. Only a window both short and narrow shrinks the keys further.
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        let pad = 16.0;
        let keypad_w = self.resolve_sidebar(area, pad);
        self.ensure_layout(keypad_w);

        // Left column = display + memory bar + keypad. Its right edge is the
        // body minus a docked sidebar (overlay/collapsed → full width).
        let left_x1 = if self.drawer.is_docked() {
            // Docked: the keypad column ends at the (eased) content edge, so the
            // history slide reflows the keypad both opening and collapsing.
            self.drawer.content_rect().x1 - pad
        } else {
            area.x1 - pad
        };
        let left = Rect::new(area.x0 + pad, area.y0 + pad, left_x1, area.y1 - pad);
        let gap = CHROME_GAP;
        let toggle_w = TOGGLE_W;
        let toggle_h = 28.0;

        let tall_grid = left.height() - (toggle_h + 8.0) - DISPLAY_H - MEM_H - 2.0 * gap;
        let side_w = (left.width() * 0.38).clamp(SIDE_CHROME_MIN_W, 320.0);
        let (chrome, grid) = if tall_grid >= self.grid_min_h() {
            self.toggle_rect = Rect::new(left.x1 - toggle_w, left.y0, left.x1, left.y0 + toggle_h);
            let display_top = left.y0 + toggle_h + 8.0;
            let chrome = Rect::new(left.x0, display_top, left.x1, display_top + DISPLAY_H);
            let grid_top = chrome.y1 + gap + MEM_H + gap;
            (chrome, Rect::new(left.x0, grid_top, left.x1, left.y1))
        } else if left.width() - side_w - gap >= SIDE_PAD_MIN_W {
            let chrome = Rect::new(left.x0, left.y0, left.x0 + side_w, left.y1);
            let grid = Rect::new(chrome.x1 + gap, left.y0, left.x1, left.y1);
            self.ensure_layout(grid.width());
            (chrome, grid)
        } else {
            let grid_top = left.y0 + DISPLAY_SHORT_H + gap + MEM_SHORT_H + gap;
            (left, Rect::new(left.x0, grid_top, left.x1, left.y1))
        };
        let tall = grid.y0 > chrome.y1;
        if tall {
            self.display_rect = chrome;
            self.layout_memory(left.x0, chrome.y1 + gap, MEM_BTN_W, MEM_H);
        } else {
            // The display sits above the memory row, which also carries the
            // sidebar toggle at its right end.
            let display_h = if grid.x0 > chrome.x1 {
                (chrome.height() - MEM_SHORT_H - gap).clamp(DISPLAY_SHORT_H, DISPLAY_H)
            } else {
                DISPLAY_SHORT_H
            };
            self.display_rect = Rect::new(chrome.x0, chrome.y0, chrome.x1, chrome.y0 + display_h);
            let mem_top = self.display_rect.y1 + gap;
            let row_w = chrome.width() - toggle_w - gap - 3.0 * MEM_GAP;
            let button_w = (row_w / 4.0).clamp(1.0, MEM_BTN_W);
            self.layout_memory(chrome.x0, mem_top, button_w, MEM_SHORT_H);
            let toggle_y = mem_top + (MEM_SHORT_H - toggle_h) / 2.0;
            self.toggle_rect = Rect::new(
                chrome.x1 - toggle_w,
                toggle_y,
                chrome.x1,
                toggle_y + toggle_h,
            );
        }
        let (expr_top, expr_bottom) = self.expression_band();
        self.expression_rect = Rect::new(
            self.display_rect.x0 + 16.0,
            expr_top,
            self.display_rect.x1 - 16.0,
            expr_bottom,
        );

        // Keypad grid (column count is layout-dependent).
        self.key_rects.clear();
        let cols = self.cols;
        let rows = self.keys.len().div_ceil(cols);
        let cell_gap = 8.0;
        let cw = (grid.width() - cell_gap * (cols as f64 - 1.0)) / cols as f64;
        let ch = ((grid.height() - cell_gap * (rows as f64 - 1.0)) / rows as f64).max(1.0);
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

    /// The height the current pad needs at [`KEY_MIN_H`] per key.
    fn grid_min_h(&self) -> f64 {
        let rows = self.keys.len().div_ceil(self.cols) as f64;
        rows * KEY_MIN_H + (rows - 1.0) * 8.0
    }

    /// The four memory buttons from `x0`, each `width` by `height`.
    fn layout_memory(&mut self, x0: f64, top: f64, width: f64, height: f64) {
        self.mem_rects.clear();
        for i in 0..4 {
            let x = x0 + f64::from(i) * (width + MEM_GAP);
            self.mem_rects
                .push(Rect::new(x, top, x + width, top + height));
        }
    }

    /// Whether the display is too short for its status line.
    fn display_is_short(&self) -> bool {
        self.display_rect.height() < DISPLAY_H
    }

    /// The expression line's top and bottom within the display.
    fn expression_band(&self) -> (f64, f64) {
        let d = self.display_rect;
        if self.display_is_short() {
            (d.y0 + 4.0, d.y0 + 38.0)
        } else {
            (d.y0 + 30.0, d.y0 + 78.0)
        }
    }

    fn layout_history(&mut self, model: &CalcModel) {
        self.history_rects.clear();
        // Lay the rows out whenever the panel has any width (so a sliding open
        // OR close still shows content); clicks are gated on `is_open` in the
        // hit-testers, not here.
        if self.sidebar_rect == Rect::ZERO {
            self.history_view = Rect::ZERO;
            return;
        }
        let right = self.sidebar_rect;
        let header_h = 48.0;
        let row_h = 56.0;
        self.clear_hist_rect = Rect::new(
            right.x1 - 76.0,
            right.y0 + 12.0,
            right.x1 - 16.0,
            right.y0 + 36.0,
        );
        self.history_view = Rect::new(right.x0, right.y0 + header_h, right.x1, right.y1);
        self.history_content_h = model.history.len() as f64 * row_h;
        let max_scroll = (self.history_content_h - self.history_view.height()).max(0.0);
        self.history_scroll = self.history_scroll.clamp(0.0, max_scroll);

        let mut y = self.history_view.y0 - self.history_scroll;
        for (i, _) in model.history.iter().enumerate() {
            let r = Rect::new(
                self.history_view.x0 + 12.0,
                y,
                self.history_view.x1 - 12.0,
                y + row_h,
            );
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
        draw::set_text_scale(draw::paint_scale(tr, theme));
        // Advance the drawer's eased reveal once per frame (draw path only) so
        // the docked column / overlay flap slides instead of snapping.
        self.drawer.tick(theme.reduce_motion);
        self.layout(area, model);

        // ---- Sidebar toggle ------------------------------------------------
        self.draw_toggle(scene, t, tr, theme);

        // ---- Display panel -------------------------------------------------
        self.draw_display(scene, t, tr, theme, model);

        // ---- Memory bar ----------------------------------------------------
        self.draw_memory_bar(scene, t, tr, theme, model);

        // ---- Keypad --------------------------------------------------------
        self.draw_keys(scene, t, tr, theme);

        // ---- History sidebar ----------------------------------------------
        // Overlay mode draws the drawer's dimming scrim over the keypad first,
        // then the drawer floats on top; docked mode just paints the column in
        // place. Both the scrim and its fade are owned by the primitive.
        if self.drawer.is_overlay() {
            self.drawer.draw_scrim(scene, t);
        }
        // Paint whenever the panel has any width (reveal > 0), so the history
        // slides both opening and collapsing rather than blinking out.
        if self.sidebar_rect != Rect::ZERO {
            self.draw_history(scene, t, tr, theme, model);
        }
    }

    /// The display panel: status line (angle mode + Ans), the expression
    /// line, and the result/preview line.
    fn draw_display(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let display = self.display_rect;
        ui::surface::card(ctx.scene, t, display, theme.radius_standard, c.neutral[0]);
        if self.display_is_short() {
            self.draw_display_expr_and_result(ctx.scene, t, ctx.text, theme, model);
            return;
        }

        // status line: angle mode + Ans
        let status = Rect::new(
            display.x0 + 16.0,
            display.y0 + 8.0,
            display.x1 - 16.0,
            display.y0 + 30.0,
        );
        let mode_txt = if model.deg { "DEG" } else { "RAD" };
        draw::text_in(
            &mut ctx,
            status,
            mode_txt,
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            c.accent[6],
            Align::Left,
        );
        let ans_txt = format!("Ans = {}", engine::format_number(model.ans));
        draw::text_in(
            &mut ctx,
            status,
            &ans_txt,
            &TextStyle::new(12.0),
            c.text_dim,
            Align::Right,
        );

        self.draw_display_expr_and_result(ctx.scene, t, ctx.text, theme, model);
    }

    /// The display panel's expression line and its result/preview line.
    fn draw_display_expr_and_result(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let display = self.display_rect;
        let short = self.display_is_short();
        let expr_rect = self.expression_rect;
        let expr_shown = if model.expr.trim().is_empty() {
            "0".to_string()
        } else {
            engine::prettify(&model.expr)
        };
        let expr_color = if model.expr.trim().is_empty() {
            c.text_dim
        } else {
            c.text_primary
        };
        draw::text_in(
            &mut ctx,
            expr_rect,
            &expr_shown,
            &TextStyle::new(if short { 24.0 } else { 30.0 }).with_weight(TextWeight::Medium),
            expr_color,
            Align::Right,
        );

        // result / preview line
        let res_rect = Rect::new(
            display.x0 + 16.0,
            expr_rect.y1,
            display.x1 - 16.0,
            display.y1 - if short { 4.0 } else { 6.0 },
        );
        let (res_txt, res_color, weight) = match engine::evaluate(&model.expr, &model.ctx()) {
            Ok(v) if !model.expr.trim().is_empty() => {
                let col = if model.after_equals {
                    c.accent[5]
                } else {
                    c.text_dim
                };
                (
                    format!("= {}", engine::format_number(v)),
                    col,
                    TextWeight::Bold,
                )
            }
            Err(error) if !model.expr.trim().is_empty() => (
                format!("Error: {error}"),
                c.semantic.danger,
                TextWeight::Medium,
            ),
            _ => (String::new(), c.text_dim, TextWeight::Regular),
        };
        draw::text_in(
            &mut ctx,
            res_rect,
            &res_txt,
            &TextStyle::new(if short { 24.0 } else { 34.0 }).with_weight(weight),
            res_color,
            Align::Right,
        );
    }

    /// The memory-register buttons (MC/MR/M+/M-) and the "M = …" readout.
    fn draw_memory_bar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        for (i, key) in MEM_KEYS.iter().enumerate() {
            let Some(&r) = self.mem_rects.get(i) else {
                continue;
            };
            let hovered = self.hovered_mem == Some(i);
            let active = matches!(key, Key::MemRecall | Key::MemClear) && model.memory != 0.0;
            let fill = if hovered { c.neutral[3] } else { c.neutral[2] };
            ui::surface::card(ctx.scene, t, r, 8.0, fill);
            let txt_col = if active { c.accent[6] } else { c.text_dim };
            draw::text_in(
                &mut ctx,
                r,
                key.label(),
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                txt_col,
                Align::Center,
            );
        }
        if let Some(&mem3) = self.mem_rects.get(3) {
            // Ends before the sidebar toggle when the toggle shares the row.
            let label_x1 = if self.toggle_rect.y0 >= mem3.y0 {
                self.toggle_rect.x0 - 12.0
            } else {
                self.display_rect.x1
            };
            let mem_label = Rect::new(mem3.x1 + 12.0, mem3.y0, label_x1, mem3.y1);
            if mem_label.width() < 48.0 {
                return;
            }
            let mem_txt = format!("M = {}", engine::format_number(model.memory));
            let mem_col = if model.memory == 0.0 {
                c.text_dim
            } else {
                c.accent[6]
            };
            draw::text_in(
                &mut ctx,
                mem_label,
                &mem_txt,
                &TextStyle::new(14.0).with_weight(TextWeight::Medium),
                mem_col,
                Align::Right,
            );
        }
    }

    /// The scientific keypad's key buttons.
    fn draw_keys(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        for (i, key) in self.keys.iter().enumerate() {
            let Some(&r) = self.key_rects.get(i) else {
                continue;
            };
            let hovered = self.hovered_key == Some(i);
            let pressed = self.pressed_key == Some(i);
            let (fill, text_col) = key_colors(theme, key.role(), hovered, pressed);
            ui::surface::card(ctx.scene, t, r, 10.0, fill);
            if key.role() == KeyRole::Accent {
                ctx.stroke_round(r, 10.0, c.accent[6], 1.0);
            }
            let size = match key.role() {
                KeyRole::Digit | KeyRole::Operator | KeyRole::Accent => 22.0_f32,
                _ => 16.0,
            }
            .min(ui::num::f32_of_f64(r.height() * 0.6));
            draw::text_in(
                &mut ctx,
                r,
                key.label(),
                &TextStyle::new(size).with_weight(TextWeight::Medium),
                text_col,
                Align::Center,
            );
        }
    }

    /// Draw the standard sidebar-toggle affordance owned by the primitive.
    fn draw_toggle(&self, scene: &mut Scene, t: Affine, _tr: &mut TextRenderer, theme: &Theme) {
        self.drawer
            .draw_toggle(scene, t, theme, self.toggle_rect, false);
    }

    fn draw_history(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let right = self.sidebar_rect;

        // A docked column blends with the base; a floating drawer reads as a
        // raised surface with its own left edge + shadow-ish border.
        let overlay = self.drawer.is_overlay();
        let panel_bg = if overlay {
            c.surfaces.raised
        } else {
            c.surfaces.base
        };
        ctx.fill_rect(right, panel_bg);
        draw::line(
            ctx.scene,
            t,
            Point::new(right.x0, right.y0),
            Point::new(right.x0, right.y1),
            ui::surface::edge_on(panel_bg),
            1.0,
        );

        let header = Rect::new(right.x0 + 16.0, right.y0, right.x1 - 16.0, right.y0 + 48.0);
        draw::text_in(
            &mut ctx,
            header,
            "History",
            &TextStyle::new(16.0).with_weight(TextWeight::Semibold),
            c.text_primary,
            Align::Left,
        );
        ui::surface::card(ctx.scene, t, self.clear_hist_rect, 6.0, c.neutral[2]);
        draw::text_in(
            &mut ctx,
            self.clear_hist_rect,
            "Clear",
            &TextStyle::new(12.0).with_weight(TextWeight::Medium),
            c.text_dim,
            Align::Center,
        );

        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &self.history_view,
        );
        if model.history.is_empty() {
            draw::text_in(
                &mut ctx,
                self.history_view,
                "No calculations yet",
                &TextStyle::new(14.0),
                c.text_dim,
                Align::Center,
            );
        }
        for (i, r) in &self.history_rects {
            let Some(entry) = model.history.get(*i) else {
                continue;
            };
            draw_recorded_row(
                ctx.scene,
                t,
                ctx.text,
                theme,
                Rect::new(r.x0, r.y0 + 4.0, r.x1, r.y1 - 4.0),
                entry,
            );
        }
        ctx.scene.pop_layer();
    }

    // -- hit-testing ---------------------------------------------------------

    pub fn key_at(&self, p: Point) -> Option<usize> {
        // An open overlay drawer captures clicks over it (so a key behind the
        // scrim isn't pressed through the drawer).
        if self.drawer.is_overlay() && self.drawer.sidebar_contains(p) {
            return None;
        }
        self.key_rects.iter().position(|r| r.contains(p))
    }
    pub fn mem_at(&self, p: Point) -> Option<usize> {
        if self.drawer.is_overlay() && self.drawer.sidebar_contains(p) {
            return None;
        }
        self.mem_rects.iter().position(|r| r.contains(p))
    }
    /// History index under `p`, if the click hit a row inside the viewport.
    pub fn history_at(&self, p: Point) -> Option<usize> {
        if !self.drawer.is_open() || !self.history_view.contains(p) {
            return None;
        }
        self.history_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }
    pub fn clear_history_at(&self, p: Point) -> bool {
        self.drawer.is_open() && self.clear_hist_rect.contains(p)
    }
    pub fn history_viewport(&self) -> Rect {
        if self.drawer.is_open() {
            self.history_view
        } else {
            Rect::ZERO
        }
    }

    /// Whether `p` hit the standard sidebar-toggle affordance.
    pub fn toggle_at(&self, p: Point) -> bool {
        self.toggle_rect.contains(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open overlay drawer.
    pub fn scrim_at(&self, p: Point) -> bool {
        self.drawer.scrim_contains(p)
    }
    /// Flip the sidebar open/closed. The primitive remembers the explicit
    /// choice so the breakpoint default no longer overrides it.
    pub fn toggle_sidebar(&mut self) {
        self.drawer.toggle();
    }
    /// Force the overlay drawer shut (e.g. a scrim tap).
    pub fn close_sidebar(&mut self) {
        self.drawer.close();
    }
    /// Snap the drawer open/closed with no animation (deterministic `--shots`).
    pub fn snap_sidebar(&mut self, open: bool) {
        self.drawer.snap(open);
    }
    pub fn sidebar_open(&self) -> bool {
        self.drawer.is_open()
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
        KeyRole::Accent => (
            c.accent[5],
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
        ),
        KeyRole::Warm => (
            draw::mix(c.neutral[2], c.semantic.danger, 0.28),
            c.text_primary,
        ),
    };
    let fill = if pressed {
        draw::wash(base, 0.18)
    } else if hovered {
        draw::wash(base, 0.10)
    } else {
        base
    };
    (fill, text)
}

/// Shared presentation of a recorded calculation, without keypad/actions.
pub(crate) fn draw_recorded_row(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    entry: &crate::model::HistoryEntry,
) {
    let mut ctx = PaintCtx::new(scene, t, tr, theme);
    let c = &theme.tokens.colors;
    ui::surface::card(ctx.scene, t, row, 8.0, c.neutral[1]);
    let expr_r = Rect::new(row.x0 + 12.0, row.y0 + 6.0, row.x1 - 12.0, row.y0 + 26.0);
    draw::text_in(
        &mut ctx,
        expr_r,
        &engine::prettify(&entry.expr),
        &TextStyle::new(13.0),
        c.text_dim,
        Align::Left,
    );
    let res_r = Rect::new(row.x0 + 12.0, row.y0 + 24.0, row.x1 - 12.0, row.y1 - 4.0);
    draw::text_in(
        &mut ctx,
        res_r,
        &format!("= {}", entry.result),
        &TextStyle::new(18.0).with_weight(TextWeight::Semibold),
        c.text_primary,
        Align::Right,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CalcModel;

    fn body(w: f64) -> Rect {
        Rect::new(0.0, 60.0, w, 860.0)
    }

    /// The history sidebar docks open on the right when wide, collapses by default
    /// when narrow and opens there as an overlay drawer with a dismiss scrim that
    /// captures clicks over it, and an explicit user hide survives relayout when
    /// wide. Fresh screen per phase.
    #[test]
    fn history_sidebar_docks_wide_overlays_narrow_and_honours_user_choice() {
        // phase wide_window_docks_sidebar_open_by_default
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            s.layout(body(1200.0), &m);
            assert!(
                s.sidebar_open(),
                "wide window should default to history shown"
            );
            assert!(s.drawer.is_docked(), "wide window should dock, not overlay");
            assert!(s.sidebar_rect.width() > 0.0);
            // Docked on the RIGHT: the history column hugs the right edge.
            assert!(
                (s.sidebar_rect.x1 - body(1200.0).x1).abs() < 0.5,
                "history docks on the right edge"
            );
        }

        // phase narrow_window_collapses_sidebar_by_default
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            s.layout(body(520.0), &m);
            assert!(
                !s.sidebar_open(),
                "narrow window must NOT force the sidebar open"
            );
            assert!(
                s.drawer.is_overlay(),
                "narrow window uses the overlay drawer mode"
            );
            assert_eq!(
                s.history_viewport(),
                Rect::ZERO,
                "collapsed → no history viewport"
            );
        }

        // phase narrow_window_opens_as_overlay_with_scrim
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            s.layout(body(520.0), &m);
            // user taps the toggle to open the drawer (snap to settle deterministically)
            s.toggle_sidebar();
            s.snap_sidebar(true);
            s.layout(body(520.0), &m);
            assert!(s.sidebar_open());
            assert!(s.drawer.is_overlay(), "still an overlay at this width");
            assert!(
                s.drawer.scrim_rect().width() > 0.0,
                "overlay must lay down a dismiss scrim"
            );
            // a scrim tap (left of the right-anchored drawer) dismisses it
            let scrim_pt = Point::new(body(520.0).x0 + 10.0, s.drawer.scrim_rect().center().y);
            assert!(s.scrim_at(scrim_pt));
            s.close_sidebar();
            s.layout(body(520.0), &m);
            assert!(!s.sidebar_open());
        }

        // phase overlay_drawer_captures_clicks_over_it
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            s.layout(body(520.0), &m);
            s.toggle_sidebar();
            s.snap_sidebar(true);
            s.layout(body(520.0), &m);
            // a point inside the drawer is not a keypad/mem press
            let over = Point::new(s.sidebar_rect.center().x, s.sidebar_rect.center().y);
            assert_eq!(
                s.key_at(over),
                None,
                "drawer must not press the key behind it"
            );
            assert_eq!(s.mem_at(over), None);
        }

        // phase user_choice_overrides_breakpoint_default_when_wide
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            s.layout(body(1200.0), &m);
            assert!(s.sidebar_open());
            // user explicitly hides it; that choice sticks across redraws
            s.toggle_sidebar();
            s.layout(body(1200.0), &m);
            assert!(!s.sidebar_open(), "explicit hide must survive a relayout");
        }
    }

    #[test]
    fn short_windows_keep_every_control_apart_and_inside_the_body() {
        let overlaps = |a: Rect, b: Rect| a.intersect(b).area() > 0.01;
        let sizes = [
            // (width, height, keys must keep KEY_MIN_H)
            (1200.0, 800.0, true),
            (440.0, 800.0, true),
            // Landscape phone, history docked beside the pad.
            (914.0, 270.0, true),
            // A 420 dpi phone in landscape, less its navigation bar.
            (825.0, 274.0, true),
            (600.0, 280.0, true),
            // Split screen: short and narrow.
            (360.0, 300.0, false),
        ];
        for (w, h, roomy) in sizes {
            let area = Rect::new(0.0, 60.0, w, 60.0 + h);
            let mut s = CalcScreen::default();
            s.layout(area, &CalcModel::seeded());
            let context = format!("{w}x{h}");
            let chrome = [s.display_rect, s.toggle_rect]
                .into_iter()
                .chain(s.mem_rects.iter().copied())
                .collect::<Vec<_>>();
            for (i, a) in chrome.iter().enumerate() {
                assert!(
                    area.contains_rect(*a),
                    "{context}: chrome {a:?} leaves the body"
                );
                for b in &chrome[i + 1..] {
                    assert!(!overlaps(*a, *b), "{context}: chrome {a:?} overlaps {b:?}");
                }
            }
            for (i, key) in s.key_rects.iter().enumerate() {
                assert!(
                    area.contains_rect(*key),
                    "{context}: key {i} {key:?} leaves the body"
                );
                if roomy {
                    assert!(key.height() >= KEY_MIN_H, "{context}: key {i} is {key:?}");
                }
                for c in &chrome {
                    assert!(
                        !overlaps(*key, *c),
                        "{context}: key {i} {key:?} overlaps {c:?}"
                    );
                }
                for (j, other) in s.key_rects.iter().enumerate().skip(i + 1) {
                    assert!(
                        !overlaps(*key, *other),
                        "{context}: keys {i} and {j} overlap"
                    );
                }
            }
            assert!(
                s.display_rect.contains_rect(s.expression_rect),
                "{context}: the expression line leaves the display"
            );
        }
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
