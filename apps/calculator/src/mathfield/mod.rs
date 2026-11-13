//! A 2D math input field for the calculator's display and the graph formula
//! editors: the formula is a tree of atoms ([`model`]), the caret an insertion
//! point in it ([`edit`]), laid out as stacked notation ([`layout`]) and drawn
//! into the Vello scene ([`render`]). It serializes to the flat ASCII string
//! [`crate::engine`] parses.

pub mod edit;
pub mod layout;
pub mod model;
pub mod render;

use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::Modifiers;
use winit::keyboard::{Key as WKey, NamedKey};

use edit::Editor;
use layout::Layout;
use model::{Node, Row};

/// The `Title` role's resolved size. A role already carries the reader's text
/// and UI scale, and the math layout paints at exactly the size it is given, so
/// never divide by the renderer's text scale.
pub fn formula_size(theme: &Theme) -> f32 {
    theme.tokens.typography.size(ui::tokens::TextRole::Title)
}

/// One editable formula. Calculator mode and the graph screens feed it the
/// model's linear string; History uses the same layout without a caret.
pub struct MathField {
    editor: Editor,
    /// The string the editor was last built from, so an external change is
    /// detected and a local edit is not clobbered.
    source: String,
    rect: Rect,
    layout: Option<Layout>,
    /// `(linear form, base size)` the cached [`Self::layout`] was built from.
    /// Layout is content-local, so it is rebuilt only when the content or the
    /// size changes, not on a pan, hover or caret move.
    layout_sig: Option<(String, f32)>,
    base_size: f32,
    dragging: bool,
    /// Content-local pixels scrolled off the top-left, so a long or tall
    /// formula scrolls within the fixed rect. Moved to keep the caret in view,
    /// and by the wheel.
    scroll: Vec2,
    /// The focused field keeps its caret in view; an unfocused one only clamps
    /// its scroll, from [`MathField::draw`]'s `focused`.
    follows_caret: bool,
}

impl MathField {
    pub fn new(linear: &str) -> Self {
        MathField {
            editor: Editor::from_linear(&editor_linear(linear)),
            source: linear.to_string(),
            rect: Rect::ZERO,
            layout: None,
            layout_sig: None,
            base_size: layout::BASE_SIZE,
            dragging: false,
            scroll: Vec2::ZERO,
            follows_caret: true,
        }
    }

    #[must_use]
    pub fn with_size(mut self, size: f32) -> Self {
        self.base_size = size;
        self
    }

    /// The theme's resolved size; changing it invalidates the cached glyphs
    /// through the layout signature.
    pub fn set_size(&mut self, size: f32) {
        self.base_size = size;
    }

    /// Rebuilds only when the string differs from what this field last
    /// produced, so it never stomps an edit in progress. The stored form is the
    /// engine's (`*`); the editor uses `·` internally.
    pub fn set_source(&mut self, linear: &str) {
        if linear != self.source {
            self.editor = Editor::from_linear(&editor_linear(linear));
            self.source = linear.to_string();
        }
    }

    /// The caller feeds the current formula in through
    /// [`MathField::set_source`] every pass, so a read-only field tracks a value
    /// that changes elsewhere.
    #[must_use]
    pub fn follow_source(mut self) -> Self {
        self.source.clear();
        self
    }

    /// As the engine parses it: the editor stores `·` for multiplication, the
    /// engine wants `*`.
    pub fn linear(&self) -> String {
        engine_linear(&self.editor.to_linear())
    }

    pub fn is_empty(&self) -> bool {
        self.editor.expr.is_empty()
    }

    /// A field that keeps its caret in view then scrolls to the tail.
    pub fn park_caret_at_end(&mut self) {
        self.editor.end(false);
    }

    /// Sets the content rect and (re)builds the layout.
    pub fn set_rect(&mut self, rect: Rect, tr: &mut TextRenderer) {
        self.rect = rect;
        self.relayout(tr);
    }

    fn relayout(&mut self, tr: &mut TextRenderer) {
        // The linear form is a faithful signature of the tree and far cheaper
        // than a full measure pass.
        let size = self.base_size;
        let sig = (self.editor.to_linear(), size);
        if self.layout.is_none() || self.layout_sig.as_ref() != Some(&sig) {
            let shown = display_row(&self.editor.expr.root);
            self.layout = Some(layout::layout(&shown, tr, size));
            self.layout_sig = Some(sig);
        }
        // A caret move does not change the signature but must still bring the
        // caret into view; an inactive row keeps the user's scroll.
        if self.follows_caret {
            self.ensure_caret_visible();
        } else {
            self.clamp_scroll();
        }
    }

