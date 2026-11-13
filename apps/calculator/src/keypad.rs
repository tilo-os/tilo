//! The Calculator mode: a live display, a memory bar and a scientific keypad
//! picked from the available width. A plain renderer driven by the
//! [`CalcModel`]: it owns layout and hover caches, never state. Past
//! calculations are the window's History (`crate::history_face`).

use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::draw::{self, Align};
use crate::engine;
use crate::mathfield::{self, MathField};
use crate::model::{CalcModel, Key, KeyRole, KeypadLayout, KeypadPage};

pub const MEM_KEYS: [Key; 4] = [Key::MemClear, Key::MemRecall, Key::MemAdd, Key::MemSub];

/// The display's three values, each of which can be copied out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplaySlot {
    Expression,
    Result,
    Answer,
}

impl DisplaySlot {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Result => "result",
            Self::Answer => "previous answer",
        }
    }
}

/// How long a copy's mark stays.
const COPIED_SAID_FOR: std::time::Duration = std::time::Duration::from_millis(1_400);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hover {
    pub key: Option<usize>,
    pub mem: Option<usize>,
    pub copy: Option<DisplaySlot>,
}

/// The theme's measurements, cached from the last draw so the event path,
/// which has no theme, lays out the same way.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    pad: f64,
    gap: f64,
    cell_gap: f64,
    display_pad: f64,
    display_tall: f64,
    display_short: f64,
    status_line: f64,
    expr_tall_line: f64,
    result_tall_line: f64,
    short_line: f64,
    mem_h: f64,
    key_min_h: f64,
    key_min_w: f64,
    side_max_w: f64,
    key_max_w: f64,
    key_max_h: f64,
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
            side_max_w: f64::from(theme.tokens.metrics.panel_width_utility),
            key_max_w: 2.0 * f64::from(s.s12),
            key_max_h: 1.25 * f64::from(s.s16),
        }
    }
}

pub struct CalcScreen {
    pub field: MathField,
    pub field_focused: bool,
    keys: Vec<Key>,
    layout_kind: KeypadLayout,
    cols: usize,
    key_rects: Vec<Rect>,
    mem_rects: Vec<Rect>,
    expression_rect: Rect,
    display_rect: Rect,
    memory_rect: Rect,
    pub hovered_key: Option<usize>,
    pub hovered_mem: Option<usize>,
    pub pressed_key: Option<usize>,
    page: KeypadPage,
    pub(crate) copy_rects: [(DisplaySlot, Rect); 3],
    status_rect: Rect,
    result_rect: Rect,
    hovered_copy: Option<DisplaySlot>,
    /// `true` copied, `false` refused: the mark says what happened, not what
    /// the value is.
    copy_said: Option<(DisplaySlot, bool, ui::time::Instant)>,
    hover: Hover,
    content: Rect,
    m: Metrics,
    mem_w: f64,
}

impl Default for CalcScreen {
    fn default() -> Self {
        let layout_kind = KeypadLayout::Scientific;
        let stock = Theme::dark();
        Self {
            field: MathField::new(""),
            field_focused: false,
            keys: layout_kind.keys(),
            layout_kind,
            cols: layout_kind.cols(),
            key_rects: Vec::new(),
            mem_rects: Vec::new(),
            expression_rect: Rect::ZERO,
            display_rect: Rect::ZERO,
            memory_rect: Rect::ZERO,
            hovered_key: None,
            hovered_mem: None,
            pressed_key: None,
            page: KeypadPage::Primary,
            copy_rects: [
                (DisplaySlot::Expression, Rect::ZERO),
                (DisplaySlot::Result, Rect::ZERO),
                (DisplaySlot::Answer, Rect::ZERO),
            ],
            status_rect: Rect::ZERO,
            result_rect: Rect::ZERO,
            hovered_copy: None,
            copy_said: None,
            hover: Hover::default(),
            content: Rect::ZERO,
            m: Metrics::of(&stock),
            mem_w: f64::from(ui::tokens::Metrics::BASE.min_pointer_target) + 12.0,
        }
    }
}

