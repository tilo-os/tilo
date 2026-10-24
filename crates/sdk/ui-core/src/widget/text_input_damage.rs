//! What a shaped multiline field repaints. A frame that only scrolled it
//! offers its host a copy of what stays on screen ([`crate::scroll_copy`]);
//! one that changed some of its lines in place repaints those lines and
//! where its caret, selection and marks were and are, not the field.

use std::hash::{Hash, Hasher};

use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Color;

use super::TextInput;
use crate::scroll_copy::{Piece, Showing, Since};
use crate::theme::Theme;

/// How one frame showed a shaped multiline field: its lines and marks
/// (caret, current line, selection, a host's diagnostics) as pieces.
pub(super) type FieldShown = Showing<FieldLook>;

/// What the whole field's paint depends on besides its lines and marks.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FieldLook {
    states: [bool; 6],
    left: u64,
    radius: u64,
    colors: [Color; 6],
}

/// The keys of marks among the pieces, after every line's.
const MARK: u64 = 1 << 63;

fn fingerprint(value: impl Hash) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

fn mark(index: usize, rect: Rect) -> Piece {
    Piece {
        key: MARK | u64::try_from(index).unwrap_or(u64::MAX >> 1),
        rect,
        paint: fingerprint([rect.x0, rect.y0, rect.x1, rect.y1].map(f64::to_bits)),
    }
}

