//! The Calculator mode: a live display, a memory bar, a scientific keypad,
//! and a clickable history. This is a plain renderer driven by the
//! [`CalcModel`]; it owns only layout + hover caches, never state.

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
use crate::model::{keypad, CalcModel, Key, KeyRole, KEYPAD_COLS};

/// Memory-bar buttons, left → right.
pub const MEM_KEYS: [Key; 4] = [Key::MemClear, Key::MemRecall, Key::MemAdd, Key::MemSub];

pub struct CalcScreen {
    keys: Vec<Key>,
    key_rects: Vec<Rect>,
    mem_rects: Vec<Rect>,
    history_rects: Vec<(usize, Rect)>,
    clear_hist_rect: Rect,
    pub hovered_key: Option<usize>,
    pub hovered_mem: Option<usize>,
    pub pressed_key: Option<usize>,
    pub history_scroll: f64,
    history_view: Rect,
    history_content_h: f64,
}

impl Default for CalcScreen {
    fn default() -> Self {
        Self {
            keys: keypad(),
            key_rects: Vec::new(),
            mem_rects: Vec::new(),
            history_rects: Vec::new(),
            clear_hist_rect: Rect::ZERO,
            hovered_key: None,
            hovered_mem: None,
            pressed_key: None,
            history_scroll: 0.0,
            history_view: Rect::ZERO,
            history_content_h: 0.0,
        }
    }
}

impl CalcScreen {
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// Recompute all geometry for the given body `area`. Called at the top
    /// of both draw and event handling so hit-tests match the last paint.
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        let pad = 16.0;
        let hist_w = (area.width() * 0.30).clamp(280.0, 380.0);
        let left = Rect::new(area.x0 + pad, area.y0 + pad, area.x1 - hist_w - pad, area.y1 - pad);
        let right = Rect::new(area.x1 - hist_w, area.y0, area.x1, area.y1);

        // Display + memory bar + keypad stacked in the left column.
        let display_h = 124.0;
        let mem_h = 44.0;
        let gap = 12.0;
        let mem_top = left.y0 + display_h + gap;
        let grid_top = mem_top + mem_h + gap;

        // Memory bar: 4 buttons on the left half.
        self.mem_rects.clear();
        let mem_btn_w = 64.0;
        let mem_gap = 8.0;
        for i in 0..4 {
            let x0 = left.x0 + i as f64 * (mem_btn_w + mem_gap);
            self.mem_rects.push(Rect::new(x0, mem_top, x0 + mem_btn_w, mem_top + mem_h));
        }

        // Keypad grid.
        self.key_rects.clear();
        let rows = (self.keys.len() + KEYPAD_COLS - 1) / KEYPAD_COLS;
        let cell_gap = 8.0;
        let grid = Rect::new(left.x0, grid_top, left.x1, left.y1);
        let cw = (grid.width() - cell_gap * (KEYPAD_COLS as f64 - 1.0)) / KEYPAD_COLS as f64;
        let ch = (grid.height() - cell_gap * (rows as f64 - 1.0)) / rows as f64;
        for idx in 0..self.keys.len() {
            let r = idx / KEYPAD_COLS;
            let c = idx % KEYPAD_COLS;
            let x0 = grid.x0 + c as f64 * (cw + cell_gap);
            let y0 = grid.y0 + r as f64 * (ch + cell_gap);
            self.key_rects.push(Rect::new(x0, y0, x0 + cw, y0 + ch));
        }

        // History list rows on the right.
        let header_h = 48.0;
        let row_h = 56.0;
        self.clear_hist_rect =
            Rect::new(right.x1 - 76.0, right.y0 + 12.0, right.x1 - 16.0, right.y0 + 36.0);
        self.history_view =
            Rect::new(right.x0, right.y0 + header_h, right.x1, right.y1);
        self.history_content_h = model.history.len() as f64 * row_h;
        let max_scroll = (self.history_content_h - self.history_view.height()).max(0.0);
        self.history_scroll = self.history_scroll.clamp(0.0, max_scroll);

        self.history_rects.clear();
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
        let hist_w = (area.width() * 0.30).clamp(280.0, 380.0);
        let left = Rect::new(area.x0 + pad, area.y0 + pad, area.x1 - hist_w - pad, area.y1 - pad);
        let right = Rect::new(area.x1 - hist_w, area.y0, area.x1, area.y1);

        // ---- Display panel -------------------------------------------------
        let display = Rect::new(left.x0, left.y0, left.x1, left.y0 + 124.0);
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

        // ---- History panel -------------------------------------------------
        draw::fill_rect(scene, t, right, c.surfaces.base);
        draw::line(
            scene,
            t,
            Point::new(right.x0, right.y0),
            Point::new(right.x0, right.y1),
            c.border_standard,
            1.0,
        );
        let header = Rect::new(right.x0 + 16.0, right.y0, right.x1 - 16.0, right.y0 + 48.0);
        draw::text_in(scene, tr, t, header, "History", 16.0, c.text_primary, Align::Left, TextWeight::Semibold);
        // Clear-history pill
        let clr_hover = false;
        let _ = clr_hover;
        draw::fill_rrect(scene, t, self.clear_hist_rect, 6.0, c.neutral[2]);
        draw::text_in(scene, tr, t, self.clear_hist_rect, "Clear", 12.0, c.text_dim, Align::Center, TextWeight::Medium);

        // clip the rows to the list viewport
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
        self.key_rects.iter().position(|r| r.contains(p))
    }
    pub fn mem_at(&self, p: Point) -> Option<usize> {
        self.mem_rects.iter().position(|r| r.contains(p))
    }
    /// History index under `p`, if the click hit a row inside the viewport.
    pub fn history_at(&self, p: Point) -> Option<usize> {
        if !self.history_view.contains(p) {
            return None;
        }
        self.history_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }
    pub fn clear_history_at(&self, p: Point) -> bool {
        self.clear_hist_rect.contains(p)
    }
    pub fn history_viewport(&self) -> Rect {
        self.history_view
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
