//! The "Problems" panel: a scrollable list of compiler diagnostics. Clicking a
//! row that carries a source location asks the app to move the editor caret
//! there.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

use crate::app::plural;
use crate::compile::{DiagKind, Diagnostic};

const ROW_H: f64 = 26.0;
const HEADER_H: f64 = 30.0;

pub struct DiagnosticsPanel {
    rect: Rect,
    diags: Vec<Diagnostic>,
    scroll_y: f64,
    hover: Option<usize>,
}

impl Default for DiagnosticsPanel {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            diags: Vec::new(),
            scroll_y: 0.0,
            hover: None,
        }
    }
}

impl DiagnosticsPanel {
    #[cfg(test)]
    pub(crate) fn history_test_row_top(&self, row: usize) -> f64 {
        self.list_rect().y0 + row as f64 * ROW_H - self.scroll_y
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn set_diagnostics(&mut self, diags: Vec<Diagnostic>) {
        self.diags = diags;
        let max = self.max_scroll();
        if self.scroll_y > max {
            self.scroll_y = max;
        }
    }

    /// Borrow the current diagnostics so the source editor can render inline
    /// squiggles + hover tooltips from the same list the panel shows.
    pub fn diags(&self) -> &[Diagnostic] {
        &self.diags
    }

    pub fn error_count(&self) -> usize {
        self.diags
            .iter()
            .filter(|d| d.kind == DiagKind::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diags
            .iter()
            .filter(|d| d.kind == DiagKind::Warning)
            .count()
    }

    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + HEADER_H,
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn max_scroll(&self) -> f64 {
        let content = self.diags.len() as f64 * ROW_H;
        (content - self.list_rect().height()).max(0.0)
    }

    pub(crate) fn scroll_y(&self) -> f64 {
        self.scroll_y
    }

    pub fn scroll_by(&mut self, dy: f64) {
        self.scroll_y = (self.scroll_y + dy).clamp(0.0, self.max_scroll());
    }

    pub fn on_mouse_move(&mut self, p: Point) {
        self.hover = self.row_at(p);
    }

    fn row_at(&self, p: Point) -> Option<usize> {
        let list = self.list_rect();
        if !list.contains(p) {
            return None;
        }
        let i = ((p.y - list.y0 + self.scroll_y) / ROW_H).floor() as usize;
        if i < self.diags.len() { Some(i) } else { None }
    }

    /// Returns the 1-based line to jump to, if the clicked row has one.
    pub fn on_mouse_down(&mut self, p: Point) -> Option<usize> {
        let i = self.row_at(p)?;
        self.diags.get(i).and_then(|d| d.line)
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        // Panel background + top border.
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &self.rect,
        );
        // Bar role: the panel's one derived hairline against the editor.
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::chrome_hairline(theme),
            None,
            &Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + 1.0),
        );

        // Header.
        let errs = self.error_count();
        let warns = self.warning_count();
        let header = if errs == 0 && warns == 0 {
            "Problems — none".to_string()
        } else {
            format!(
                "Problems — {}, {}",
                plural(errs, "error", "errors"),
                plural(warns, "warning", "warnings")
            )
        };
        tr.draw_text_weighted(
            scene,
            &header,
            12.5,
            c.text_primary,
            transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 8.0)),
            None,
            TextWeight::Semibold,
        );

        let list = self.list_rect();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &list);

        if self.diags.is_empty() {
            tr.draw_text(
                scene,
                "No problems detected. The document compiled cleanly.",
                12.0,
                c.text_dim,
                transform * Affine::translate((list.x0 + 12.0, list.y0 + 8.0)),
                None,
            );
            scene.pop_layer();
            return;
        }

        for (i, d) in self.diags.iter().enumerate() {
            let y = list.y0 + i as f64 * ROW_H - self.scroll_y;
            if y + ROW_H < list.y0 || y > list.y1 {
                continue;
            }
            let row = Rect::new(list.x0, y, list.x1, y + ROW_H);
            if Some(i) == self.hover {
                scene.fill(Fill::NonZero, transform, c.lift(0.35), None, &row);
            }

            // Severity dot.
            let dot_color = match d.kind {
                DiagKind::Error => c.semantic.danger,
                DiagKind::Warning => c.semantic.warning,
            };
            let dot = Rect::new(
                list.x0 + 12.0,
                y + ROW_H * 0.5 - 4.0,
                list.x0 + 20.0,
                y + ROW_H * 0.5 + 4.0,
            );
            scene.fill(Fill::NonZero, transform, dot_color, None, &dot);

            // Location.
            let loc = match (d.line, d.col) {
                (Some(l), Some(col)) => format!("{l}:{col}"),
                (Some(l), None) => format!("{l}"),
                _ => "—".to_string(),
            };
            tr.draw_style(
                scene,
                &loc,
                c.text_dim,
                transform * Affine::translate((list.x0 + 30.0, y + 6.0)),
                None,
                &TextStyle::new(12.0).with_family(FontChoice::Monospace),
            );

            // Message (+ first hint, dimmed).
            let mut msg = d.message.clone();
            if let Some(h) = d.hints.first() {
                msg.push_str("  · ");
                msg.push_str(h);
            }
            tr.draw_text(
                scene,
                &msg,
                12.0,
                c.text_primary,
                transform * Affine::translate((list.x0 + 92.0, y + 6.0)),
                Some((list.width() - 104.0) as f32),
            );
        }

        scene.pop_layer();
    }
}