    fn clamp_scroll(&mut self) {
        let (max_x, max_y) = self.scroll_bounds();
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);
    }

    const PAD_X: f64 = ui::tokens::Spacing::BASE.s1 as f64;

    /// Left-padded and scrolled horizontally; centred when it fits, top-aligned
    /// and scrolled when taller, so the field never grows with the formula.
    fn content_origin(&self) -> Point {
        let h = self.layout.as_ref().map_or(0.0, |l| l.height);
        let view_h = self.rect.height();
        let y = if h <= view_h {
            self.rect.y0 + (view_h - h) / 2.0
        } else {
            self.rect.y0 - self.scroll.y
        };
        Point::new(self.rect.x0 + Self::PAD_X - self.scroll.x, y)
    }

    fn view_w(&self) -> f64 {
        (self.rect.width() - Self::PAD_X).max(1.0)
    }

    fn scroll_bounds(&self) -> (f64, f64) {
        match self.layout.as_ref() {
            Some(l) => (
                (l.width - self.view_w()).max(0.0),
                (l.height - self.rect.height()).max(0.0),
            ),
            None => (0.0, 0.0),
        }
    }

    /// Called every layout pass.
    fn ensure_caret_visible(&mut self) {
        let caret = {
            let Some(layout) = self.layout.as_ref() else {
                return;
            };
            render::caret_gap(layout, &self.editor.caret.steps, self.editor.caret.offset)
                .map(|g| (g.x, g.top, g.bottom))
        };
        let view_w = self.view_w();
        let view_h = self.rect.height();
        let content_h = self.layout.as_ref().map_or(0.0, |l| l.height);
        let margin = 8.0f64;
        if let Some((cx, ctop, cbot)) = caret {
            if cx < self.scroll.x + margin {
                self.scroll.x = cx - margin;
            } else if cx > self.scroll.x + view_w - margin {
                self.scroll.x = cx - view_w + margin;
            }
            if content_h > view_h {
                if ctop < self.scroll.y {
                    self.scroll.y = ctop;
                } else if cbot > self.scroll.y + view_h {
                    self.scroll.y = cbot - view_h;
                }
            }
        }
        let (max_x, max_y) = self.scroll_bounds();
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);
    }

    /// Clamped to the content extent.
    pub fn scroll_by(&mut self, dx: f64, dy: f64) {
        let (max_x, max_y) = self.scroll_bounds();
        self.scroll.x = (self.scroll.x + dx).clamp(0.0, max_x);
        self.scroll.y = (self.scroll.y + dy).clamp(0.0, max_y);
    }

    /// The content is scrolled rather than fully visible.
    pub fn overflows(&self) -> bool {
        let (mx, my) = self.scroll_bounds();
        mx > 0.0 || my > 0.0
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        text_color: Color,
        focused: bool,
    ) {
        if self.follows_caret != focused {
            self.follows_caret = focused;
            if !focused {
                // A row that just lost focus reads from its start again, so a
                // long neighbouring formula is never shown as a clipped tail.
                self.scroll = Vec2::ZERO;
            }
        }
        self.relayout(tr);
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let origin = self.content_origin();
        let steps = &self.editor.caret.steps;
        let caret = render::caret_gap(layout, steps, self.editor.caret.offset);
        let sel = self.editor.selection();
        let sel_gaps = render::row_gaps(layout, steps);
        let sel_gaps_ref: Vec<(usize, &layout::Gap)> = sel_gaps;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.rect);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if let Some(sel) = sel.as_ref() {
            render::draw_selection(&mut ctx, origin, sel, &sel_gaps_ref, &self.editor.expr);
        }
        ui::math::draw_layout(ctx.scene, t, ctx.text, layout, origin, text_color);
        if focused && let Some(g) = caret {
            render::draw_caret(&mut ctx, origin, g);
        }
        // A formula wider than its field fades out where it runs past an edge.
        let (max_x, _) = self.scroll_bounds();
        let ground = theme.tokens.colors.surfaces.content;
        if self.scroll.x > 0.5 {
            ui::surface::content_edge_fade(
                ctx.scene,
                t,
                self.rect,
                ui::surface::ContentEdge::Leading,
                ground,
            );
        }
        if self.scroll.x < max_x - 0.5 {
            ui::surface::content_edge_fade(
                ctx.scene,
                t,
                self.rect,
                ui::surface::ContentEdge::Trailing,
                ground,
            );
        }
        scene.pop_layer();
    }

    /// Maps a peer's scene-space cursor onto the glyph layout, so its caret is
    /// drawn inside the formula at the nearest gap, not as a pointer over the
    /// window.
    pub fn remote_caret_bars(&self, cursors: &[(Point, Color)]) -> Vec<(Rect, Color)> {
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };
        let origin = self.content_origin();
        let mut out = Vec::new();
        for (p, col) in cursors {
            if !self.rect.contains(*p) {
                continue;
            }
            let lx = p.x - origin.x;
            let ly = p.y - origin.y;
            if let Some(g) = layout::nearest_gap(&layout.gaps, lx, ly) {
                let x = origin.x + g.x;
                let bar = Rect::new(x - 1.0, origin.y + g.top, x + 1.0, origin.y + g.bottom);
                out.push((bar, *col));
            }
        }
        out
    }

    pub fn draw_remote_carets(&self, scene: &mut Scene, t: Affine, cursors: &[(Point, Color)]) {
        let bars = self.remote_caret_bars(cursors);
        if bars.is_empty() {
            return;
        }
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.rect);
        for (bar, col) in bars {
            ui::paint::fill_rect(scene, t, bar, col);
        }
        scene.pop_layer();
    }

    /// Whether the caret holds a selection, so Ctrl+C copies that rather than
    /// the calculator's displayed result.
    pub fn has_selection(&self) -> bool {
        self.editor.selected_linear().is_some()
    }

    /// The engine-ready linear form after an edit, recorded as the new source so
    /// a later `set_source` with the same value is a no-op.
    pub fn current_linear(&mut self) -> String {
        let s = engine_linear(&self.editor.to_linear());
        self.source.clone_from(&s);
        s
    }

    /// `true` when it changed the content, so the caller persists and
    /// re-plots; a caret-only move returns `false`.
    pub fn handle_key(&mut self, key: &WKey, modifiers: &Modifiers) -> bool {
        let m = modifiers.state();
        let shift = m.shift_key();
        let ctrl = m.control_key() || m.super_key();
        match key {
            WKey::Named(NamedKey::ArrowLeft) => {
                if ctrl {
                    self.editor.move_word_left(shift);
                } else {
                    self.editor.move_left(shift);
                }
                false
            }
            WKey::Named(NamedKey::ArrowRight) => {
                if ctrl {
                    self.editor.move_word_right(shift);
                } else {
                    self.editor.move_right(shift);
                }
                false
            }
            WKey::Named(NamedKey::ArrowUp) => {
                self.editor.move_up();
                false
            }
            WKey::Named(NamedKey::ArrowDown) => {
                self.editor.move_down();
                false
            }
            WKey::Named(NamedKey::Home) => {
                self.editor.home(shift);
                false
            }
            WKey::Named(NamedKey::End) => {
                self.editor.end(shift);
                false
            }
            WKey::Named(NamedKey::Backspace) => {
                self.editor.backspace();
                true
            }
            WKey::Named(NamedKey::Delete) => {
                self.editor.delete();
                true
            }
            WKey::Named(NamedKey::Space) => {
                self.editor.move_right(false);
                false
            }
            WKey::Character(s) if ctrl => match s.as_str() {
                "a" | "A" => {
                    self.editor.select_all();
                    false
                }
                "c" | "C" => {
                    if let Some(lin) = self.editor.selected_linear() {
                        ui::clipboard_copy(&lin);
                    }
                    false
                }
                "x" | "X" => {
                    if let Some(lin) = self.editor.selected_linear() {
                        ui::clipboard_copy(&lin);
                        self.editor.delete_selection();
                        return true;
                    }
                    false
                }
                "v" | "V" => {
                    if let Some(paste) = ui::clipboard_paste() {
                        let cleaned: String = paste.chars().filter(|c| !c.is_control()).collect();
                        if !cleaned.is_empty() {
                            self.editor.paste_linear(&cleaned);
                            return true;
                        }
                    }
                    false
                }
                _ => false,
            },
            WKey::Character(s) => {
                let mut changed = false;
                for ch in s.chars() {
                    if accept(ch) {
                        self.editor.type_char(normalize(ch));
                        changed = true;
                    }
                }
                changed
            }
            _ => false,
        }
    }

    /// Places the caret at the nearest gap against the cached layout, or
    /// selects a token on a double click; a no-op before the first layout.
    pub fn on_press_cached(&mut self, p: Point, double: bool) {
        let origin = self.content_origin();
        let lx = p.x - origin.x;
        let ly = p.y - origin.y;
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        if let Some(g) = layout::nearest_gap(&layout.gaps, lx, ly) {
            let steps = g.steps.clone();
            let index = g.index;
            if double {
                self.editor.select_token_at(steps, index);
            } else {
                self.editor.caret.steps = steps;
                self.editor.caret.offset = index;
                self.editor.anchor = None;
                self.dragging = true;
            }
        }
    }

    /// Extends the selection to the nearest gap in the caret's row.
    pub fn on_drag_cached(&mut self, p: Point) {
        if !self.dragging {
            return;
        }
        let origin = self.content_origin();
        let lx = p.x - origin.x;
        let ly = p.y - origin.y;
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let steps = self.editor.caret.steps.clone();
        let nearest = layout
            .gaps
            .iter()
            .filter(|g| g.steps == steps)
            .min_by(|a, b| {
                let da = gap_cost(a, lx, ly);
                let db = gap_cost(b, lx, ly);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(g) = nearest {
            if self.editor.anchor.is_none() {
                self.editor.anchor = Some(self.editor.caret.offset);
            }
            self.editor.caret.offset = g.index;
        }
    }

    pub fn on_release(&mut self) {
        self.dragging = false;
    }

    /// Height the content needs, rebuilding the layout if the content changed.
    /// Callers grow the hosting card to it before handing the field its rect.
    pub fn content_height(&mut self, tr: &mut TextRenderer) -> f64 {
        self.relayout(tr);
        self.layout.as_ref().map_or(0.0, |l| l.height)
    }
}

/// Smallest inner height a formula card uses: where a single-line formula sits.
pub const CARD_MIN_H: f64 = ui::tokens::Metrics::BASE.min_pointer_target as f64;

/// Largest inner height a card grows to: a single fraction or a stacked root
/// shows in full, and only a fraction over a fraction scrolls within it.
pub const CARD_MAX_H: f64 = 2.0 * ui::tokens::Metrics::BASE.panel_row_height as f64;

/// Grows to fit, clamped to `[CARD_MIN_H, CARD_MAX_H]`; past the maximum the
/// field reports [`MathField::overflows`] and scrolls.
pub fn card_view_height(content_h: f64) -> f64 {
    content_h.clamp(CARD_MIN_H, CARD_MAX_H)
}

/// The engine's ASCII hyphen reads as the true minus sign. Only the characters
/// change, so the tree and every caret path stay the editor's own.
fn display_row(row: &Row) -> Row {
    row.iter().map(display_node).collect()
}

fn display_node(node: &Node) -> Node {
    match node {
        Node::Char('-') => Node::Char('−'),
        Node::Char(c) => Node::Char(*c),
        Node::Frac { num, den } => Node::Frac {
            num: display_row(num),
            den: display_row(den),
        },
        Node::Sup { exp } => Node::Sup {
            exp: display_row(exp),
        },
        Node::Sub { sub } => Node::Sub {
            sub: display_row(sub),
        },
        Node::SubSup { sub, sup } => Node::SubSup {
            sub: display_row(sub),
            sup: display_row(sup),
        },
        Node::Sqrt { index, radicand } => Node::Sqrt {
            index: index.as_ref().map(display_row),
            radicand: display_row(radicand),
        },
        Node::Paren { body } => Node::Paren {
            body: display_row(body),
        },
        Node::Delimited { open, close, body } => Node::Delimited {
            open: *open,
            close: *close,
            body: display_row(body),
        },
        Node::Func { name, arg } => Node::Func {
            name: name.clone(),
            arg: display_row(arg),
        },
        Node::Text(text) => Node::Text(text.clone()),
        Node::Accent { base, mark, over } => Node::Accent {
            base: display_row(base),
            mark: *mark,
            over: *over,
        },
        Node::UnderOver { base, under, over } => Node::UnderOver {
            base: display_row(base),
            under: under.as_ref().map(display_row),
            over: over.as_ref().map(display_row),
        },
        Node::Table { rows: cells } => Node::Table {
            rows: cells
                .iter()
                .map(|row| row.iter().map(display_row).collect())
                .collect(),
        },
    }
}

fn engine_linear(s: &str) -> String {
    s.replace('·', "*")
}

/// Engine `*` → editor `·`, so a loaded formula shows the multiplication
/// glyph.
fn editor_linear(s: &str) -> String {
    s.replace('*', "·")
}

fn gap_cost(g: &layout::Gap, px: f64, py: f64) -> f64 {
    let dy = if py < g.top {
        g.top - py
    } else if py > g.bottom {
        py - g.bottom
    } else {
        0.0
    };
    dy * 4.0 + (g.x - px).abs()
}

fn accept(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || "+-*/^(),. %!·π√∫".contains(ch)
        || ch == '×'
        || ch == '÷'
        || ch == '−'
}

fn normalize(ch: char) -> char {
    match ch {
        '×' => '·',
        '÷' => '/',
        '−' => '-',
        'A'..='Z' => ch.to_ascii_lowercase(),
        other => other,
    }
}

#[cfg(test)]
mod tests;
