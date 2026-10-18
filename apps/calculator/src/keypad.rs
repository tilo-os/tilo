//! The Calculator mode: a live display, a memory bar, a scientific keypad,
//! and a collapsible history panel. This is a plain renderer driven by the
//! [`CalcModel`]; it owns only layout + hover + panel caches, never state.
//!
//! Two responsive behaviours live here:
//!  * the keypad picks a [`KeypadLayout`] (compact / standard / scientific)
//!    from its available width, so the pad stays usable from a narrow strip
//!    up to a wide window; and
//!  * the history panel is the toolkit's trailing side panel: it docks beside
//!    the keypad when the window is wide, and collapses to a button that
//!    opens a floating drawer (over a dismiss scrim) when it is not.

use ui::chrome::{SelectionState, paint_row_selection};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::chrome::SidePanel;
use crate::draw::{self, Align};
use crate::engine;
use crate::model::{CalcModel, HistoryEntry, Key, KeyRole, KeypadLayout};

/// Memory-bar buttons, left → right.
pub const MEM_KEYS: [Key; 4] = [Key::MemClear, Key::MemRecall, Key::MemAdd, Key::MemSub];

/// What the pointer is over, for deciding when to repaint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hover {
    pub key: Option<usize>,
    pub mem: Option<usize>,
    pub history: Option<usize>,
    pub clear: bool,
    pub toggle: bool,
}

/// The theme's measurements the screen lays out with, cached from the last
/// draw so the event path (which has no theme) places everything the same way.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    /// Inset of the keypad column from the panel and the window.
    pad: f64,
    /// Between the display, the memory row and the keypad.
    gap: f64,
    /// Between neighbouring keys and memory buttons.
    cell_gap: f64,
    /// Inset of the display's text from its edge.
    display_pad: f64,
    /// The display with its status line, and without.
    display_tall: f64,
    display_short: f64,
    /// Line boxes the display's lines are set on.
    status_line: f64,
    expr_tall_line: f64,
    result_tall_line: f64,
    short_line: f64,
    /// The memory row's button height.
    mem_h: f64,
    /// The shortest a key gets before the chrome around the pad gives way.
    key_min_h: f64,
    /// The narrowest a key column gets beside the display.
    key_min_w: f64,
    /// One history row.
    row_h: f64,
    /// The list's inset from the panel's edges.
    list_inset: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let s = &theme.tokens.spacing;
        let line = |role: TextRole| f64::from(theme.text(role).line_height_px());
        let pad_y = f64::from(s.s3);
        let status_line = line(TextRole::Caption);
        let expr_tall_line = line(TextRole::LargeTitle);
        let result_tall_line = line(TextRole::Display);
        let short_line = line(TextRole::Title);
        Self {
            pad: f64::from(s.s4),
            gap: f64::from(s.s3),
            cell_gap: f64::from(s.s2),
            display_pad: f64::from(s.s4),
            display_tall: 2.0 * pad_y
                + status_line
                + expr_tall_line
                + result_tall_line
                + f64::from(s.s1),
            display_short: 2.0 * f64::from(s.s2) + 2.0 * short_line,
            status_line,
            expr_tall_line,
            result_tall_line,
            short_line,
            mem_h: ButtonFace::height(ControlSize::Small, theme),
            key_min_h: f64::from(theme.tokens.metrics.min_pointer_target),
            key_min_w: f64::from(s.s12),
            row_h: line(TextRole::Caption) + line(TextRole::Title) + 2.0 * f64::from(s.s2),
            list_inset: f64::from(s.s2),
        }
    }
}

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
    /// Expression line used to anchor the platform IME candidate window.
    expression_rect: Rect,
    display_rect: Rect,
    /// The memory register's readout, right of the memory buttons.
    memory_rect: Rect,
    pub hovered_key: Option<usize>,
    pub hovered_mem: Option<usize>,
    pub pressed_key: Option<usize>,
    pub history_scroll: f64,
    history_view: Rect,
    history_content_h: f64,
    hover: Hover,
    /// The trailing history panel: the shared drawer, docked or floating.
    panel: SidePanel,
    m: Metrics,
    /// The memory buttons' width and the clear button's, measured at the
    /// size they paint at.
    mem_w: f64,
    clear_w: f64,
}

