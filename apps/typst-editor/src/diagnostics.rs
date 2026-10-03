//! The "Problems" panel: a scrollable list of compiler diagnostics. Clicking a
//! row that carries a source location asks the app to move the editor caret
//! there.
//!
//! Every row is a library [`ListRowFace`]: the severity is the leading badge
//! and the status dot, the message is the title, the first hint is the
//! secondary line and the `line:column` location is the numeric meta. The
//! panel paints no ground of its own: it stands on the ground it is drawn on
//! and one hairline separates it from the editor above.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Interaction, ListRowFace, RowLeading, RowStatus, badge_column_width};

use crate::app::plural;
use crate::compile::{DiagKind, Diagnostic};

const ERROR_BADGE: &str = "Error";
const WARNING_BADGE: &str = "Warning";

pub struct DiagnosticsPanel {
    rect: Rect,
    diags: Vec<Diagnostic>,
    scroll_y: f64,
    hover: Option<usize>,
    /// Measured for the theme the panel was last drawn or laid out in.
    heights: Vec<f64>,
    header_h: f64,
    empty_h: f64,
    badge_w: f64,
}

impl Default for DiagnosticsPanel {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            diags: Vec::new(),
            scroll_y: 0.0,
            hover: None,
            heights: Vec::new(),
            header_h: 0.0,
            empty_h: 0.0,
            badge_w: 0.0,
        }
    }
}

/// The text of a diagnostic's location column.
fn location(d: &Diagnostic) -> Option<String> {
    match (d.line, d.col) {
        (Some(l), Some(col)) => Some(format!("{l}:{col}")),
        (Some(l), None) => Some(l.to_string()),
        _ => None,
    }
}

impl DiagnosticsPanel {
    /// Where row `row` is now, scrolled: it may lie outside the visible list.
    pub(crate) fn row_rect(&self, row: usize) -> Rect {
        let top = self.list_rect().y0 + self.heights.iter().take(row).sum::<f64>() - self.scroll_y;
        Rect::new(
            self.rect.x0,
            top,
            self.rect.x1,
            top + self.heights.get(row).copied().unwrap_or(0.0),
        )
    }

