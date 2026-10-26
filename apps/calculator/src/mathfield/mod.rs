//! A GeoGebra-class 2D math input field, built locally for the calculator's
//! Calculator display and Graph 2D / Graph 3D function editors.
//!
//! The formula is a tree of math atoms ([`model`]), the caret is an insertion
//! point in that tree ([`edit`]), it lays out as stacked 2D notation
//! ([`layout`]) and renders a caret bar + selection band into the Vello scene
//! ([`render`]).
//!
//! The field serializes to the flat ASCII string the existing
//! [`crate::engine`] parses, so graphing keeps working unchanged, and the
//! caller persists that string in the CRDT exactly as before.

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

/// The size a formula is set at in `theme`: the `Title` role's resolved size.
///
/// A theme role is already resolved against the reader's text and UI scale, and
/// the math layout paints at exactly the size it is given, so this is the size
/// the formula actually appears at — never divided by, or multiplied by, the
/// renderer's text scale.
pub fn formula_size(theme: &Theme) -> f32 {
    theme.tokens.typography.size(ui::tokens::TextRole::Title)
}

/// A controller wrapping one editable formula. Calculator mode and the graph
/// screens feed it the linear string from the model; the history tape uses
/// the same layout without a caret.
pub struct MathField {
    editor: Editor,
    /// Source linear string the editor was last built from; used to detect
    /// external (CRDT) changes and avoid clobbering local edits.
    source: String,
    /// On-screen content rect (set each layout pass).
    rect: Rect,
    /// Cached layout from the last draw, in content-local coords.
    layout: Option<Layout>,
    /// Signature `(linear form, base size)` the cached [`Self::layout`] was
    /// built from. Layout is rect-independent (content-local coords), so it
    /// only needs rebuilding when the formula content or the font size
    /// changes — not on every frame, pan, hover, or caret move. This is what
    /// keeps typing from re-shaping the whole formula each frame.
    layout_sig: Option<(String, f32)>,
    base_size: f32,
    /// Whether a drag-select is in progress.
    dragging: bool,
    /// Content-local pixels scrolled off the top-left. Keeps a long or tall
    /// formula scrolling *within* the fixed field rect instead of overflowing
    /// it (which used to make the formula appear to resize the row). Updated to
    /// keep the caret in view while typing, and by the mouse wheel.
    scroll: Vec2,
    /// Whether this field is the focused one and so keeps its caret in view
    /// on every layout pass. Tracked from `draw`'s `focused` flag; on the
    /// frame focus is lost the scroll snaps back to the formula's start, and
    /// an unfocused field then only clamps its scroll (so wheel scrolling an
    /// inactive row still sticks) instead of chasing a parked caret.
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

    /// Set the size the formula is set at, resolved from the theme (which
    /// already carries the text scale). Changing it invalidates the cached
    /// glyph geometry via the effective-size layout signature below.
    pub fn set_size(&mut self, size: f32) {
        self.base_size = size;
    }

    /// Sync the editor to an external linear string (e.g. loaded from the
    /// CRDT). Only rebuilds when the string actually differs from what this
    /// field last produced, so it never stomps an in-progress edit. The
    /// stored form is the engine form (`*`); the editor uses `·` internally.
    pub fn set_source(&mut self, linear: &str) {
        if linear != self.source {
            self.editor = Editor::from_linear(&editor_linear(linear));
            self.source = linear.to_string();
        }
    }

    /// Leave the editor showing `linear` (the default, [`MathField::new`]): the
    /// caller feeds the current formula in through
    /// [`MathField::set_source`] on every pass, so a read-only field tracks a
    /// value that changes elsewhere instead of holding the first one it saw.
    #[must_use]
    pub fn follow_source(mut self) -> Self {
        self.source.clear();
        self
    }

    /// The current content as a linear string the engine parses. The editor
    /// stores `·` for multiplication (`GeoGebra` style); the engine wants `*`.
    pub fn linear(&self) -> String {
        engine_linear(&self.editor.to_linear())
    }

    pub fn is_empty(&self) -> bool {
        self.editor.expr.is_empty()
    }