impl TextInput {
    /// Note what this frame shows of a shaped multiline field, once its
    /// draw is done.
    pub(super) fn note_shown(&self, transform: Affine, theme: &Theme) {
        // What this model does not follow (a placeholder, a composition,
        // spelling, an autocorrection, styled spans, an accessory) leaves
        // the frame unnoted: the field repaints whole.
        let untracked = self.shows_placeholder()
            || self.core.has_preedit()
            || self.rich_spans.is_some()
            || self.accessory.is_some()
            || !self.spell_spans.is_empty()
            || self.last_autocorrection.is_some()
            || self.autocorrection_preview.is_some();
        let Some(layout) = self
            .single_layout
            .as_ref()
            .filter(|_| self.multiline_shaped && !untracked)
        else {
            return;
        };
        let origin_y = self.single_origin.y;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "on-screen offsets in logical pixels are far inside f32"
        )]
        let (top, bottom) = (
            (self.rect.y0 - origin_y) as f32,
            (self.rect.y1 - origin_y) as f32,
        );
        let caret_byte = self.text_to_layout_position(self.caret_position()).byte;
        let current_line = (self.is_focused && !self.shows_placeholder())
            .then(|| layout.logical_line_index(caret_byte));
        // Pieces are where they paint with the field unscrolled.
        let unscrolled = origin_y + self.shown_scroll_y();
        let band = |top: f32, bottom: f32| {
            Rect::new(
                self.rect.x0,
                unscrolled + f64::from(top),
                self.rect.x1,
                unscrolled + f64::from(bottom),
            )
        };
        let mut pieces: Vec<Piece> = Vec::new();
        let mut last: Option<(usize, f32, f32, rustc_hash::FxHasher)> = None;
        let close = |line: Option<(usize, f32, f32, rustc_hash::FxHasher)>,
                     pieces: &mut Vec<Piece>| {
            if let Some((index, top, bottom, hasher)) = line {
                pieces.push(Piece {
                    key: u64::try_from(index).unwrap_or(MARK - 1),
                    rect: band(top, bottom),
                    paint: hasher.finish(),
                });
            }
        };
        for run in layout.rows_within(top, bottom) {
            let (row_top, row_bottom) = (run.line_top, run.line_top + run.line_height);
            if let Some(line) = last.as_mut().filter(|line| line.0 == run.line_i) {
                line.1 = line.1.min(row_top);
                line.2 = line.2.max(row_bottom);
                continue;
            }
            close(last.take(), &mut pieces);
            let mut hasher = rustc_hash::FxHasher::default();
            run.text.hash(&mut hasher);
            (current_line == Some(run.line_i)).hash(&mut hasher);
            if self.paints_highlight()
                && let Some(spans) = self.hl_spans.get(run.line_i)
            {
                for span in spans {
                    (span.range.start, span.range.end, span.kind).hash(&mut hasher);
                }
            }
            last = Some((run.line_i, row_top, row_bottom, hasher));
        }
        close(last.take(), &mut pieces);
        let mut marks: Vec<Rect> = self
            .ml_caret
            .iter()
            .map(|(caret, _)| *caret + Vec2::new(0.0, self.shown_scroll_y()))
            .collect();
        if current_line.is_some() && self.highlight_current_line {
            marks.extend(
                layout
                    .logical_line_bands(caret_byte)
                    .into_iter()
                    .map(|(top, height)| band(top, top + height)),
            );
        }
        if let Some((start, end)) = self.selection_range() {
            marks.extend(
                self.selection_rects(theme, start, end)
                    .into_iter()
                    .map(|rect| rect + Vec2::new(0.0, self.shown_scroll_y())),
            );
        }
        pieces.extend(
            marks
                .into_iter()
                .enumerate()
                .map(|(index, rect)| mark(index, rect)),
        );
        let colors = &theme.tokens.colors;
        let bordered = !self.plain;
        let reach = f64::from(theme.tokens.radii.control).max(2.0) + 1.0;
        let rect = self.rect;
        self.shown.note(Showing {
            scroll: Vec2::new(0.0, self.shown_scroll_y()),
            clip: rect,
            transform: crate::scroll_copy::on_screen(transform),
            look: FieldLook {
                states: [
                    bordered,
                    self.is_focused,
                    self.is_hovered,
                    self.invalid,
                    self.effectively_disabled(),
                    self.caret_paint_suppressed,
                ],
                left: (self.single_origin.x - self.rect.x0).to_bits(),
                radius: f64::from(theme.tokens.radii.control).to_bits(),
                colors: [
                    theme.text_primary,
                    theme.text_dim,
                    colors.surfaces.raised,
                    colors.intent.selection_tint,
                    colors.intent.accent_ink,
                    colors.surfaces.content,
                ],
            },
            // A field with chrome paints its border and rounded corners over
            // the rows.
            overlays: if bordered {
                vec![
                    Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + reach),
                    Rect::new(rect.x0, rect.y1 - reach, rect.x1, rect.y1),
                    Rect::new(rect.x0, rect.y0, rect.x0 + reach, rect.y1),
                    Rect::new(rect.x1 - reach, rect.y0, rect.x1, rect.y1),
                ]
            } else {
                Vec::new()
            },
            pieces,
            fixed: Vec::new(),
        });
    }

    /// Add `marks` (on screen) to what this frame shows painted over the
    /// field's rows, for a host that paints over them (a code editor's
    /// diagnostics): a frame where they changed repaints where they were
    /// and are.
    pub fn note_marks(&self, marks: impl IntoIterator<Item = Rect>) {
        if let Some(mut shown) = self.shown.drawing_mut() {
            let scroll = shown.scroll;
            let first = shown
                .pieces
                .iter()
                .filter(|piece| piece.key & MARK != 0)
                .count();
            let added: Vec<Piece> = marks
                .into_iter()
                .enumerate()
                .map(|(index, rect)| mark(first.saturating_add(index), rect + scroll))
                .collect();
            shown.pieces.extend(added);
        }
    }

    /// This field's damage since the frame on screen, for a host that draws
    /// it as part of `owner` (itself, as a widget in the tree): a scroll is
    /// offered as a copy, the lines and marks that changed repaint where
    /// they were and are, anything else repaints the field.
    pub fn collect_damage_for<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) {
        let since = self.shown.damage(owner, regions);
        if since == Since::Repaint {
            regions.push(self.rect);
        }
    }

    /// From the host's `mark_drawn`: the frame just drawn is on screen.
    pub fn mark_shown(&mut self) {
        self.shown.present();
    }
}