    /// One accessibility node per listed problem, so a reader can reach each
    /// and jump to it. A row scrolled out of view keeps its node, without an
    /// area.
    pub fn accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        let list = self.list_rect();
        self.diags
            .iter()
            .enumerate()
            .map(|(index, d)| {
                let meta = location(d);
                let mut face = self.face(d);
                if let Some(meta) = meta.as_deref() {
                    face = face.meta(meta);
                }
                let shown = self.row_rect(index).intersect(list);
                face.accessibility(
                    if shown.area() > 0.0 {
                        shown
                    } else {
                        Rect::ZERO
                    },
                    format!("typst.problem.{index}"),
                )
            })
            .collect()
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.clamp_scroll();
    }

    pub fn set_diagnostics(&mut self, diags: Vec<Diagnostic>) {
        self.diags = diags;
        self.heights.clear();
        self.hover = None;
        self.clamp_scroll();
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

    /// What the panel needs to show every row: its header and the whole list.
    /// Valid after [`Self::measure_rows`].
    pub fn preferred_height(&self) -> f64 {
        self.header_h + self.content_height()
    }

    fn content_height(&self) -> f64 {
        if self.diags.is_empty() {
            self.empty_h
        } else {
            self.heights.iter().sum()
        }
    }

    /// Measure the header and every row in `theme`. Call before laying the
    /// panel out and again whenever the diagnostics or the theme change.
    pub fn measure_rows(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let spacing = &theme.tokens.spacing;
        self.header_h = f64::from(theme.tokens.metrics.min_pointer_target + 2.0 * spacing.s1);
        let line = f64::from(tr.measure_style("Ag", None, &theme.text(TextRole::Body)).1);
        self.empty_h = line + 2.0 * f64::from(spacing.s3);
        self.badge_w = badge_column_width(tr, theme, &[ERROR_BADGE, WARNING_BADGE]);
        self.heights = self
            .diags
            .iter()
            .map(|d| self.face(d).height(tr, theme))
            .collect();
        self.clamp_scroll();
    }

    /// The row for `d`, without interaction state.
    fn face<'a>(&self, d: &'a Diagnostic) -> ListRowFace<'a> {
        let (badge, status) = match d.kind {
            DiagKind::Error => (ERROR_BADGE, RowStatus::Error),
            DiagKind::Warning => (WARNING_BADGE, RowStatus::Warning),
        };
        let mut face = ListRowFace::new(&d.message)
            .leading(RowLeading::Badge(badge))
            .leading_column(self.badge_w)
            .status(status);
        if let Some(hint) = d.hints.first() {
            face = face.secondary(hint);
        }
        face
    }

    /// The header band: the title and counts on the left, the app's close
    /// button on the right.
    pub fn header_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (self.rect.y0 + self.header_h).min(self.rect.y1),
        )
    }

    /// Where the close button goes: a square control at the header's
    /// trailing edge, inset by the content gutter.
    pub fn close_rect(&self, theme: &Theme) -> Rect {
        let header = self.header_rect();
        let side = f64::from(theme.tokens.metrics.min_pointer_target);
        let inset = f64::from(theme.tokens.spacing.s3);
        let x1 = header.x1 - inset;
        let cy = header.center().y;
        Rect::new(x1 - side, cy - side / 2.0, x1, cy + side / 2.0)
    }

    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.header_h,
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.list_rect().height()).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
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
        let mut y = list.y0 - self.scroll_y;
        for (i, h) in self.heights.iter().enumerate() {
            if p.y >= y && p.y < y + h {
                return Some(i);
            }
            y += h;
        }
        None
    }

    /// Returns the 1-based line to jump to, if the clicked row has one.
    pub fn on_mouse_down(&mut self, p: Point) -> Option<usize> {
        let i = self.row_at(p)?;
        self.diags.get(i).and_then(|d| d.line)
    }

    /// The header's summary of what the list holds; empty when it holds nothing.
    fn summary(&self) -> String {
        let (errs, warns) = (self.error_count(), self.warning_count());
        match (errs, warns) {
            (0, 0) => String::new(),
            (e, 0) => plural(e, "error", "errors"),
            (0, w) => plural(w, "warning", "warnings"),
            (e, w) => format!(
                "{}, {}",
                plural(e, "error", "errors"),
                plural(w, "warning", "warnings")
            ),
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.heights.len() != self.diags.len() || self.header_h <= 0.0 {
            self.measure_rows(tr, theme);
        }
        let colors = &theme.tokens.colors;
        let gutter = f64::from(theme.tokens.spacing.s3);
        // One hairline against the editor above.
        ui::surface::separator(
            scene,
            transform,
            self.rect.x0,
            self.rect.x1,
            self.rect.y0,
            0.0,
            colors.surfaces.content,
        );

        // Header: the title, then what the list holds.
        let header = self.header_rect();
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        let title_w = ctx
            .measure_role("Problems", TextRole::Subheading, None)
            .width;
        ctx.text_role_in_band(
            "Problems",
            TextRole::Subheading,
            theme.text_primary,
            header.x0 + gutter,
            header,
            None,
        );
        let summary = self.summary();
        if !summary.is_empty() {
            ctx.text_role_in_band(
                &summary,
                TextRole::Caption,
                theme.text_dim,
                header.x0 + gutter + title_w + gutter,
                header,
                None,
            );
        }

        let list = self.list_rect();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &list);
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);

        if self.diags.is_empty() {
            let band = Rect::new(list.x0, list.y0, list.x1, list.y0 + self.empty_h);
            ctx.text_role_in_band(
                "The document compiled without problems.",
                TextRole::Body,
                theme.text_dim,
                band.x0 + gutter,
                band,
                Some(ui::num::f32_of_f64((band.width() - 2.0 * gutter).max(1.0))),
            );
            scene.pop_layer();
            return;
        }

        let mut y = list.y0 - self.scroll_y;
        for (i, d) in self.diags.iter().enumerate() {
            let h = self.heights.get(i).copied().unwrap_or(0.0);
            if y + h >= list.y0 && y <= list.y1 {
                let meta = location(d);
                let mut face = self.face(d).interaction(Interaction {
                    hovered: Some(i) == self.hover,
                    ..Interaction::default()
                });
                if let Some(meta) = meta.as_deref() {
                    face = face.meta(meta);
                }
                if i + 1 < self.diags.len() {
                    face = face.separator(false);
                }
                face.paint(&mut ctx, Rect::new(list.x0, y, list.x1, y + h));
            }
            y += h;
        }

        scene.pop_layer();
    }
}
