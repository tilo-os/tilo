//! The response body as a document: line numbers, syntax inks and folds.
//!
//! This is genuine content drawing (a code document the person reads), so it
//! paints its own lines; everything it paints reads the theme: the `Code`
//! role for text, the syntax palette for tokens, `text_dim` for the gutter,
//! and `ui::icons` for the fold carets. The gutter is two columns, line
//! numbers and then fold carets, so a caret never overprints a number. It
//! sizes itself to its lines and lives in a vertical scroll view.

use std::collections::{HashMap, HashSet};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::palette::token_color;
use crate::vars::{self, JsonLine, Token, TokenKind};

/// Resolved geometry of the view at the current theme and text scale.
#[derive(Clone)]
struct Metrics {
    style: TextStyle,
    line_h: f64,
    pad_x: f64,
    pad_y: f64,
    number_w: f64,
    fold_w: f64,
    gap: f64,
}

/// A document of tokenized lines that fold.
pub(crate) struct CodeView {
    name: String,
    lines: Vec<JsonLine>,
    collapsed: HashSet<usize>,
    text: String,
    /// Widths of each token at the current style, measured once.
    widths: Vec<Vec<f64>>,
    metrics: Option<Metrics>,
    measured_for: Option<(u32, u32)>,
    /// The theme of the last layout pass, which `measure` has no argument for.
    theme: Option<Theme>,
    rect: Rect,
    folds: Vec<(Rect, usize)>,
    cursor: Point,
    structure_dirty: bool,
    dirty: bool,
}

impl CodeView {
    /// A JSON document, pretty-printed when it parses.
    pub(crate) fn json(name: &str, body: &str) -> Self {
        let pretty = vars::pretty_json(body).unwrap_or_else(|| body.to_owned());
        Self::new(name, vars::tokenize_json(&pretty), pretty)
    }

    /// Text shown as written, without folds.
    pub(crate) fn plain(name: &str, body: &str) -> Self {
        let lines = body
            .lines()
            .map(|line| JsonLine {
                tokens: vec![Token {
                    text: line.to_owned(),
                    kind: TokenKind::Plain,
                }],
                depth: 0,
                opens_block: false,
                close_line: None,
            })
            .collect();
        Self::new(name, lines, body.to_owned())
    }

    fn new(name: &str, lines: Vec<JsonLine>, text: String) -> Self {
        Self {
            name: name.to_owned(),
            lines,
            collapsed: HashSet::new(),
            text,
            widths: Vec::new(),
            metrics: None,
            measured_for: None,
            theme: None,
            rect: Rect::ZERO,
            folds: Vec::new(),
            cursor: Point::ZERO,
            structure_dirty: false,
            dirty: true,
        }
    }

    fn has_fold(line: &JsonLine) -> bool {
        line.opens_block && line.close_line.is_some()
    }

    /// The lines that are showing, as indices into `lines`.
    fn visible(&self) -> Vec<usize> {
        let mut shown = Vec::with_capacity(self.lines.len());
        let mut hidden_until: Option<usize> = None;
        for (index, line) in self.lines.iter().enumerate() {
            if let Some(close) = hidden_until {
                if index <= close {
                    continue;
                }
                hidden_until = None;
            }
            shown.push(index);
            if line.opens_block
                && self.collapsed.contains(&index)
                && let Some(close) = line.close_line
            {
                hidden_until = Some(close);
            }
        }
        shown
    }

    fn resolve(&mut self, theme: &Theme, text: &mut TextRenderer) -> Metrics {
        let key = (theme.text_scale.to_bits(), theme.ui_scale.to_bits());
        if let Some(metrics) = &self.metrics
            && self.measured_for == Some(key)
        {
            return metrics.clone();
        }
        let style = theme.text(TextRole::Code);
        let (_, line_h) = text.measure_style("Ag", None, &style);
        let digits = self.lines.len().max(1).to_string();
        let (number_w, _) = text.measure_style(&digits, None, &style.clone().numeric());
        let spacing = &theme.tokens.spacing;
        let metrics = Metrics {
            line_h: f64::from(line_h),
            pad_x: f64::from(spacing.s3),
            pad_y: f64::from(spacing.s2),
            number_w: f64::from(number_w),
            fold_w: f64::from(theme.tokens.metrics.icon_sm),
            gap: f64::from(spacing.s2),
            style: style.clone(),
        };
        self.widths = self
            .lines
            .iter()
            .map(|line| {
                line.tokens
                    .iter()
                    .map(|token| f64::from(text.measure_style(&token.text, None, &style).0))
                    .collect()
            })
            .collect();
        self.metrics = Some(metrics.clone());
        self.measured_for = Some(key);
        metrics
    }

    fn toggle(&mut self, line: usize) {
        if !self.collapsed.remove(&line) {
            self.collapsed.insert(line);
        }
        self.structure_dirty = true;
        self.dirty = true;
    }

    fn text_x(m: &Metrics) -> f64 {
        m.pad_x + m.number_w + m.gap + m.fold_w + m.gap
    }
}