impl CalcScreen {
    /// A refusal is the case that matters: without it a person believes the
    /// value is on the clipboard.
    pub fn say_copied(&mut self, slot: DisplaySlot, copied: bool) {
        self.copy_said = Some((slot, copied, ui::time::now()));
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    #[cfg(test)]
    pub(crate) fn key_center(&self, i: usize) -> Option<Point> {
        self.key_rects.get(i).map(ui::kurbo::Rect::center)
    }
    pub fn key_rect(&self, i: usize) -> Option<Rect> {
        self.key_rects.get(i).copied()
    }

    pub fn page(&self) -> KeypadPage {
        self.page
    }

    /// A no-op on a layout that shows every key.
    pub fn toggle_page(&mut self) {
        if !self.layout_kind.pages() {
            return;
        }
        self.page = match self.page {
            KeypadPage::Primary => KeypadPage::Functions,
            KeypadPage::Functions => KeypadPage::Primary,
        };
        self.keys = self.layout_kind.keys_on(self.page);
        self.pressed_key = None;
        self.hovered_key = None;
        self.rebuild_keys();
    }

    /// Empty while the line is not shown.
    pub fn copy_rect(&self, slot: DisplaySlot) -> Rect {
        self.copy_rects
            .iter()
            .find(|(have, _)| *have == slot)
            .map_or(Rect::ZERO, |(_, rect)| *rect)
    }

    pub fn copy_at(&self, p: Point) -> Option<DisplaySlot> {
        self.copy_rects
            .iter()
            .find(|(_, rect)| *rect != Rect::ZERO && rect.contains(p))
            .map(|(slot, _)| *slot)
    }

    #[cfg(test)]
    pub(crate) fn key_index_for_label(&self, label: &str) -> Option<usize> {
        self.keys.iter().position(|k| k.label() == label)
    }

    #[cfg(test)]
    pub(crate) fn mem_center(&self, i: usize) -> Option<Point> {
        self.mem_rects.get(i).map(ui::kurbo::Rect::center)
    }
    pub fn mem_rect(&self, i: usize) -> Option<Rect> {
        self.mem_rects.get(i).copied()
    }

    pub fn expression_rect(&self) -> Rect {
        self.expression_rect
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

    pub fn content_column(&self) -> Rect {
        self.content
    }

    fn ensure_layout(&mut self, keypad_w: f64) {
        self.adopt_layout(KeypadLayout::for_width(keypad_w));
    }

    fn rebuild_keys(&mut self) {
        // A same-frame hit test must not land on yesterday's key.
        self.key_rects.clear();
    }

    /// A short pad trades function keys for key height before its keys
    /// shrink; the compact pad is the floor.
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

    fn adopt_layout(&mut self, want: KeypadLayout) {
        if want != self.layout_kind {
            self.layout_kind = want;
            self.cols = want.cols();
            // A new width starts at the number page.
            self.page = KeypadPage::Primary;
            self.keys = want.keys_on(self.page);
            self.pressed_key = None;
            self.hovered_key = None;
        }
    }

    pub fn scale_for(&mut self, theme: &Theme) {
        self.m = Metrics::of(theme);
    }

    /// Draw path only.
    pub fn prepare(&mut self, tr: &mut TextRenderer, theme: &Theme, model: &CalcModel, area: Rect) {
        self.scale_for(theme);
        self.mem_w = MEM_KEYS
            .iter()
            .map(|key| {
                ButtonFace::new(Rect::ZERO, key.label())
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Secondary)
                    .intrinsic_width(tr, theme)
            })
            .fold(0.0, f64::max);
        self.field.set_source(&model.expr);
        self.field.set_size(mathfield::formula_size(theme));
        let formula_h = (self.field.content_height(tr)
            / f64::from(tr.text_scale().max(f32::EPSILON)))
        .min(mathfield::CARD_MAX_H * theme.responsive_scale());
        let extra = (formula_h - self.m.expr_tall_line).max(0.0);
        self.m.expr_tall_line += extra;
        self.m.display_tall += extra;
        // Keep a fraction or radical intact even in the compact display.
        self.m.display_short = self
            .m
            .display_short
            .max(formula_h + self.m.short_line + 2.0 * f64::from(theme.tokens.spacing.s2));
        self.layout(area);
    }

    /// Call at the top of both draw and event handling, so hit tests match the
    /// last paint. The keys keep `key_min_h` while the chrome gives way: a
    /// short window drops the status line, and a wide one moves the display
    /// beside the pad.
    pub fn layout(&mut self, area: Rect) {
        self.content = area;
        let m = self.m;
        let body = area;
        let left = Rect::new(
            body.x0 + m.pad,
            body.y0 + m.pad,
            (body.x1 - m.pad).max(body.x0 + m.pad),
            (body.y1 - m.pad).max(body.y0 + m.pad),
        );
        self.ensure_layout(left.width());

        let (chrome, grid, display_h) = self.split_regions(left);
        // Keys stop at `key_max_w`, so a wide window does not turn them into
        // tiles; a stacked display narrows with the pad.
        let pad_max_w = self.cols as f64 * m.key_max_w + (self.cols as f64 - 1.0) * m.cell_gap;
        let stacked = chrome == left;
        let (chrome, grid) = if stacked {
            let width = left.width().min(pad_max_w);
            let x0 = left.x0 + (left.width() - width) / 2.0;
            (
                Rect::new(x0, chrome.y0, x0 + width, chrome.y1),
                Rect::new(x0, grid.y0, x0 + width, grid.y1),
            )
        } else {
            (
                chrome,
                Rect::new(
                    grid.x0,
                    grid.y0,
                    grid.x0 + grid.width().min(pad_max_w),
                    grid.y1,
                ),
            )
        };
        // Keys stop at `key_max_h`, so the pad stands in the middle of what a
        // tall window leaves.
        let cols = self.cols;
        let rows = self.keys.len().div_ceil(cols);
        let ch = ((grid.height() - m.cell_gap * (rows as f64 - 1.0)) / rows as f64)
            .clamp(1.0, m.key_max_h.max(1.0));
        let slack =
            (grid.height() - (rows as f64 * ch + (rows as f64 - 1.0) * m.cell_gap)).max(0.0);
        let dy = (slack / 2.0).floor();
        let (chrome, grid) = if stacked {
            (
                Rect::new(chrome.x0, chrome.y0 + dy, chrome.x1, chrome.y1),
                Rect::new(grid.x0, grid.y0 + dy, grid.x1, grid.y1),
            )
        } else {
            (chrome, Rect::new(grid.x0, grid.y0 + dy, grid.x1, grid.y1))
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
        self.status_rect = if self.display_is_short() {
            Rect::ZERO
        } else {
            Rect::new(
                self.display_rect.x0 + m.display_pad,
                expr_top - m.status_line,
                self.display_rect.x1 - m.display_pad,
                expr_top,
            )
        };
        let result_h = if self.display_is_short() {
            m.short_line
        } else {
            m.result_tall_line
        };
        self.result_rect = Rect::new(
            self.expression_rect.x0,
            expr_bottom,
            self.expression_rect.x1,
            expr_bottom + result_h,
        );
        self.copy_rects = [
            (
                DisplaySlot::Expression,
                Self::copy_icon(self.expression_rect),
            ),
            // Reading surfaces, not editors: the whole line is the target.
            (DisplaySlot::Result, self.result_rect),
            (DisplaySlot::Answer, self.status_rect),
        ];

        self.key_rects.clear();
        let cw = (grid.width() - m.cell_gap * (cols as f64 - 1.0)) / cols as f64;
        for idx in 0..self.keys.len() {
            let r = idx / cols;
            let c = idx % cols;
            let x0 = grid.x0 + c as f64 * (cw + m.cell_gap);
            let y0 = grid.y0 + r as f64 * (ch + m.cell_gap);
            self.key_rects.push(Rect::new(x0, y0, x0 + cw, y0 + ch));
        }
    }

    /// Splits `left` by how much room there is: stacked tall, stacked short, or
    /// side by side.
    fn split_regions(&mut self, left: Rect) -> (Rect, Rect, f64) {
        let m = self.m;
        let memory_min_w = 4.0 * self.mem_w + 3.0 * m.cell_gap;
        let side_w = (left.width() * 0.38).clamp(memory_min_w, m.side_max_w.max(memory_min_w));
        let side_pad_min_w = 4.0 * m.key_min_w + 3.0 * m.cell_gap;
        let tall_grid = left.height() - m.display_tall - m.mem_h - 2.0 * m.gap;
        if tall_grid >= self.grid_min_h() {
            let grid_top = left.y0 + m.display_tall + m.gap + m.mem_h + m.gap;
            (
                left,
                Rect::new(left.x0, grid_top, left.x1, left.y1),
                m.display_tall,
            )
        } else if left.height() - m.display_short - m.mem_h - 2.0 * m.gap >= self.grid_min_h() {
            // Drop the status line before splitting the display from the pad.
            let grid_top = left.y0 + m.display_short + m.gap + m.mem_h + m.gap;
            (
                left,
                Rect::new(left.x0, grid_top, left.x1, left.y1),
                m.display_short,
            )
        } else if left.width() - side_w - m.gap >= side_pad_min_w {
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
        }
    }

    fn copy_icon(line: Rect) -> Rect {
        if line.width() < 40.0 || line.height() < 12.0 {
            return Rect::ZERO;
        }
        let side = line.height().clamp(12.0, 20.0);
        let y0 = line.y0 + (line.height() - side) / 2.0;
        Rect::new(line.x1 - side, y0, line.x1, y0 + side)
    }

    /// Room is left for the copy icon, so the value never runs under it.
    fn line_text(line: Rect, copy: Rect) -> Rect {
        if copy == Rect::ZERO || copy.height() < 12.0 || copy.width() >= line.width() {
            return line;
        }
        Rect::new(line.x0, line.y0, (copy.x0 - 6.0).max(line.x0), line.y1)
    }

    fn copy_icon_of(&self, slot: DisplaySlot) -> Rect {
        let copy = self.copy_rect(slot);
        if copy == Rect::ZERO {
            return Rect::ZERO;
        }
        match slot {
            DisplaySlot::Expression => copy,
            DisplaySlot::Result | DisplaySlot::Answer => Self::copy_icon(copy),
        }
    }

    fn grid_min_h(&self) -> f64 {
        let rows = self.keys.len().div_ceil(self.cols) as f64;
        rows * self.m.key_min_h + (rows - 1.0) * self.m.cell_gap
    }

    fn layout_memory(&mut self, x0: f64, top: f64, width: f64, height: f64) {
        self.mem_rects.clear();
        for i in 0..4 {
            let x = x0 + f64::from(i) * (width + self.m.cell_gap);
            self.mem_rects
                .push(Rect::new(x, top, x + width, top + height));
        }
    }

    fn display_is_short(&self) -> bool {
        self.display_rect.height() < self.m.display_tall - 0.5
    }

    fn expression_band(&self) -> (f64, f64) {
        let d = self.display_rect;
        let m = &self.m;
        if self.display_is_short() {
            let expr_h = (d.height() - m.short_line - 2.0 * m.cell_gap).max(m.short_line);
            let top = d.y0 + (d.height() - expr_h - m.short_line) / 2.0;
            (top, top + expr_h)
        } else {
            let pad = (d.height() - m.status_line - m.expr_tall_line - m.result_tall_line) / 2.0;
            let top = d.y0 + pad + m.status_line;
            (top, top + m.expr_tall_line)
        }
    }

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

    fn draw_display(
        &mut self,
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
        let expr_line = Rect::new(display.x0 + pad, expr_top, display.x1 - pad, expr_bottom);
        let expr_icon = self.copy_icon_of(DisplaySlot::Expression);
        let expr_rect = Self::line_text(expr_line, expr_icon);

        if !short {
            let status = self.status_rect;
            let icon = self.copy_icon_of(DisplaySlot::Answer);
            let text = Self::line_text(status, icon);
            let ans = format!("Ans = {}", engine::format_number(model.ans));
            draw::text_in(
                &mut ctx,
                text,
                &ans,
                &theme.text(TextRole::Caption).numeric(),
                c.text_dim,
                Align::Right,
            );
            self.draw_copy_icon(&mut ctx, DisplaySlot::Answer, theme, c.text_dim);
        }

        self.field.set_rect(expr_rect, ctx.text);
        if model.expr.trim().is_empty() {
            draw::text_in(
                &mut ctx,
                expr_rect,
                "0",
                &theme.text(TextRole::Title).numeric(),
                c.text_tertiary,
                Align::Left,
            );
        }
        self.draw_copy_icon(&mut ctx, DisplaySlot::Expression, theme, c.text_tertiary);
        let local = theme.on_ground(c.surfaces.raised);
        self.field.draw(
            ctx.scene,
            t,
            ctx.text,
            &local,
            c.text_primary,
            self.field_focused,
        );

        let res_line = self.result_rect;
        let res_icon = self.copy_icon_of(DisplaySlot::Result);
        let res_rect = Self::line_text(res_line, res_icon);
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
        // The answer must stay whole, so it shrinks rather than clipping.
        let mut res_style = res_style;
        let measured = f64::from(ctx.text.measure_style(&res_txt, None, &res_style).0);
        if measured > res_rect.width() && measured > 0.0 && res_rect.width() > 0.0 {
            let fit = (res_rect.width() / measured).clamp(0.55, 1.0) as f32;
            res_style.size *= fit;
            res_style.line_height *= fit;
        }
        draw::text_in(
            &mut ctx,
            res_rect,
            &res_txt,
            &res_style,
            res_ink,
            Align::Right,
        );
        if !res_txt.is_empty() {
            self.draw_copy_icon(&mut ctx, DisplaySlot::Result, theme, c.text_tertiary);
        }
    }

    /// The value is the target; this is its affordance, not a second control.
    fn draw_copy_icon(
        &self,
        ctx: &mut PaintCtx<'_>,
        slot: DisplaySlot,
        theme: &Theme,
        ink: vello::peniko::Color,
    ) {
        let rect = self.copy_icon_of(slot);
        if rect == Rect::ZERO {
            return;
        }
        let hot = self.hovered_copy == Some(slot);
        let said_now = self.copy_said.is_some_and(|(said, _, at)| {
            said == slot && ui::time::now().saturating_duration_since(at) < COPIED_SAID_FOR
        });
        // The result's mark stays; the other lines reveal theirs when pointed
        // at. A copy's answer shows whatever the pointer is doing.
        if !hot && !said_now && slot != DisplaySlot::Result {
            return;
        }
        if hot {
            ui::surface::card(
                ctx.scene,
                ctx.transform,
                rect,
                f64::from(theme.tokens.radii.control),
                theme.tokens.colors.surfaces.raised,
            );
        }
        // A copy that just happened says so, for as long as it is news.
        let said = self.copy_said.filter(|(said, _, at)| {
            *said == slot && ui::time::now().saturating_duration_since(*at) < COPIED_SAID_FOR
        });
        let (icon, colour) = match said {
            Some((_, true, _)) => (
                ui::icons::Icon::CircleCheck,
                theme.tokens.colors.text_primary,
            ),
            Some((_, false, _)) => (
                ui::icons::Icon::TriangleAlert,
                theme.tokens.colors.semantic.danger,
            ),
            None => (
                ui::icons::Icon::Copy,
                if hot {
                    theme.tokens.colors.text_primary
                } else {
                    ink
                },
            ),
        };
        icon.draw(
            ctx.scene,
            ctx.transform,
            rect.inset(rect.height().min(rect.width()) * 0.16),
            colour,
        );
    }

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
                .role(ButtonRole::Secondary)
                .hovered(self.hovered_mem == Some(i));
            Button::paint(scene, t, tr, theme, &face);
        }
        if model.memory != 0.0 && self.memory_rect.width() >= 48.0 {
            let style = theme.text(TextRole::Label).numeric();
            let value = engine::format_number(model.memory);
            // Never a clipped label: "Memory 42", else "M 42", else the value.
            let readout = [format!("Memory {value}"), format!("M {value}"), value]
                .into_iter()
                .find(|text| {
                    f64::from(tr.measure_style(text, None, &style).0) <= self.memory_rect.width()
                });
            if let Some(readout) = readout {
                let mut ctx = PaintCtx::new(scene, t, tr, theme);
                draw::text_in(
                    &mut ctx,
                    self.memory_rect,
                    &readout,
                    &style,
                    theme.tokens.colors.text_dim,
                    Align::Right,
                );
            }
        }
    }

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
            let local = key_theme(role, theme);
            let ink = face.colors(&local).2;
            Button::paint(scene, t, tr, &local, &face);
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