    /// Park the caret at the very end of the formula, so a field that keeps
    /// its caret in view scrolls to the formula's tail.
    pub fn park_caret_at_end(&mut self) {
        self.editor.end(false);
    }

    /// Set the content rect for this frame and (re)build the layout.
    pub fn set_rect(&mut self, rect: Rect, tr: &mut TextRenderer) {
        self.rect = rect;
        self.relayout(tr);
    }

    fn relayout(&mut self, tr: &mut TextRenderer) {
        // Rebuild only when the content or size actually changed. The linear
        // form is a faithful signature of the (canonical) tree the editor
        // builds, and is far cheaper to compute than a full measure+box pass.
        let size = self.base_size;
        let sig = (self.editor.to_linear(), size);
        if self.layout.is_none() || self.layout_sig.as_ref() != Some(&sig) {
            let shown = display_row(&self.editor.expr.root);
            self.layout = Some(layout::layout(&shown, tr, size));
            self.layout_sig = Some(sig);
        }
        // Track the caret every pass (a caret move doesn't change the content
        // signature, but must still bring the caret back into view) — but only
        // in the focused field; an inactive row keeps whatever the user
        // scrolled it to, clamped to its content.
        if self.follows_caret {
            self.ensure_caret_visible();
        } else {
            self.clamp_scroll();
        }
    }

    /// Keep both scroll axes inside the content extent.
    fn clamp_scroll(&mut self) {
        let (max_x, max_y) = self.scroll_bounds();
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);
    }

    /// Left padding between the field edge and the content.
    const PAD_X: f64 = ui::tokens::Spacing::BASE.s1 as f64;

    /// Origin (top-left) where content is drawn. Horizontally the content is
    /// left-padded and shifted by the horizontal scroll; vertically it is
    /// centered when it fits and top-aligned + scrolled when it is taller than
    /// the field, so the field never grows with the formula.
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

    /// Visible content width (field width minus the left pad).
    fn view_w(&self) -> f64 {
        (self.rect.width() - Self::PAD_X).max(1.0)
    }

    /// Maximum scroll offsets for the current content + field size.
    fn scroll_bounds(&self) -> (f64, f64) {
        match self.layout.as_ref() {
            Some(l) => (
                (l.width - self.view_w()).max(0.0),
                (l.height - self.rect.height()).max(0.0),
            ),
            None => (0.0, 0.0),
        }
    }

    /// Nudge the scroll so the caret stays inside the field, then clamp both
    /// axes to the content. Called every layout pass.
    fn ensure_caret_visible(&mut self) {
        // Read what we need out of the (immutably borrowed) layout first, so we
        // can mutate `self.scroll` afterwards without a borrow conflict.
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

    /// Scroll the content by `(dx, dy)` content pixels (e.g. from the wheel),
    /// clamped to the content extent.
    pub fn scroll_by(&mut self, dx: f64, dy: f64) {
        let (max_x, max_y) = self.scroll_bounds();
        self.scroll.x = (self.scroll.x + dx).clamp(0.0, max_x);
        self.scroll.y = (self.scroll.y + dy).clamp(0.0, max_y);
    }

    /// Whether the content currently overflows the field on either axis (so it
    /// is scrolled rather than fully visible).
    pub fn overflows(&self) -> bool {
        let (mx, my) = self.scroll_bounds();
        mx > 0.0 || my > 0.0
    }

    /// Draw the field. `focused` controls caret visibility.
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
                // Only the focused field follows its caret. A row that just
                // lost focus reads from its start again, so a long neighbouring
                // formula is never shown as a left-clipped tail
                // (`)·05 · (x² + y²) − 3`).
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
        // Clip to the field rect so a long/tall formula scrolls within a
        // bounded box instead of spilling over neighbouring rows.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.rect);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if let Some(sel) = sel.as_ref() {
            render::draw_selection(&mut ctx, origin, sel, &sel_gaps_ref, &self.editor.expr);
        }
        ui::math::draw_layout(ctx.scene, t, ctx.text, layout, origin, text_color);
        if focused && let Some(g) = caret {
            render::draw_caret(&mut ctx, origin, g);
        }
        // A formula wider than its field says so: it fades out where it runs
        // on past an edge instead of being cut off.
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

    /// Map remote collaborators' scene-space cursor positions onto this field's
    /// 2D glyph layout, returning a caret-bar rect + colour for each cursor that
    /// lands within the field. This is what turns a peer's `RemoteCursorMoved`
    /// event into a caret drawn *inside* the structured formula (snapped to the
    /// nearest insertion gap), rather than only a floating pointer over the
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

    /// Draw remote collaborators' carets into this field (clipped to its rect),
    /// each in the peer's colour. Call after [`MathField::draw`].
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

    /// The engine-ready linear form after an edit, recording it as the new
    /// source so a subsequent `set_source` with the same value is a no-op.
    pub fn current_linear(&mut self) -> String {
        let s = engine_linear(&self.editor.to_linear());
        self.source.clone_from(&s);
        s
    }

    /// Handle a key press. Returns `true` if it changed the content (so the
    /// caller can persist + re-plot). Caret-only moves return `false`.
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
                // Space exits the current box (GeoGebra) when inside one,
                // else is ignored (expressions have no literal spaces).
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

    /// Press at scene point `p`, hit-testing against the layout cached by the
    /// last [`draw`](crate::draw)/`set_rect`. Places the caret at the nearest gap, or
    /// selects a token on a double click. No-op if no layout is cached yet.
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

    /// Drag to scene point `p` while the button is held: extend the selection
    /// to the nearest gap in the caret's current row, against the cached
    /// layout.
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

    /// Height the laid-out content needs, measured against `tr` (rebuilding the
    /// layout if the content changed). Callers use this to GROW the hosting card
    /// to fit the formula up to [`CARD_MAX_H`] before handing the field its rect,
    /// so a taller formula makes its card taller instead of being clipped.
    pub fn content_height(&mut self, tr: &mut TextRenderer) -> f64 {
        self.relayout(tr);
        self.layout.as_ref().map_or(0.0, |l| l.height)
    }
}

