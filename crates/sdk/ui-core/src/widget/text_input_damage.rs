//! What a shaped multiline field repaints. A frame that only scrolled it
//! offers its host a copy of what stays on screen ([`crate::scroll_copy`]);
//! one that changed some of its lines in place repaints those lines and
//! where its caret, selection and marks were and are, not the field.

use std::hash::{Hash, Hasher};

use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Color;

use super::TextInput;
use crate::scroll_copy::{Piece, Showing, Since};
use crate::text_render::TextRenderer;
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
    /// draw is done. Each line is a piece where it paints: its rows' bands
    /// and the ink that reaches past them
    /// ([`TextRenderer::rich_line_ink_reach`]), for every line whose ink
    /// may show in the field, as its draw takes them.
    pub(super) fn note_shown(&self, transform: Affine, theme: &Theme, renderer: &mut TextRenderer) {
        let Some(layout) = self
            .single_layout
            .as_ref()
            .filter(|_| self.multiline_shaped && !self.untracked())
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
        let reach = renderer.rich_ink_reach(layout);
        let mut pieces: Vec<Piece> = Vec::new();
        let mut last: Option<(usize, f32, f32, rustc_hash::FxHasher)> = None;
        let mut close = |line: Option<(usize, f32, f32, rustc_hash::FxHasher)>,
                         pieces: &mut Vec<Piece>| {
            if let Some((index, top, bottom, hasher)) = line {
                let ink = renderer.rich_line_ink_reach(layout, index);
                pieces.push(Piece {
                    key: u64::try_from(index).unwrap_or(MARK - 1),
                    rect: band(top - ink.above, bottom + ink.below),
                    paint: hasher.finish(),
                });
            }
        };
        for run in layout.rows_reaching(top, bottom, reach) {
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
        self.shown.note(Showing {
            scroll: Vec2::new(0.0, self.shown_scroll_y()),
            clip: self.rect,
            transform: crate::scroll_copy::on_screen(transform),
            look: self.field_look(theme),
            overlays: self.field_overlays(theme),
            pieces,
            fixed: Vec::new(),
        });
    }

    /// What this model does not follow (a placeholder, a composition,
    /// spelling, an autocorrection, styled spans, an accessory): the frame
    /// is left unnoted, and the field repaints whole.
    fn untracked(&self) -> bool {
        self.shows_placeholder()
            || self.core.has_preedit()
            || self.rich_spans.is_some()
            || self.accessory.is_some()
            || !self.spell_spans.is_empty()
            || self.last_autocorrection.is_some()
            || self.autocorrection_preview.is_some()
    }

    fn field_look(&self, theme: &Theme) -> FieldLook {
        let colors = &theme.tokens.colors;
        FieldLook {
            states: [
                !self.plain,
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
        }
    }

    /// A field with chrome paints its border and rounded corners over the
    /// rows.
    fn field_overlays(&self, theme: &Theme) -> Vec<Rect> {
        if self.plain {
            return Vec::new();
        }
        let reach = f64::from(theme.tokens.radii.control).max(2.0) + 1.0;
        let rect = self.rect;
        vec![
            Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + reach),
            Rect::new(rect.x0, rect.y1 - reach, rect.x1, rect.y1),
            Rect::new(rect.x0, rect.y0, rect.x0 + reach, rect.y1),
            Rect::new(rect.x1 - reach, rect.y0, rect.x1, rect.y1),
        ]
    }

    /// From the shaped draw, once the field is scrolled and placed for
    /// this frame: where the frame keeps the pixels of its rows, as
    /// [`Self::collect_damage_for`] will find it ([`crate::draw_cull`]).
    pub(super) fn field_keep(
        &self,
        transform: Affine,
        theme: &Theme,
    ) -> Option<crate::draw_cull::Keep> {
        if self.untracked() {
            return None;
        }
        let presented = self.shown.presented()?;
        if presented.clip != self.rect
            || presented.transform != crate::scroll_copy::on_screen(transform)
            || presented.look != self.field_look(theme)
        {
            return None;
        }
        let delta = presented.scroll - Vec2::new(0.0, self.shown_scroll_y());
        let mut overlays = self.field_overlays(theme);
        overlays.extend_from_slice(&presented.overlays);
        crate::draw_cull::Keep::scrolled(transform, self.rect, delta, &overlays)
    }

    /// The line numbers in the gutter beside the rows `visible` shows, but
    /// for the lines `leave_out` (ascending) whose numbers the frame keeps.
    pub(super) fn draw_gutter_line_numbers(
        &self,
        scene: &mut vello::Scene,
        transform: Affine,
        tr: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        style: &crate::text_render::TextStyle,
        caret_byte: usize,
        (gutter_width, visible, leave_out): (f64, Rect, &[usize]),
    ) {
        if gutter_width <= 0.0 {
            return;
        }
        let Some(layout) = &self.single_layout else {
            return;
        };
        let current_line = layout.logical_line_index(caret_byte);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "on-screen offsets in logical pixels are far inside f32"
        )]
        let (top, bottom) = (
            (visible.y0 - self.single_origin.y) as f32,
            (visible.y1 - self.single_origin.y) as f32,
        );
        for (line, top, _height) in layout.logical_line_origins_within(top, bottom) {
            if leave_out.binary_search(&line).is_ok() {
                continue;
            }
            let number = line.saturating_add(1).to_string();
            let (width, _) = tr.measure_style(&number, None, style);
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine transform composition over f64 screen coordinates"
            )]
            let origin = transform
                * Affine::translate((
                    self.rect.x0 + gutter_width
                        - super::GUTTER_PAD_R * self.gutter_text_scale
                        - f64::from(width),
                    self.single_origin.y + f64::from(top),
                ));
            let color = if line == current_line {
                theme.text_primary
            } else {
                theme.text_dim
            };
            tr.draw_style(scene, &number, color, origin, None, style);
        }
    }

    /// Paint the selection over the code's rows, and say which logical
    /// lines the frame need not paint ([`Self::kept_lines`]): the selection
    /// and the current line's band are marks over the rows.
    pub(super) fn paint_selection_keeping_lines(
        &self,
        (scene, transform): (&mut vello::Scene, Affine),
        tr: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        style: &crate::text_render::TextStyle,
        (visible, gutter_width, caret_byte): (Rect, f64, usize),
    ) -> Vec<usize> {
        let mut marks = self
            .selection_range()
            .map(|(start, end)| self.selection_rects(theme, start, end))
            .unwrap_or_default();
        let color = theme.tokens.colors.intent.selection_tint;
        for rect in &marks {
            scene.fill(vello::peniko::Fill::NonZero, transform, color, None, rect);
        }
        if let Some(layout) = &self.single_layout {
            marks.extend(
                layout
                    .logical_line_bands(caret_byte)
                    .into_iter()
                    .map(|(top, height)| {
                        let top = self.single_origin.y + f64::from(top);
                        Rect::new(self.rect.x0, top, self.rect.x1, top + f64::from(height))
                    }),
            );
        }
        self.kept_lines(scene, transform, tr, style, (visible, gutter_width), &marks)
    }

    /// The logical lines of the shaped layout this frame need not paint,
    /// ascending: those whose rows and line number the frame keeps on
    /// screen as they were ([`crate::draw_cull::kept_band`]), clear of
    /// every mark painted over the rows now (`marks`, on screen) or on the
    /// frame on screen (a caret, a current line, a selection).
    pub(super) fn kept_lines(
        &self,
        scene: &vello::Scene,
        transform: Affine,
        tr: &mut crate::text_render::TextRenderer,
        style: &crate::text_render::TextStyle,
        (visible, gutter_width): (Rect, f64),
        marks: &[Rect],
    ) -> Vec<usize> {
        let Some(band) = crate::draw_cull::kept_band(scene, transform, visible) else {
            return Vec::new();
        };
        let Some(layout) = self.single_layout.as_ref() else {
            return Vec::new();
        };
        let scroll = Vec2::new(0.0, self.shown_scroll_y());
        let mut marks = marks.to_vec();
        if let Some(presented) = self.shown.presented() {
            marks.extend(
                presented
                    .pieces
                    .iter()
                    .filter(|piece| piece.key & MARK != 0)
                    .map(|piece| piece.rect - scroll),
            );
        }
        let origin = self.single_origin;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "on-screen offsets in logical pixels are far inside f32"
        )]
        let (top, bottom) = ((band.y0 - origin.y) as f32, (band.y1 - origin.y) as f32);
        let number_right =
            self.rect.x0 + gutter_width - super::GUTTER_PAD_R * self.gutter_text_scale;
        let meets = |rect: Rect| {
            marks.iter().any(|mark| {
                let common = mark.intersect(rect);
                common.width() > 0.0 && common.height() > 0.0
            })
        };
        // Each line's rows, as one ink box: from its number (if the gutter
        // shows one) to the end of its longest row, half a row of overhang
        // around them, and above and below at least as far as its ink
        // reaches past its bands.
        let mut lines: Vec<(usize, Rect, f64)> = Vec::new();
        for run in layout.rows_within(top, bottom) {
            let height = f64::from(run.line_height);
            let row = Rect::new(
                origin.x,
                origin.y + f64::from(run.line_top),
                origin.x + f64::from(run.line_w),
                origin.y + f64::from(run.line_top) + height,
            );
            match lines.last_mut() {
                Some((line, bands, overhang)) if *line == run.line_i => {
                    *bands = bands.union(row);
                    *overhang = overhang.max(height / 2.0);
                }
                _ => lines.push((run.line_i, row, height / 2.0)),
            }
        }
        let mut kept = Vec::new();
        for (line, bands, overhang) in lines {
            let reach = tr.rich_line_ink_reach(layout, line);
            let ink = Rect::new(
                bands.x0 - overhang,
                bands.y0 - overhang.max(f64::from(reach.above)),
                bands.x1 + overhang,
                bands.y1 + overhang.max(f64::from(reach.below)),
            );
            if ink.y0 < band.y0 || ink.y1 > band.y1 || meets(ink) {
                continue;
            }
            let ink = if gutter_width > 0.0 {
                let number = line.saturating_add(1).to_string();
                let (width, _) = tr.measure_style(&number, None, style);
                let left = number_right - f64::from(width) - ink.height() / 2.0;
                ink.union(Rect::new(left, ink.y0, number_right + 1.0, ink.y1))
            } else {
                ink
            };
            if crate::draw_cull::kept(scene, transform, ink) {
                kept.push(line);
            }
        }
        kept
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

#[cfg(test)]
#[path = "text_input_damage_tests.rs"]
mod tests;