impl Widget for CodeView {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let node = taffy.leaf_node(Style {
            size: Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        node
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let Some(theme) = self.theme.clone() else {
            return Size::ZERO;
        };
        let metrics = self.resolve(&theme, text_renderer);
        let rows = self.visible().len() as f64;
        let height = rows * metrics.line_h + 2.0 * metrics.pad_y;
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        Size {
            width,
            height: height as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        self.dirty = true;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let metrics = self.resolve(theme, text_renderer);
        let colors = &theme.tokens.colors;
        let visible = self.visible();
        self.folds.clear();
        let number_style = metrics.style.clone().numeric();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = self.rect.y0 + metrics.pad_y;
        for index in visible {
            let band = Rect::new(self.rect.x0, y, self.rect.x1, y + metrics.line_h);
            y += metrics.line_h;
            if presence.should_cull(band) {
                continue;
            }
            let Some(line) = self.lines.get(index) else {
                continue;
            };
            let number = (index + 1).to_string();
            let (w, _) = ctx.text.measure_style(&number, None, &number_style);
            ctx.text(
                &number,
                &number_style,
                theme.text_tertiary,
                Point::new(
                    self.rect.x0 + metrics.pad_x + metrics.number_w - f64::from(w),
                    band.y0,
                ),
                None,
            );
            let collapsed = self.collapsed.contains(&index);
            if Self::has_fold(line) {
                let side = metrics.fold_w.min(metrics.line_h);
                let x0 = self.rect.x0 + metrics.pad_x + metrics.number_w + metrics.gap;
                let caret = Rect::new(
                    x0,
                    band.center().y - side / 2.0,
                    x0 + metrics.fold_w,
                    band.center().y + side / 2.0,
                );
                let icon = if collapsed {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                };
                icon.draw(ctx.scene, ctx.transform, caret, theme.text_dim);
                self.folds.push((band_for_caret(caret, band), index));
            }
            let mut x = self.rect.x0 + Self::text_x(&metrics);
            let widths = self.widths.get(index);
            for (slot, token) in line.tokens.iter().enumerate() {
                ctx.text(
                    &token.text,
                    &metrics.style,
                    token_color(theme, token.kind),
                    Point::new(x, band.y0),
                    None,
                );
                x += widths.and_then(|w| w.get(slot)).copied().unwrap_or(0.0);
            }
            if collapsed && line.opens_block {
                ctx.text(
                    "\u{2026}",
                    &metrics.style,
                    colors.text_dim,
                    Point::new(x + metrics.gap, band.y0),
                    None,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let cursor = self.cursor;
                if let Some((_, line)) = self.folds.iter().find(|(rect, _)| rect.contains(cursor)) {
                    let line = *line;
                    self.toggle(line);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.folds
            .iter()
            .any(|(rect, _)| rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "CodeView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Document)
                .with_name(self.name.clone())
                .with_value(self.text.clone())
                .with_bounds(self.rect),
        )
    }
}

/// The hit target of a caret is the gutter cell it sits in, a full line tall.
fn band_for_caret(caret: Rect, band: Rect) -> Rect {
    Rect::new(caret.x0 - 2.0, band.y0, caret.x1 + 2.0, band.y1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::headless::FrameHarness;

    fn harness(view: CodeView) -> FrameHarness {
        FrameHarness::for_root(ui::State::new(), view, (600, 400), Theme::dark())
    }

    /// The fold caret and the line number are two columns: the caret never
    /// overprints a number, whatever the document's length.
    #[test]
    fn the_fold_gutter_sits_beside_the_line_numbers() {
        let body = serde_json::json!({ "a": { "b": [1, 2, 3] }, "c": true }).to_string();
        let d = harness(CodeView::json("Body", &body));
        let _ = d.render_rgba();
        let view = d.root::<CodeView>();
        let m = view.metrics.clone().expect("measured");
        let numbers_end = view.rect.x0 + m.pad_x + m.number_w;
        assert!(!view.folds.is_empty(), "the object folds");
        for (rect, _) in &view.folds {
            assert!(
                rect.x0 + 2.0 >= numbers_end,
                "caret starts at {} after the numbers end at {numbers_end}",
                rect.x0 + 2.0
            );
        }
    }

    /// A press on a caret folds its block away, and the next press unfolds it.
    #[test]
    fn pressing_a_caret_folds_and_unfolds_its_block() {
        let body = serde_json::json!({ "a": { "b": [1, 2, 3] }, "c": true }).to_string();
        let mut d = harness(CodeView::json("Body", &body));
        let _ = d.render_rgba();
        let all = d.root::<CodeView>().visible().len();
        let (caret, _) = d.root::<CodeView>().folds[0];
        d.click_at(caret.center().x, caret.center().y);
        let _ = d.render_rgba();
        let folded = d.root::<CodeView>().visible().len();
        assert!(
            folded < all,
            "the block folded away: {folded} of {all} lines"
        );
        let (caret, _) = d.root::<CodeView>().folds[0];
        d.click_at(caret.center().x, caret.center().y);
        let _ = d.render_rgba();
        assert_eq!(d.root::<CodeView>().visible().len(), all);
    }

    /// Plain text has no folds and one numbered line per line of text.
    #[test]
    fn plain_text_is_numbered_without_folds() {
        let d = harness(CodeView::plain("Body", "one\ntwo\nthree"));
        let _ = d.render_rgba();
        let view = d.root::<CodeView>();
        assert!(view.folds.is_empty());
        assert_eq!(view.visible().len(), 3);
    }
}