/// Smallest inner (content-viewport) height a formula card uses. A short,
/// single-line formula (`x + 1`, `sin(x)`) sits in a card of this height so the
/// common case looks unchanged.
pub const CARD_MIN_H: f64 = ui::tokens::Metrics::BASE.min_pointer_target as f64;

/// Largest inner height a formula card grows to. It is generous on purpose: a
/// single fraction (`(x²+1)/(x−3)` ≈ 67 px) and a stacked `√` over superscripts
/// (the 3D default `sin(√(x²+y²))` ≈ 39 px) are shown *in full* — the card grows
/// to fit them rather than clipping. Only genuinely oversized content (a
/// fraction-over-fraction and deeper nesting) exceeds this height, at which point
/// the card stops growing and the formula becomes vertically scrollable within
/// it — never clipped behind the card edge.
pub const CARD_MAX_H: f64 = 2.0 * ui::tokens::Metrics::BASE.panel_row_height as f64;

/// The inner (content-viewport) height a card should use for a formula whose
/// laid-out content is `content_h` tall: grow to fit, clamped to
/// `[CARD_MIN_H, CARD_MAX_H]`. When `content_h` exceeds [`CARD_MAX_H`] the
/// returned height is the max, and the field will report [`MathField::overflows`]
/// so the caller knows the formula scrolls within the card.
pub fn card_view_height(content_h: f64) -> f64 {
    content_h.clamp(CARD_MIN_H, CARD_MAX_H)
}

/// The formula as it is drawn: the engine's ASCII hyphen reads as the true
/// minus sign. Only the characters change, so the tree (and with it every
/// caret path and gap) is the editor's own.
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

/// Editor `·` (middle dot) → engine `*`.
fn engine_linear(s: &str) -> String {
    s.replace('·', "*")
}

/// Engine `*` → editor `·`, so a string loaded from the CRDT renders with the
/// proper multiplication glyph.
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

/// Characters the field accepts as input.
fn accept(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || "+-*/^(),. %!·π√∫".contains(ch)
        || ch == '×'
        || ch == '÷'
        || ch == '−'
}

/// Normalize a few unicode inputs to the editor's canonical glyphs.
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