impl Default for CalcScreen {
    fn default() -> Self {
        let layout_kind = KeypadLayout::Scientific;
        let stock = Theme::dark();
        Self {
            keys: layout_kind.keys(),
            layout_kind,
            cols: layout_kind.cols(),
            key_rects: Vec::new(),
            mem_rects: Vec::new(),
            history_rects: Vec::new(),
            clear_hist_rect: Rect::ZERO,
            expression_rect: Rect::ZERO,
            display_rect: Rect::ZERO,
            memory_rect: Rect::ZERO,
            hovered_key: None,
            hovered_mem: None,
            pressed_key: None,
            history_scroll: 0.0,
            history_view: Rect::ZERO,
            history_content_h: 0.0,
            hover: Hover::default(),
            panel: SidePanel::new(false, f64::from(ui::tokens::Metrics::BASE.inspector_width)),
            m: Metrics::of(&stock),
            mem_w: f64::from(ui::tokens::Metrics::BASE.min_pointer_target) + 12.0,
            clear_w: 0.0,
        }
    }
}

impl CalcScreen {
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// Screen-space center of keypad key `i`, once the pad has been laid out.
    #[cfg(test)]
    pub(crate) fn key_center(&self, i: usize) -> Option<Point> {
        self.key_rects.get(i).map(ui::kurbo::Rect::center)
    }
    /// Key geometry for the app's custom-canvas accessibility tree.
    pub fn key_rect(&self, i: usize) -> Option<Rect> {
        self.key_rects.get(i).copied()
    }

    /// Index of the first keypad key whose label is `label`.
    #[cfg(test)]
    pub(crate) fn key_index_for_label(&self, label: &str) -> Option<usize> {
        self.keys.iter().position(|k| k.label() == label)
    }

    /// Screen-space center of memory-bar button `i`, once laid out.
    #[cfg(test)]
    pub(crate) fn mem_center(&self, i: usize) -> Option<Point> {
        self.mem_rects.get(i).map(ui::kurbo::Rect::center)
    }
    /// Memory-button geometry for the app's custom-canvas accessibility tree.
    pub fn mem_rect(&self, i: usize) -> Option<Rect> {
        self.mem_rects.get(i).copied()
    }

    /// Screen-space center of the history row for entry `i`, if it
    /// is currently laid out (panel shown and the row is in the viewport).
    #[cfg(test)]
    pub(crate) fn history_row_center(&self, i: usize) -> Option<Point> {
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

    /// Screen-space center of the "Clear history" button, if the
    /// panel is currently shown.
    #[cfg(test)]
    pub(crate) fn clear_history_center(&self) -> Option<Point> {
        (self.panel.is_open() && self.clear_hist_rect != Rect::ZERO)
            .then(|| self.clear_hist_rect.center())
    }
    pub fn clear_history_rect(&self) -> Rect {
        self.clear_hist_rect
    }
    pub fn toggle_rect(&self) -> Rect {
        self.panel.toggle_rect()
    }

    /// Keep `reserve` of the window's trailing corner free for its History
    /// control.
    pub fn set_trailing_reserve(&mut self, reserve: f64) {
        self.panel.set_trailing_reserve(reserve);
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

    /// The column beside the history panel, header row included.
    pub fn content_column(&self) -> Rect {
        self.panel.content()
    }

    /// Pick the keypad layout for `keypad_w` and rebuild the key list if the
    /// breakpoint changed. Cheap when unchanged (the common case).
    fn ensure_layout(&mut self, keypad_w: f64) {
        self.adopt_layout(KeypadLayout::for_width(keypad_w));
    }

    /// Pick the widest layout for a `keypad_w` × `keypad_h` pad whose rows
    /// still keep `key_min_h`: a short pad trades function keys for key
    /// height before its keys shrink. The compact pad is the floor.
    fn ensure_layout_within(&mut self, keypad_w: f64, keypad_h: f64) {
        self.ensure_layout(keypad_w);
        for narrower in [KeypadLayout::Standard, KeypadLayout::Compact] {
            if self.grid_min_h() <= keypad_h {
                break;
            }
            if narrower.cols() < self.cols {
                self.adopt_layout(narrower);
            }
        }
    }

    /// Switch to `want`, rebuilding the key list only when it changes.
    fn adopt_layout(&mut self, want: KeypadLayout) {
        if want != self.layout_kind {
            self.layout_kind = want;
            self.cols = want.cols();
            self.keys = want.keys();
            self.pressed_key = None;
            self.hovered_key = None;
        }
    }

    /// Adopt the theme's scale and metrics.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.panel.scale_for(theme);
        self.m = Metrics::of(theme);
    }

    /// Measure the buttons at the size they paint at, advance the panel's
    /// eased reveal one frame and lay the screen out. Draw path only.
    pub fn prepare(&mut self, tr: &mut TextRenderer, theme: &Theme, model: &CalcModel, area: Rect) {
        self.scale_for(theme);
        self.mem_w = MEM_KEYS
            .iter()
            .map(|key| {
                ButtonFace::new(Rect::ZERO, key.label())
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Plain)
                    .intrinsic_width(tr, theme)
            })
            .fold(0.0, f64::max);
        self.clear_w = ButtonFace::new(Rect::ZERO, CLEAR_HISTORY)
            .control_size(ControlSize::Small)
            .role(ButtonRole::Destructive)
            .intrinsic_width(tr, theme);
        self.panel.tick(theme);
        self.layout(area, model);
    }

