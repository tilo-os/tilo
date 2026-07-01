//! A GeoGebra-class 2D math input field, built locally for the calculator's
//! Graph 2D / Graph 3D function editors.
//!
//! This replaces the old "type into a `String`, draw a literal `|`" editors
//! with a real structured editor: the formula is a tree of math atoms
//! ([`model`]), the caret is a genuine insertion point in that tree
//! ([`edit`]), it lays out as stacked 2D notation ([`layout`]) and renders a
//! real caret bar + selection band into the Vello scene ([`render`]).
//!
//! The field serializes to the flat ASCII string the existing
//! [`crate::engine`] parses, so graphing keeps working unchanged, and the
//! caller persists that string in the CRDT exactly as before.

pub mod edit;
pub mod layout;
pub mod model;
pub mod render;

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::Modifiers;
use winit::keyboard::{Key as WKey, NamedKey};

use edit::Editor;
use layout::Layout;

/// A controller wrapping one editable formula. The graph screens own one of
/// these per function row and feed it the linear string from the model.
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
        }
    }

    pub fn with_size(mut self, size: f32) -> Self {
        self.base_size = size;
        self
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

    /// The current content as a linear string the engine parses. The editor
    /// stores `·` for multiplication (GeoGebra style); the engine wants `*`.
    pub fn linear(&self) -> String {
        engine_linear(&self.editor.to_linear())
    }

    pub fn is_empty(&self) -> bool {
        self.editor.expr.is_empty()
    }

    /// Park the caret inside the first 2D structure (fraction numerator,
    /// power exponent, or radicand) so a static frame shows the caret living
    /// *inside* a structure rather than at the row's end. Used to seed the
    /// `--shots` view; harmless for live use (the first real key resets it).
    pub fn park_caret_in_structure(&mut self) {
        self.editor.park_in_first_structure();
    }

    /// Park a representative selection (a span of atoms in the first
    /// structure, else the whole root row) so a static frame can show the
    /// drag-selection highlight band. Seed-only; cleared by the first key.
    pub fn park_demo_selection(&mut self) {
        self.editor.park_demo_selection();
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
        let sig = (self.editor.to_linear(), self.base_size);
        if self.layout.is_some() && self.layout_sig.as_ref() == Some(&sig) {
            return;
        }
        self.layout = Some(layout::layout(&self.editor.expr.root, tr, self.base_size));
        self.layout_sig = Some(sig);
    }

    /// Origin (top-left) where content is drawn: vertically centered in rect,
    /// left-padded a little.
    fn content_origin(&self) -> Point {
        let h = self.layout.as_ref().map(|l| l.height).unwrap_or(0.0);
        let pad_x = 4.0;
        let y = self.rect.y0 + (self.rect.height() - h) / 2.0;
        Point::new(self.rect.x0 + pad_x, y)
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
        self.relayout(tr);
        let layout = self.layout.as_ref().unwrap();
        let origin = self.content_origin();
        let steps = &self.editor.caret.steps;
        let caret = render::caret_gap(layout, steps, self.editor.caret.offset);
        let sel = self.editor.selection();
        let sel_gaps = render::row_gaps(layout, steps);
        let sel_gaps_ref: Vec<(usize, &layout::Gap)> = sel_gaps;
        render::draw_field(
            scene,
            t,
            tr,
            theme,
            layout,
            origin,
            caret,
            sel.as_ref(),
            &sel_gaps_ref,
            text_color,
            focused,
        );
    }

    /// The engine-ready linear form after an edit, recording it as the new
    /// source so a subsequent [`set_source`] with the same value is a no-op.
    pub fn current_linear(&mut self) -> String {
        let s = engine_linear(&self.editor.to_linear());
        self.source = s.clone();
        s
    }

    // -- input ---------------------------------------------------------------

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
            WKey::Character(s) if ctrl => {
                match s.as_str() {
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
                            let cleaned: String =
                                paste.chars().filter(|c| !c.is_control()).collect();
                            if !cleaned.is_empty() {
                                self.editor.paste_linear(&cleaned);
                                return true;
                            }
                        }
                        false
                    }
                    _ => false,
                }
            }
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
    /// last [`draw`]/[`set_rect`]. Places the caret at the nearest gap, or
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

    pub fn contains(&self, p: Point) -> bool {
        self.rect.contains(p)
    }

    /// Width the laid-out content needs (so callers can size a row).
    pub fn content_width(&self) -> f64 {
        self.layout.as_ref().map(|l| l.width).unwrap_or(0.0)
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
        || "+-*/^(),. %!·π√".contains(ch)
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