    pub fn key_at(&self, p: Point) -> Option<usize> {
        self.key_rects.iter().position(|r| r.contains(p))
    }
    pub fn mem_at(&self, p: Point) -> Option<usize> {
        self.mem_rects.iter().position(|r| r.contains(p))
    }
    /// Returns whether anything painted changed.
    pub fn set_pointer(&mut self, p: Point) -> bool {
        self.hovered_copy = self.copy_at(p);
        self.hovered_key = if self.hovered_copy.is_some() {
            None
        } else {
            self.key_at(p)
        };
        self.hovered_mem = self.mem_at(p);
        let next = Hover {
            key: self.hovered_key,
            mem: self.hovered_mem,
            copy: self.hovered_copy,
        };
        let changed = next != self.hover;
        self.hover = next;
        changed
    }
}

/// Functions are secondary, digits lift one surface, `=` is primary.
fn key_button_role(role: KeyRole) -> ButtonRole {
    match role {
        KeyRole::Digit | KeyRole::Operator => ButtonRole::Key,
        KeyRole::Function => ButtonRole::Secondary,
        KeyRole::Equals => ButtonRole::Primary,
        KeyRole::Clear => ButtonRole::Destructive,
    }
}

fn key_theme(role: KeyRole, theme: &Theme) -> Theme {
    if role == KeyRole::Digit {
        theme.on_ground(theme.tokens.colors.surfaces.raised)
    } else {
        theme.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Header;
    use crate::model::CalcModel;

    #[test]
    fn enlarged_calculator_keeps_a_stacked_display_and_usable_keypad() {
        let dark: fn() -> Theme = Theme::dark;
        let light: fn() -> Theme = Theme::light;
        for (base, ui_scale, text_scale) in [
            (dark, 1.0, 1.0),
            (dark, 1.5, 1.0),
            (light, 1.5, 1.0),
            (dark, 1.5, 1.5),
            (dark, 1.0, 2.0),
        ] {
            let theme = base().with_ui_scale(ui_scale).with_text_scale(text_scale);
            let mut screen = CalcScreen::default();
            let mut text = TextRenderer::new();
            // The live desktop carries the reader's text scale.
            text.set_text_scale(theme.text_scale);
            let model = CalcModel {
                expr: "sqrt(2)/3".into(),
                ..CalcModel::seeded()
            };
            let area = Rect::new(0.0, 0.0, 1440.0, 810.0);
            screen.prepare(&mut text, &theme, &model, area);
            // Re-place the field as the draw pass does, then measure it.
            let band = screen.expression_rect;
            screen.field.set_rect(band, &mut text);
            let content = screen.field.content_height(&mut text);
            assert!(area.contains_rect(screen.display_rect));
            assert!(
                band.height() >= content || screen.field.overflows(),
                "a formula that fits its display must not scroll: band {} vs content {content}",
                band.height()
            );
            // Grow for a larger text scale instead of clipping.
            let plain = CalcScreen::default().m.display_tall;
            assert!(
                screen.m.display_tall > plain,
                "the display must grow with the text scale ({} vs {plain})",
                screen.m.display_tall
            );
            for key in &screen.key_rects {
                assert!(area.contains_rect(*key));
                assert!(key.height() > 0.0 && key.width() > 0.0);
                assert!(!key.overlaps(screen.display_rect));
            }
            if theme.text_scale <= 1.0 {
                let key = screen.key_rects.first().unwrap();
                assert!(key.y0 >= screen.display_rect.y1);
                assert!(key.width() / key.height() < 3.0);
                assert!(key.height() >= f64::from(theme.tokens.metrics.min_pointer_target));
            }
        }
    }

    fn window(w: f64, h: f64) -> Rect {
        Rect::new(0.0, 0.0, w, Header::default().height + h)
    }

    #[test]
    fn short_windows_keep_every_control_apart_and_inside_the_body() {
        let overlaps = |a: Rect, b: Rect| a.intersect(b).area() > 0.01;
        let sizes = [
            // (width, height, keys keep their minimum height)
            (1200.0, 800.0, true),
            (440.0, 800.0, true),
            (914.0, 270.0, true),
            (825.0, 274.0, true),
            (600.0, 280.0, true),
            (360.0, 300.0, false),
        ];
        for (w, h, roomy) in sizes {
            let area = window(w, h);
            let mut s = CalcScreen::default();
            s.layout(area);
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
        s.layout(window(440.0, 800.0));
        assert_eq!(s.cols, KeypadLayout::Compact.cols());
        s.layout(window(1400.0, 800.0));
        assert!(s.cols >= KeypadLayout::Standard.cols());
        assert_eq!(s.key_rects.len(), s.keys.len());
        let area = window(1700.0, 1400.0);
        s.layout(area);
        let m = s.m;
        for key in &s.key_rects {
            assert!(key.width() <= m.key_max_w + 0.5, "{key:?}");
            assert!(key.height() <= m.key_max_h + 0.5, "{key:?}");
        }
        let left = s
            .key_rects
            .iter()
            .map(|r| r.x0)
            .fold(f64::INFINITY, f64::min);
        let right = s.key_rects.iter().map(|r| r.x1).fold(0.0, f64::max);
        assert!((s.display_rect.x0 - left).abs() < 0.5 && (s.display_rect.x1 - right).abs() < 0.5);
        let content = s.content;
        let centre = f64::midpoint(content.x0, content.x1);
        assert!(
            (f64::midpoint(left, right) - centre).abs() < m.pad + 0.5,
            "the pad is centred"
        );
        let bottom = s.key_rects.iter().map(|r| r.y1).fold(0.0, f64::max);
        let above = s.display_rect.y0 - content.y0;
        let below = content.y1 - bottom;
        assert!(above > m.pad + 1.0, "the pad moved down: {above}");
        assert!((above - below).abs() < 2.0, "{above} above, {below} below");
    }

    /// The four operators sit in one column in every layout.
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

    /// Every key has a face, and the roles keep their hierarchy.
    #[test]
    fn every_key_has_a_surface_and_digits_stand_apart() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            for key in KeypadLayout::Scientific
                .keys()
                .iter()
                .chain(MEM_KEYS.iter())
            {
                let role = key.role();
                let local = key_theme(role, &theme);
                let (fill, _, _) = ButtonFace::new(Rect::ZERO, "")
                    .role(key_button_role(role))
                    .colors(&local);
                assert!(fill.components[3] > 0.99, "bare key: {}", key.label());
            }
            let fill = |role| {
                ButtonFace::new(Rect::ZERO, "")
                    .role(key_button_role(role))
                    .colors(&key_theme(role, &theme))
                    .0
            };
            assert_ne!(fill(KeyRole::Digit), fill(KeyRole::Function));
        }
    }

    #[test]
    fn keycap_roles_follow_what_the_key_is() {
        assert_eq!(key_button_role(Key::Digit('7').role()), ButtonRole::Key);
        assert_eq!(key_button_role(Key::Ins("+").role()), ButtonRole::Key);
        assert_eq!(key_button_role(Key::Backspace.role()), ButtonRole::Key);
        assert_eq!(
            key_button_role(Key::Ins("sin(").role()),
            ButtonRole::Secondary
        );
        assert_eq!(key_button_role(Key::Equals.role()), ButtonRole::Primary);
        assert_eq!(key_button_role(Key::Clear.role()), ButtonRole::Destructive);
        assert!(
            Key::Backspace.icon().is_some(),
            "backspace is an icon, not a glyph"
        );
    }
    #[test]
    fn a_narrow_pad_pages_between_numbers_and_functions() {
        let mut screen = CalcScreen::default();
        screen.layout(Rect::new(0.0, 0.0, 390.0, 844.0));
        assert_eq!(screen.page(), KeypadPage::Primary);
        assert!(screen.keys().iter().any(|key| matches!(key, Key::Page(_))));
        let primary = screen.keys().len();
        screen.toggle_page();
        assert_eq!(screen.page(), KeypadPage::Functions);
        assert_eq!(screen.keys().len(), 28);
        screen.toggle_page();
        assert_eq!(screen.page(), KeypadPage::Primary);
        assert_eq!(screen.keys().len(), primary);
    }
}