    /// Whether the history panel is mid-slide (drives the app's animation
    /// frame pump).
    pub fn drawer_animating(&self) -> bool {
        self.panel.is_animating()
    }

    /// Recompute all geometry for the window `area` (the header row
    /// included). Called at the top of both draw and event handling so
    /// hit-tests match the last paint.
    ///
    /// The keys keep at least `key_min_h` for as long as the chrome can give
    /// way: a short window first drops the display's status line, then, when
    /// wide enough, moves the display and memory bar beside the pad so the
    /// keys get the full height. Only a window both short and narrow shrinks
    /// the keys further.
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        self.panel.layout(area);
        let m = self.m;
        let content = self.panel.content();
        let header_h = self.panel.header_height();
        let body = Rect::new(
            content.x0,
            (content.y0 + header_h).min(content.y1),
            content.x1,
            content.y1,
        );
        let left = Rect::new(
            body.x0 + m.pad,
            body.y0 + m.pad,
            (body.x1 - m.pad).max(body.x0 + m.pad),
            (body.y1 - m.pad).max(body.y0 + m.pad),
        );
        self.ensure_layout(left.width());

        let side_w = (left.width() * 0.38).clamp(
            4.0 * self.mem_w + 3.0 * m.cell_gap,
            f64::from(ui::tokens::Metrics::BASE.panel_width_utility),
        );
        let side_pad_min_w = 4.0 * m.key_min_w + 3.0 * m.cell_gap;
        let tall_grid = left.height() - m.display_tall - m.mem_h - 2.0 * m.gap;
        let (chrome, grid, display_h) = if tall_grid >= self.grid_min_h() {
            let grid_top = left.y0 + m.display_tall + m.gap + m.mem_h + m.gap;
            (
                left,
                Rect::new(left.x0, grid_top, left.x1, left.y1),
                m.display_tall,
            )
        } else if left.width() - side_w - m.gap >= side_pad_min_w {
            // Beside the pad: the display takes what the memory row leaves.
            let chrome = Rect::new(left.x0, left.y0, left.x0 + side_w, left.y1);
            let grid = Rect::new(chrome.x1 + m.gap, left.y0, left.x1, left.y1);
            self.ensure_layout_within(grid.width(), grid.height());
            let height = (chrome.height() - m.mem_h - m.gap).clamp(m.display_short, m.display_tall);
            (chrome, grid, height)
        } else {
            let grid_top = left.y0 + m.display_short + m.gap + m.mem_h + m.gap;
            (
                left,
                Rect::new(left.x0, grid_top, left.x1, left.y1),
                m.display_short,
            )
        };
        self.display_rect = Rect::new(chrome.x0, chrome.y0, chrome.x1, chrome.y0 + display_h);
        let mem_top = self.display_rect.y1 + m.gap;
        let row_w = chrome.width() - 3.0 * m.cell_gap;
        let button_w = (row_w / 4.0).clamp(1.0, self.mem_w);
        self.layout_memory(chrome.x0, mem_top, button_w, m.mem_h);
        let mem_end = self.mem_rects.last().map_or(chrome.x0, |r| r.x1);
        self.memory_rect = Rect::new(mem_end + m.gap, mem_top, chrome.x1, mem_top + m.mem_h);

        let (expr_top, expr_bottom) = self.expression_band();
        self.expression_rect = Rect::new(
            self.display_rect.x0 + m.display_pad,
            expr_top,
            self.display_rect.x1 - m.display_pad,
            expr_bottom,
        );

        // Keypad grid (column count is layout-dependent).
        self.key_rects.clear();
        let cols = self.cols;
        let rows = self.keys.len().div_ceil(cols);
        let cw = (grid.width() - m.cell_gap * (cols as f64 - 1.0)) / cols as f64;
        let ch = ((grid.height() - m.cell_gap * (rows as f64 - 1.0)) / rows as f64).max(1.0);
        for idx in 0..self.keys.len() {
            let r = idx / cols;
            let c = idx % cols;
            let x0 = grid.x0 + c as f64 * (cw + m.cell_gap);
            let y0 = grid.y0 + r as f64 * (ch + m.cell_gap);
            self.key_rects.push(Rect::new(x0, y0, x0 + cw, y0 + ch));
        }

        // History list geometry (only meaningful when the panel is shown).
        self.layout_history(model);
    }

    /// The height the current pad needs at `key_min_h` per key.
    fn grid_min_h(&self) -> f64 {
        let rows = self.keys.len().div_ceil(self.cols) as f64;
        rows * self.m.key_min_h + (rows - 1.0) * self.m.cell_gap
    }

    /// The four memory buttons from `x0`, each `width` by `height`.
    fn layout_memory(&mut self, x0: f64, top: f64, width: f64, height: f64) {
        self.mem_rects.clear();
        for i in 0..4 {
            let x = x0 + f64::from(i) * (width + self.m.cell_gap);
            self.mem_rects
                .push(Rect::new(x, top, x + width, top + height));
        }
    }

    /// Whether the display is too short for its status line.
    fn display_is_short(&self) -> bool {
        self.display_rect.height() < self.m.display_tall - 0.5
    }

    /// The expression line's top and bottom within the display.
    fn expression_band(&self) -> (f64, f64) {
        let d = self.display_rect;
        let m = &self.m;
        if self.display_is_short() {
            let top = d.y0 + (d.height() - 2.0 * m.short_line) / 2.0;
            (top, top + m.short_line)
        } else {
            let pad = (d.height() - m.status_line - m.expr_tall_line - m.result_tall_line) / 2.0;
            let top = d.y0 + pad + m.status_line;
            (top, top + m.expr_tall_line)
        }
    }

    fn layout_history(&mut self, model: &CalcModel) {
        self.history_rects.clear();
        self.clear_hist_rect = Rect::ZERO;
        // Lay the rows out whenever the panel has any width (so a sliding open
        // OR close still shows content); clicks are gated on `is_open` in the
        // hit-testers, not here.
        let body = self.panel.body();
        if body == Rect::ZERO {
            self.history_view = Rect::ZERO;
            return;
        }
        let m = self.m;
        if !model.history.is_empty() {
            let toggle = self.panel.toggle_rect();
            let header = self.panel.header_band();
            let small = m.mem_h;
            let x1 = toggle.x0 - m.cell_gap;
            self.clear_hist_rect = Rect::new(
                (x1 - self.clear_w).max(header.x0),
                header.center().y - small / 2.0,
                x1,
                header.center().y + small / 2.0,
            );
        }
        self.history_view = body;
        self.history_content_h = model.history.len() as f64 * m.row_h;
        let max_scroll = (self.history_content_h - self.history_view.height()).max(0.0);
        self.history_scroll = self.history_scroll.clamp(0.0, max_scroll);

        let mut y = self.history_view.y0 - self.history_scroll;
        for (i, _) in model.history.iter().enumerate() {
            let r = Rect::new(
                self.history_view.x0 + m.list_inset,
                y,
                self.history_view.x1 - m.list_inset,
                y + m.row_h,
            );
            if r.y1 > self.history_view.y0 && r.y0 < self.history_view.y1 {
                self.history_rects.push((i, r));
            }
            y += m.row_h;
        }
    }

    /// The display, memory bar and keypad.
    pub fn draw_content(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        self.draw_display(scene, t, tr, theme, model);
        self.draw_memory_bar(scene, t, tr, theme, model);
        self.draw_keys(scene, t, tr, theme);
    }

    /// The history panel, over the keypad:
    /// the panel, its rows and its show/hide button.
    pub fn draw_panel(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        if self.panel.column() == Rect::ZERO {
            self.panel
                .paint_toggle(scene, t, tr, theme, self.hover.toggle);
            return;
        }
        let local = self.panel.paint_panel(scene, t, theme);
        if !self.panel.is_overlay() {
            ui::widget::toolbar::paint_band(scene, t, &local, self.panel.header_band());
        }
        self.draw_history(scene, t, tr, &local, model);
        let ground = if self.panel.toggle_on_content() {
            theme
        } else {
            &local
        };
        self.panel
            .paint_toggle(scene, t, tr, ground, self.hover.toggle);
    }

    /// The display panel: status line (Ans), the expression line, and the
    /// result/preview line.
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
        ui::surface::card(
            ctx.scene,
            t,
            display,
            f64::from(theme.tokens.radii.control),
            c.surfaces.raised,
        );
        let m = &self.m;
        let short = self.display_is_short();
        let pad = m.display_pad;
        let (expr_top, expr_bottom) = self.expression_band();
        let expr_rect = Rect::new(display.x0 + pad, expr_top, display.x1 - pad, expr_bottom);

        if !short {
            // status line: the previous answer
            let status = Rect::new(
                display.x0 + pad,
                expr_top - m.status_line,
                display.x1 - pad,
                expr_top,
            );
            let ans = format!("Ans = {}", engine::format_number(model.ans));
            draw::text_in(
                &mut ctx,
                status,
                &ans,
                &theme.text(TextRole::Caption).numeric(),
                c.text_dim,
                Align::Right,
            );
        }

        let expr_role = if short {
            TextRole::Title
        } else {
            TextRole::LargeTitle
        };
        let (expr_shown, expr_ink) = if model.expr.trim().is_empty() {
            ("0".to_string(), c.text_tertiary)
        } else {
            (engine::prettify(&model.expr), c.text_primary)
        };
        draw::text_in(
            &mut ctx,
            expr_rect,
            &expr_shown,
            &theme.text(expr_role).numeric(),
            expr_ink,
            Align::Right,
        );

        // result / preview line
        let res_rect = Rect::new(
            display.x0 + pad,
            expr_rect.y1,
            display.x1 - pad,
            expr_rect.y1
                + if short {
                    m.short_line
                } else {
                    m.result_tall_line
                },
        );
        let result_role = if short {
            TextRole::Title
        } else {
            TextRole::Display
        };
        let (res_txt, res_style, res_ink) = match engine::evaluate(&model.expr, &model.ctx()) {
            Ok(v) if !model.expr.trim().is_empty() => {
                let ink = if model.after_equals {
                    c.text_primary
                } else {
                    c.text_dim
                };
                (
                    format!("= {}", engine::format_number(v)),
                    theme.text(result_role).numeric(),
                    ink,
                )
            }
            Err(error) if !model.expr.trim().is_empty() => (
                format!("Error: {error}"),
                theme.text(TextRole::Body),
                c.semantic.danger,
            ),
            _ => (String::new(), theme.text(result_role), c.text_dim),
        };
        draw::text_in(
            &mut ctx,
            res_rect,
            &res_txt,
            &res_style,
            res_ink,
            Align::Right,
        );
    }

    /// The memory-register buttons (MC/MR/M+/M−) and the memory readout.
    fn draw_memory_bar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        for (i, key) in MEM_KEYS.iter().enumerate() {
            let Some(&r) = self.mem_rects.get(i) else {
                continue;
            };
            let face = ButtonFace::new(r, key.label())
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain)
                .hovered(self.hovered_mem == Some(i));
            Button::paint(scene, t, tr, theme, &face);
        }
        if model.memory != 0.0 && self.memory_rect.width() >= 48.0 {
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            let readout = format!("Memory {}", engine::format_number(model.memory));
            draw::text_in(
                &mut ctx,
                self.memory_rect,
                &readout,
                &theme.text(TextRole::Label).numeric(),
                theme.tokens.colors.text_dim,
                Align::Right,
            );
        }
    }

    /// The scientific keypad's key buttons.
    fn draw_keys(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        for (i, key) in self.keys.iter().enumerate() {
            let Some(&r) = self.key_rects.get(i) else {
                continue;
            };
            let role = key.role();
            let face = ButtonFace::new(r, "")
                .control_size(ControlSize::Large)
                .role(key_button_role(role))
                .hovered(self.hovered_key == Some(i))
                .pressed(self.pressed_key == Some(i));
            let face = match key.icon() {
                Some(icon) => face.icon(icon),
                None => face,
            };
            let ink = face.colors(theme).2;
            Button::paint(scene, t, tr, theme, &face);
            if key.icon().is_some() {
                continue;
            }
            let label_role = match role {
                KeyRole::Function => TextRole::Body,
                _ => TextRole::Title,
            };
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            draw::text_in(
                &mut ctx,
                r,
                key.label(),
                &theme.text(label_role).numeric(),
                ink,
                Align::Center,
            );
        }
    }

    fn draw_history(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        local: &Theme,
        model: &CalcModel,
    ) {
        let header = self.panel.header_band();
        {
            let mut ctx = PaintCtx::new(scene, t, tr, local);
            let inset = f64::from(local.tokens.spacing.s3);
            let x1 = if self.clear_hist_rect == Rect::ZERO {
                self.panel.toggle_rect().x0
            } else {
                self.clear_hist_rect.x0
            };
            draw::text_in(
                &mut ctx,
                Rect::new(header.x0 + inset, header.y0, x1 - inset, header.y1),
                "Calculations",
                &local.text(TextRole::Heading),
                local.tokens.colors.text_primary,
                Align::Left,
            );
        }
        if self.clear_hist_rect != Rect::ZERO {
            let face = ButtonFace::new(self.clear_hist_rect, CLEAR_HISTORY)
                .control_size(ControlSize::Small)
                .role(ButtonRole::Destructive)
                .hovered(self.hover.clear);
            Button::paint(scene, t, tr, local, &face);
        }

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &self.history_view,
        );
        let mut ctx = PaintCtx::new(scene, t, tr, local);
        if model.history.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::History),
                ..EmptyStateContent::new("No calculations yet")
            };
            EmptyState::paint(
                &mut ctx,
                self.history_view,
                &content,
                Point::new(-1.0, -1.0),
            );
        }
        let ground = local.tokens.colors.surfaces.content;
        for (n, (i, r)) in self.history_rects.iter().enumerate() {
            let Some(entry) = model.history.get(*i) else {
                continue;
            };
            let hovered = self.hover.history == Some(*i);
            paint_row_selection(
                ctx.scene,
                t,
                *r,
                SelectionState::new(false, hovered, false),
                ground,
                local,
            );
            draw_recorded_row(&mut ctx, *r, entry);
            let next_hovered = self
                .history_rects
                .get(n + 1)
                .is_some_and(|(j, _)| self.hover.history == Some(*j));
            if n + 1 < self.history_rects.len() && !hovered && !next_hovered {
                let inset = f64::from(local.tokens.spacing.s3);
                ui::surface::separator(ctx.scene, t, r.x0, r.x1, r.y1, inset, ground);
            }
        }
        scene.pop_layer();
    }

    // -- hit-testing ---------------------------------------------------------

    pub fn key_at(&self, p: Point) -> Option<usize> {
        // An open floating drawer captures clicks over it (so a key behind
        // the scrim isn't pressed through the drawer).
        if self.panel.covers(p) {
            return None;
        }
        self.key_rects.iter().position(|r| r.contains(p))
    }
    pub fn mem_at(&self, p: Point) -> Option<usize> {
        if self.panel.covers(p) {
            return None;
        }
        self.mem_rects.iter().position(|r| r.contains(p))
    }
    /// History index under `p`, if the click hit a row inside the viewport.
    pub fn history_at(&self, p: Point) -> Option<usize> {
        if !self.panel.is_open() || !self.history_view.contains(p) {
            return None;
        }
        self.history_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }
    pub fn clear_history_at(&self, p: Point) -> bool {
        self.panel.is_open()
            && self.clear_hist_rect != Rect::ZERO
            && self.clear_hist_rect.contains(p)
    }
    pub fn history_viewport(&self) -> Rect {
        if self.panel.is_open() {
            self.history_view
        } else {
            Rect::ZERO
        }
    }

    /// Track the pointer. Returns whether anything the screen paints changed.
    pub fn set_pointer(&mut self, p: Point) -> bool {
        self.hovered_key = self.key_at(p);
        self.hovered_mem = self.mem_at(p);
        let next = Hover {
            key: self.hovered_key,
            mem: self.hovered_mem,
            history: self.history_at(p),
            clear: self.clear_history_at(p),
            toggle: self.panel.toggle_at(p),
        };
        let changed = next != self.hover;
        self.hover = next;
        changed
    }

    /// Whether `p` hit the history panel's show/hide button.
    pub fn toggle_at(&self, p: Point) -> bool {
        self.panel.toggle_at(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open floating drawer.
    pub fn scrim_at(&self, p: Point) -> bool {
        self.panel.scrim_at(p)
    }
    /// Flip the panel open/closed. The drawer remembers the explicit choice
    /// so the breakpoint default no longer overrides it.
    pub fn toggle_sidebar(&mut self) {
        self.panel.toggle();
    }
    /// Force a floating drawer shut (e.g. a scrim tap).
    pub fn close_sidebar(&mut self) {
        self.panel.close();
    }
    /// Snap the panel open/closed with no animation (deterministic captures).
    pub fn snap_sidebar(&mut self, open: bool) {
        self.panel.snap(open);
    }
    pub fn sidebar_open(&self) -> bool {
        self.panel.is_open()
    }
    /// Whether the panel floats over the keypad rather than docking.
    pub fn is_overlay(&self) -> bool {
        self.panel.is_overlay()
    }
}

/// The text of the history panel's destructive action.
const CLEAR_HISTORY: &str = "Clear history";

/// The control role of a keycap: the entry pad (digits and operators) is a
/// key, the scientific functions are quiet, `=` is the one primary action
/// and `AC` discards the expression.
fn key_button_role(role: KeyRole) -> ButtonRole {
    match role {
        KeyRole::Digit | KeyRole::Operator => ButtonRole::Key,
        KeyRole::Function => ButtonRole::Plain,
        KeyRole::Equals => ButtonRole::Primary,
        KeyRole::Clear => ButtonRole::Destructive,
    }
}

/// Shared presentation of a recorded calculation, without keypad/actions: the
/// expression as a quiet caption, its result below it in tabular figures,
/// right-aligned.
pub(crate) fn draw_recorded_row(ctx: &mut PaintCtx<'_>, row: Rect, entry: &HistoryEntry) {
    let theme = ctx.theme;
    let c = &theme.tokens.colors;
    let pad = f64::from(theme.tokens.spacing.s3);
    let pad_y = f64::from(theme.tokens.spacing.s2);
    let caption: TextStyle = theme.text(TextRole::Caption);
    let title = theme.text(TextRole::Title).numeric();
    let caption_h = f64::from(caption.line_height_px());
    let title_h = f64::from(title.line_height_px());
    let expr = Rect::new(
        row.x0 + pad,
        row.y0 + pad_y,
        row.x1 - pad,
        row.y0 + pad_y + caption_h,
    );
    draw::text_in(
        ctx,
        expr,
        &engine::prettify(&entry.expr),
        &caption,
        c.text_dim,
        Align::Left,
    );
    let result = Rect::new(row.x0 + pad, expr.y1, row.x1 - pad, expr.y1 + title_h);
    draw::text_in(
        ctx,
        result,
        &entry.result,
        &title,
        c.text_primary,
        Align::Right,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Header;
    use crate::model::CalcModel;

    /// A window `w` wide with `h` below the header row.
    fn window(w: f64, h: f64) -> Rect {
        Rect::new(0.0, 0.0, w, Header::default().height + h)
    }

    /// The history panel docks open on the right when wide, collapses by
    /// default when narrow and opens there as a floating drawer with a dismiss
    /// scrim that captures clicks over it, and an explicit user hide survives
    /// relayout when wide. Fresh screen per phase.
    #[test]
    fn history_panel_docks_wide_floats_narrow_and_honours_user_choice() {
        // phase wide_window_docks_panel_open_by_default
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            let area = window(1200.0, 800.0);
            s.layout(area, &m);
            assert!(
                s.sidebar_open(),
                "wide window should default to history shown"
            );
            assert!(!s.is_overlay(), "wide window should dock, not float");
            assert!(s.panel.column().width() > 0.0);
            // Docked on the RIGHT: the history column hugs the right edge.
            assert!(
                (s.panel.column().x1 - area.x1).abs() < 0.5,
                "history docks on the right edge"
            );
        }

        // phase narrow_window_collapses_panel_by_default
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            s.layout(window(520.0, 800.0), &m);
            assert!(
                !s.sidebar_open(),
                "narrow window must NOT force the panel open"
            );
            assert!(s.is_overlay(), "narrow window floats the panel");
            assert_eq!(
                s.history_viewport(),
                Rect::ZERO,
                "collapsed → no history viewport"
            );
            let toggle = s.toggle_rect();
            assert!(
                toggle.x1 > 520.0 - 2.0 * Header::default().inset,
                "the show button sits at the trailing end of the content header"
            );
        }

        // phase narrow_window_opens_as_drawer_with_scrim
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            let area = window(520.0, 800.0);
            s.layout(area, &m);
            // user taps the toggle to open the drawer (snap to settle deterministically)
            s.toggle_sidebar();
            s.snap_sidebar(true);
            s.layout(area, &m);
            assert!(s.sidebar_open());
            assert!(s.is_overlay(), "still floating at this width");
            let column = s.panel.column();
            assert!(
                s.panel.toggle_rect().intersect(column).area() > 0.0,
                "the drawer carries its own hide button"
            );
            // a scrim tap (left of the right-anchored drawer) dismisses it
            let scrim_pt = Point::new(area.x0 + 10.0, area.center().y);
            assert!(s.scrim_at(scrim_pt));
            s.close_sidebar();
            s.snap_sidebar(false);
            s.layout(area, &m);
            assert!(!s.sidebar_open());
        }

        // phase drawer_captures_clicks_over_it
        {
            let mut s = CalcScreen::default();
            let m = CalcModel::seeded();
            let area = window(520.0, 800.0);
            s.layout(area, &m);
            s.toggle_sidebar();
            s.snap_sidebar(true);
            s.layout(area, &m);
            // a point inside the drawer is not a keypad/mem press
            let over = s.panel.column().center();
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
            let area = window(1200.0, 800.0);
            s.layout(area, &m);
            assert!(s.sidebar_open());
            // user explicitly hides it; that choice sticks across redraws
            s.toggle_sidebar();
            s.snap_sidebar(false);
            s.layout(area, &m);
            assert!(!s.sidebar_open(), "explicit hide must survive a relayout");
        }
    }

    #[test]
    fn short_windows_keep_every_control_apart_and_inside_the_body() {
        let overlaps = |a: Rect, b: Rect| a.intersect(b).area() > 0.01;
        let sizes = [
            // (width, height, keys must keep the minimum height)
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
            let area = window(w, h);
            let mut s = CalcScreen::default();
            s.layout(area, &CalcModel::seeded());
            let context = format!("{w}x{h}");
            let chrome = [s.display_rect]
                .into_iter()
                .chain(s.mem_rects.iter().copied())
                .collect::<Vec<_>>();
            for (i, a) in chrome.iter().enumerate() {
                assert!(
                    area.contains_rect(*a),
                    "{context}: chrome {a:?} leaves the window"
                );
                for b in &chrome[i + 1..] {
                    assert!(!overlaps(*a, *b), "{context}: chrome {a:?} overlaps {b:?}");
                }
            }
            for (i, key) in s.key_rects.iter().enumerate() {
                assert!(
                    area.contains_rect(*key),
                    "{context}: key {i} {key:?} leaves the window"
                );
                if roomy {
                    assert!(
                        key.height() >= s.m.key_min_h - 0.01,
                        "{context}: key {i} is {key:?}"
                    );
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
        // very narrow window → compact pad (4 cols)
        s.layout(window(440.0, 800.0), &m);
        assert_eq!(s.cols, KeypadLayout::Compact.cols());
        // wide window, panel docked → keypad still gets enough width to be
        // scientific or at least standard (never compact at this size)
        s.layout(window(1400.0, 800.0), &m);
        assert!(s.cols >= KeypadLayout::Standard.cols());
        assert_eq!(s.key_rects.len(), s.keys.len());
    }

    /// The four basic operators sit in one column in every layout, so the
    /// right edge of the pad reads the same at every width.
    #[test]
    fn operators_stay_in_one_column_in_every_layout() {
        for layout in [
            KeypadLayout::Compact,
            KeypadLayout::Standard,
            KeypadLayout::Scientific,
        ] {
            let keys = layout.keys();
            let column = |op: &str| {
                keys.iter()
                    .position(|k| matches!(k, Key::Ins(s) if *s == op))
                    .map(|i| i % layout.cols())
            };
            let first = column("/");
            for op in ["*", "-", "+"] {
                assert_eq!(
                    column(op),
                    first,
                    "{layout:?}: {op} leaves the operator column"
                );
            }
            assert_eq!(
                first,
                Some(layout.cols() - 1),
                "{layout:?}: operators trail the pad"
            );
        }
    }

    /// Each keycap takes its control role from what the key is: the entry pad
    /// is a key, functions are quiet, `=` is the one primary and `AC` is
    /// destructive.
    #[test]
    fn keycap_roles_follow_what_the_key_is() {
        assert_eq!(key_button_role(Key::Digit('7').role()), ButtonRole::Key);
        assert_eq!(key_button_role(Key::Ins("+").role()), ButtonRole::Key);
        assert_eq!(key_button_role(Key::Backspace.role()), ButtonRole::Key);
        assert_eq!(key_button_role(Key::Ins("sin(").role()), ButtonRole::Plain);
        assert_eq!(key_button_role(Key::Equals.role()), ButtonRole::Primary);
        assert_eq!(key_button_role(Key::Clear.role()), ButtonRole::Destructive);
        assert!(
            Key::Backspace.icon().is_some(),
            "backspace is an icon, not a glyph"
        );
    }
}
