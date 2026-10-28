//! The editor's vertical scroll as every toolkit scroller does it: the wheel
//! and fingers move the lines through the shared momentum
//! ([`ui::ScrollMomentum`]), in whole device pixels and through the rubber
//! band past either end; and each frame says which lines it drew where, as
//! if unscrolled ([`ui::scroll_copy::PaintedFrames`]), so a scroll repaints
//! only the lines that came into view and a host copies the rest.
//!
//! The lines are painted where they stand in the buffer, so the scene of a
//! row depends on the theme, the fonts and the editor's geometry but never
//! on the scroll: [`RowsKept`] keeps the rows in view and appends them again
//! while all of that and the row's own source and spans hold.

use crate::highlight::Span;
use ui::scroll_copy::{Around, Painted, PaintedFrames, Piece, Showing, piece_key};
use ui::text_render::FontEpoch;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use winit::event::{MouseScrollDelta, TouchPhase};

use super::{EditorView, PaintCtx, VisibleRows};

#[cfg(test)]
#[path = "editor_row_cache_tests.rs"]
mod row_cache_tests;

/// The editor's glide and the frames it showed.
#[derive(Default)]
pub(super) struct Scroll {
    glide: ui::ScrollMomentum,
    /// Device pixels per logical pixel where the editor last took a wheel.
    device_scale: Option<f64>,
    /// The theme's motion, where the editor last drew.
    motion: ui::tokens::EffectiveMotion,
    shown: PaintedFrames,
}

/// How many rows may keep their painted scene: every row in view on any
/// screen, so scrolling through a buffer of any length does not grow it.
const ROWS_KEPT: usize = 256;
/// The most one kept row may take: its source, its spans and its scene.
const ROW_KEPT_BYTES: usize = 512 * 1024;
/// The most all kept rows may take together.
const ROWS_KEPT_BYTES: usize = 8 * 1024 * 1024;
/// The longest source line whose row keeps a scene. A longer row is painted
/// through a window of its columns, so encoding one again stays cheap, and a
/// scene of the whole row is not worth the bound it would need.
const ROW_KEPT_LINE: usize = 4096;

/// The scenes of the rows in view, each as the frame painted it where it
/// stands in the buffer (unscrolled). A frame whose look and whose rows are
/// unchanged appends them instead of shaping and encoding every glyph of
/// every line again.
///
/// A row that carries a caret, a selection, the composition, a find match or
/// a diagnostic is painted fresh and keeps nothing, so what the person moves
/// or the language server reports never comes from a kept scene. Neither
/// does a row of another theme or font, in another geometry, or holding
/// other source or other spans: those all change the look or the row.
#[derive(Default)]
pub(super) struct RowsKept {
    /// What every kept row was painted with, and [`None`] before the first
    /// draw.
    look: Option<RowLook>,
    rows: Vec<KeptRow>,
    bytes: usize,
    /// How many rows the frame being drawn reused and painted fresh.
    reused: usize,
    painted: usize,
}

/// Everything a row's paint is made of besides the row itself.
struct RowLook {
    /// The whole theme, compared exactly: every colour, spacing, syntax and
    /// ground the rows are painted from.
    theme: Theme,
    shape: LookShape,
}

/// The look besides the theme: the fonts and scale the glyphs were shaped
/// with, where the frame lands the rows on screen, and the editor's box, its
/// gutter and its measured cell. The scroll is deliberately absent: a row is
/// painted where it stands in the buffer, so moving the lines moves the
/// scene instead of changing it.
#[derive(Clone, PartialEq)]
struct LookShape {
    font_epoch: FontEpoch,
    distinguishes_letters: bool,
    text_scale: f32,
    transform: Affine,
    rect: Rect,
    gutter_w: f64,
    code_x0: f64,
    cell_w: f64,
    line_h: f64,
    text_dy: f64,
    pad: f64,
    scroll_x: f64,
}

/// One row's paint, with the row it was painted from.
struct KeptRow {
    line: usize,
    text: String,
    spans: Vec<Span>,
    scene: Scene,
    bytes: usize,
}

impl KeptRow {
    /// Whether this is row `line` as `text` and `spans` resolve it.
    fn is(&self, line: usize, text: &str, spans: &[Span]) -> bool {
        self.line == line && self.text == text && self.spans == spans
    }
}

impl RowsKept {
    /// From the frame's draw: the look it paints with. A look that differs
    /// from the kept rows' drops them all; either way the frame's counts
    /// start again.
    fn sync(&mut self, theme: &Theme, shape: &LookShape) {
        self.reused = 0;
        self.painted = 0;
        let held = self
            .look
            .as_ref()
            .is_some_and(|look| look.theme == *theme && look.shape == *shape);
        if !held {
            self.clear();
            self.look = Some(RowLook {
                theme: theme.clone(),
                shape: shape.clone(),
            });
        }
    }

    /// The kept scene of row `line`, when it paints `text` and `spans` as
    /// the frame's look does now.
    fn take(&mut self, line: usize, text: &str, spans: &[Span]) -> Option<Scene> {
        let at = self.at(line).ok()?;
        let kept = self.rows.get(at)?;
        if !kept.is(line, text, spans) {
            return None;
        }
        // A copy, not the row itself: the frame lands one scene of it in the
        // frame and hands the other to the frame's note, and the next frame
        // still has the row to reuse.
        let scene = kept.scene.clone();
        self.reused = self.reused.saturating_add(1);
        Some(scene)
    }

    /// Keep `scene` as row `line`'s paint, unless the row or the scene is
    /// past what the cache holds.
    fn keep(&mut self, line: usize, text: &str, spans: &[Span], scene: &Scene) {
        if text.len() > ROW_KEPT_LINE {
            return;
        }
        let bytes = scene_bytes(scene)
            .saturating_add(std::mem::size_of::<KeptRow>())
            .saturating_add(text.len())
            .saturating_add(spans.len().saturating_mul(std::mem::size_of::<Span>()));
        if bytes > ROW_KEPT_BYTES {
            return;
        }
        self.drop_row(line);
        while !self.rows.is_empty()
            && (self.rows.len() >= ROWS_KEPT || self.bytes.saturating_add(bytes) > ROWS_KEPT_BYTES)
        {
            let gone = self.rows.remove(0);
            self.bytes = self.bytes.saturating_sub(gone.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        // The rows are kept by line, so a frame finds one without reading
        // the lines under it.
        let at = self.rows.partition_point(|row| row.line < line);
        self.rows.insert(
            at,
            KeptRow {
                line,
                text: text.to_owned(),
                spans: spans.to_vec(),
                scene: scene.clone(),
                bytes,
            },
        );
    }

    /// Where row `line` stands among the kept rows, or where it would.
    fn at(&self, line: usize) -> Result<usize, usize> {
        self.rows.binary_search_by_key(&line, |row| row.line)
    }

    /// Keep only the rows of `lines`, the ones the frame has in view.
    fn retain(&mut self, lines: std::ops::Range<usize>) {
        let Self { rows, bytes, .. } = self;
        rows.retain(|row| {
            let keeping = lines.contains(&row.line);
            if !keeping {
                *bytes = bytes.saturating_sub(row.bytes);
            }
            keeping
        });
    }

    fn drop_row(&mut self, line: usize) {
        if let Ok(at) = self.at(line) {
            let gone = self.rows.remove(at);
            self.bytes = self.bytes.saturating_sub(gone.bytes);
        }
    }

    fn clear(&mut self) {
        self.rows.clear();
        self.bytes = 0;
        self.look = None;
    }
}

/// Owned scene payload copied into the cache. Count the actual element
/// types, including variable-font coordinates and resource patches; font
/// and image blobs themselves are shared rather than copied by `Scene::clone`.
fn scene_bytes(scene: &Scene) -> usize {
    use std::mem::{size_of, size_of_val};
    let encoding = scene.encoding();
    [
        size_of::<Scene>(),
        size_of_val(encoding.path_data.as_slice()),
        size_of_val(encoding.draw_data.as_slice()),
        size_of_val(encoding.draw_tags.as_slice()),
        size_of_val(encoding.path_tags.as_slice()),
        size_of_val(encoding.transforms.as_slice()),
        size_of_val(encoding.styles.as_slice()),
        size_of_val(encoding.resources.glyphs.as_slice()),
        size_of_val(encoding.resources.glyph_runs.as_slice()),
        size_of_val(encoding.resources.normalized_coords.as_slice()),
        size_of_val(encoding.resources.patches.as_slice()),
        size_of_val(encoding.resources.color_stops.as_slice()),
    ]
    .into_iter()
    .fold(0, usize::saturating_add)
}

/// What an editor frame paints over its lines without moving with them.
pub(super) struct Over {
    /// The fade at the top edge, while lines scroll in from above.
    pub fade: Scene,
    /// The button back to the line start, while scrolled sideways.
    pub recovery: Scene,
    /// The completion popup and the hover tooltip.
    pub popups: Scene,
}

impl EditorView {
    /// How far the lines stand scrolled, in logical pixels.
    fn scroll_px(&self) -> f64 {
        self.scroll_y * self.line_h
    }

    /// The furthest the lines scroll: the last line at the top.
    fn max_scroll_px(&self) -> f64 {
        (self.lines.len() as f64 - 1.0).max(0.0) * self.line_h
    }

    /// Move the lines `px` logical pixels down the buffer (up when
    /// negative), as far as it goes; returns how far they moved.
    fn scroll_by_px(&mut self, px: f64) -> f64 {
        if !px.is_finite() || self.line_h <= 0.0 {
            return 0.0;
        }
        let before = self.scroll_px();
        let to = (before + px).clamp(0.0, self.max_scroll_px());
        self.scroll_y = to / self.line_h;
        self.dirty = true;
        to - before
    }

    /// Where the lines are drawn: the scroll in whole device pixels, and
    /// the give past an end the fingers pulled them through.
    fn drawn_scroll_px(&self) -> f64 {
        let scale = self.scroll.device_scale.unwrap_or(1.0);
        let at = (self.scroll_px() * scale).round() / scale;
        at + self.scroll.glide.give(ui::motion::frame_now())
    }

    /// Mouse wheel and touchpad motion over the editor, through the shared
    /// momentum: a notch moves three lines at once and glides on, fingers
    /// move the lines 1:1 and a flick glides on after they lift.
    pub(super) fn handle_mouse_wheel(
        &mut self,
        delta: MouseScrollDelta,
        phase: TouchPhase,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        if !self.rect.contains(self.last_cursor) {
            return;
        }
        *handled = true;
        self.scroll.device_scale = Some(scale_factor);
        self.scroll.glide.set_device_scale(scale_factor);
        let scrolls = self.max_scroll_px() > 0.0;
        let motion = self.scroll.motion;
        self.scroll
            .glide
            .set_view(self.rect.height(), scrolls, motion);
        let step = match self.scroll.glide.wheel(&delta, phase, motion.reduced()) {
            ui::virtual_viewport::WheelStep::Scroll(step) => step,
            ui::virtual_viewport::WheelStep::Fling | ui::virtual_viewport::WheelStep::Ignored => {
                return;
            }
        };
        // A notch is three lines, not the momentum's own step.
        let step = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 3.0 * self.line_h,
            MouseScrollDelta::PixelDelta(_) => step,
        };
        let moved = self.scroll_by_px(step);
        self.scroll.glide.wheel_applied(step, moved);
    }

    /// Step a glide to the frame's time, from `update`; a glide that meets
    /// an end of the buffer stops there.
    pub(super) fn advance_glide(&mut self) {
        if !self.scroll.glide.is_active() {
            return;
        }
        let step = self.scroll.glide.advance(ui::motion::frame_now());
        if step != 0.0 && self.scroll_by_px(step) == 0.0 {
            self.scroll.glide.stop();
        }
    }

    /// Where the lines come to rest, in lines down the buffer: where they
    /// stand, plus how far a glide still carries them. A view captured
    /// mid-glide is the one the person is scrolling to.
    pub(super) fn resting_scroll_y(&self) -> f64 {
        if self.line_h <= 0.0 {
            return self.scroll_y;
        }
        let rest =
            (self.scroll_px() + self.scroll.glide.rest_distance()).clamp(0.0, self.max_scroll_px());
        rest / self.line_h
    }

    /// The lines stand where they were put: a glide under way stops.
    pub(super) fn stop_glide(&mut self) {
        self.scroll.glide.stop();
    }

    /// Whether a glide, or the give springing back, needs frames.
    pub(super) fn glide_needs_frame(&self) -> bool {
        self.scroll.glide.is_active() || self.scroll.glide.give_needs_frame(ui::motion::frame_now())
    }

    /// Paint the lines in view, each with its background, its text, its
    /// diagnostics and its carets, drawn where it stands in the buffer as
    /// if unscrolled and landed scrolled: a line a scroll only moved paints
    /// the same. Every visible row is appended every frame, in buffer order;
    /// a row nothing about changed comes from [`RowsKept`] instead of being
    /// shaped and encoded again. Returns the lines as the frame's pieces.
    pub(super) fn draw_lines(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rows: &VisibleRows,
        primary_line: usize,
    ) -> Vec<Piece<Painted>> {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx.reborrow();
        let scroll = self.drawn_scroll_px();
        let scrolled = transform * Affine::translate((0.0, -scroll)) * transform.inverse();
        let unscrolled = VisibleRows {
            origin_y: self.rect.y0,
            bottom: self.rect.y1 + scroll,
            ..*rows
        };
        self.rows_kept.sync(
            theme,
            &LookShape {
                font_epoch: text.font_epoch(),
                distinguishes_letters: text.distinguishes_letters(),
                text_scale: text.text_scale(),
                transform,
                rect: self.rect,
                gutter_w: rows.gutter_w,
                code_x0: rows.code_x0,
                cell_w: self.cell_w,
                line_h: self.line_h,
                text_dy: self.text_dy,
                pad: self.pad,
                scroll_x: self.scroll_x,
            },
        );
        let mut pieces = Vec::with_capacity(
            rows.n_visible
                .saturating_add(self.carets.len().min(rows.n_visible)),
        );
        for line in rows.first_line..rows.first_line + rows.n_visible {
            let Some(source) = self.lines.get(line).map(String::as_str) else {
                break;
            };
            let empty: &[Span] = &[];
            let spans: &[Span] = self.spans.get(line).map_or(empty, Vec::as_slice);
            let one = VisibleRows {
                first_line: line,
                n_visible: 1,
                ..unscrolled
            };
            // A row past the viewport's bottom edge paints nothing, so it
            // keeps nothing either: a scroll that brings it in must paint it
            // rather than replay an empty scene.
            let paints = one.line_top(line, self.line_h) <= one.bottom;
            // A row the person moves or the view learns after the frame is
            // painted fresh and keeps nothing.
            let still = paints && !self.row_carries_interaction(line, primary_line);
            let kept = if still {
                self.rows_kept.take(line, source, spans)
            } else {
                None
            };
            let drawn = if let Some(kept) = kept {
                kept
            } else {
                let mut drawn = Scene::new();
                let mut row = PaintCtx::new(&mut drawn, transform, &mut *text, theme);
                self.draw_line_backgrounds(&mut row, &one, primary_line);
                self.draw_line_text(&mut row, &one, primary_line);
                self.draw_diagnostics(&mut drawn, transform, theme, &one);
                self.rows_kept.painted = self.rows_kept.painted.saturating_add(1);
                if still {
                    self.rows_kept.keep(line, source, spans, &drawn);
                }
                drawn
            };
            scene.append(&drawn, Some(scrolled));
            // A line's glyphs may reach a line past it either way.
            let top = one.line_top(line, self.line_h) - scroll;
            // Keep the caret after this row's glyphs, just as in the combined
            // row. Its independent note lets movement within unchanged text
            // repaint the stroke instead of the full three-line glyph band.
            let mut carets = Scene::new();
            let caret_bounds = self.draw_carets(&mut carets, transform, theme, &one);
            scene.append(&carets, Some(scrolled));
            pieces.push(Piece {
                key: piece_key(line),
                rect: Rect::new(
                    self.rect.x0,
                    top - self.line_h,
                    self.rect.x1,
                    top + 2.0 * self.line_h,
                ),
                paint: Painted(drawn),
            });
            if let Some(bounds) = caret_bounds {
                pieces.push(Piece {
                    key: piece_key(("carets", line)),
                    rect: bounds + Vec2::new(0.0, -scroll),
                    paint: Painted(carets),
                });
            }
        }
        // Only the rows in view are kept, so a scroll through a long buffer
        // does not hold the rows it left.
        self.rows_kept
            .retain(rows.first_line..rows.first_line.saturating_add(rows.n_visible));
        pieces
    }

    /// Whether row `line` paints something the person moves or the view
    /// learns after the frame: the primary row's highlight, its line number
    /// and the composition; a caret's stroke; the bands of a selection; a
    /// find match; a diagnostic. Such a row is painted fresh, so no kept
    /// scene can show any of them stale.
    ///
    /// The composition sits at the primary caret, whose row is the primary
    /// row, so it needs no test of its own.
    fn row_carries_interaction(&self, line: usize, primary_line: usize) -> bool {
        if line == primary_line {
            return true;
        }
        let touching = |caret: &super::Caret| {
            caret.line == line
                || caret
                    .selection()
                    .is_some_and(|(start, end)| start.0 <= line && line <= end.0)
        };
        self.carets.iter().any(touching)
            || self.find_matches.iter().any(|found| found.line == line)
            || self.diagnostics.iter().any(|found| found.line == line)
    }

    /// Note what this frame showed: the lines in `pieces`, scrolled into
    /// the whole-line viewport `clip`, under what `over` paints in place.
    pub(super) fn note_shown(
        &mut self,
        transform: Affine,
        clip: Rect,
        pieces: Vec<Piece<Painted>>,
        over: Over,
    ) {
        let fade = ui::surface::content_edge_fade_band(clip, ui::surface::ContentEdge::Top);
        let fixed = vec![
            Piece {
                key: piece_key("fade"),
                rect: fade,
                paint: Painted(over.fade),
            },
            Piece {
                key: piece_key("line start"),
                rect: self.horizontal_scroll_reset_rect,
                paint: Painted(over.recovery),
            },
            Piece {
                key: piece_key("popups"),
                rect: self.rect,
                paint: Painted(over.popups),
            },
        ];
        let look = Around {
            bounds: self.rect,
            outside: Painted::default(),
        };
        match Showing::painted(
            Vec2::new(0.0, self.drawn_scroll_px()),
            clip,
            ui::scroll_copy::on_screen(transform),
            look,
            pieces,
            fixed,
        ) {
            Some(shown) => self.scroll.shown.note(shown),
            None => self.scroll.shown.forget(),
        }
    }

    /// The theme's motion, from the draw, for the next wheel.
    pub(super) fn note_motion(&mut self, theme: &Theme) {
        self.scroll.motion = theme.motion();
    }

    /// A frame that drew no lines (the empty state): the next scroll
    /// repaints.
    pub(super) fn forget_shown(&mut self) {
        self.scroll.shown.forget();
    }

    /// From the test's draw: how many rows the last frame reused from
    /// [`RowsKept`] and how many it painted.
    #[cfg(test)]
    pub(super) fn rows_kept_counts(&self) -> (usize, usize) {
        (self.rows_kept.reused, self.rows_kept.painted)
    }

    /// From the test's draw: the lines [`RowsKept`] holds, and the bytes
    /// that take.
    #[cfg(test)]
    pub(super) fn rows_kept_lines(&self) -> Vec<usize> {
        self.rows_kept.rows.iter().map(|row| row.line).collect()
    }

    #[cfg(test)]
    pub(super) fn rows_kept_bytes(&self) -> usize {
        self.rows_kept.bytes
    }

    /// From the test: drop every kept row, so the next frame paints the
    /// rows the view holds now as a frame with nothing kept.
    #[cfg(test)]
    pub(super) fn forget_rows_kept(&mut self) {
        self.rows_kept.clear();
    }

    /// From `collect_damage`: the lines that changed, came or went, and a
    /// scroll as a copy of the lines it only moved; the editor whole when
    /// anything else changed.
    pub(super) fn damage(&self, regions: &mut Vec<Rect>) {
        self.scroll.shown.damage_within(self, self.rect, regions);
    }

    /// From `mark_drawn`: the frame just drawn is on screen.
    pub(super) fn present_shown(&mut self) {
        self.scroll.shown.present();
    }
}

#[cfg(test)]
mod tests {
    use super::super::EditorView;
    use crate::doc::EditorDoc;
    use crate::model::Language;
    use automerge::AutoCommit;
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use ui::widget::Widget as _;
    use vello::Scene;
    use vello::kurbo::{Affine, Rect, Vec2};
    use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};

    /// An editor over 200 short lines, laid out in a 600 by 400 view.
    fn shown_lines(doc: &mut AutoCommit) -> EditorView {
        let source = (0..200).fold(String::new(), |mut source, line| {
            use std::fmt::Write as _;
            let _ = writeln!(source, "let value_{line} = {line};");
            source
        });
        let text = EditorDoc::ensure_buffer(doc, 0, &source);
        let mut view = EditorView::new();
        view.bind(text, Language::Rust);
        view.sync(doc, &automerge::ROOT);
        view.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
        view
    }

    /// Point at the middle of the view, then send it `delta`.
    fn wheel(view: &mut EditorView, doc: &mut AutoCommit, delta: MouseScrollDelta) {
        let centre = view.rect.center();
        ui::testing::Dispatch::new().doc(&mut *doc).send(
            view,
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
            },
        );
        ui::testing::Dispatch::new().doc(doc).send(
            view,
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta,
                phase: TouchPhase::Moved,
            },
        );
    }

    /// A view captured while a notch still glides the lines on, as a
    /// continuation captures it, is the view the glide rests at.
    #[test]
    fn a_view_captured_mid_glide_is_the_one_the_lines_rest_at() {
        let mut doc = AutoCommit::new();
        let mut view = shown_lines(&mut doc);
        let start = ui::time::now();
        ui::time::testing::with_monotonic_time(start, || {
            wheel(&mut view, &mut doc, MouseScrollDelta::LineDelta(0.0, -1.0));
        });
        assert!(view.scroll.glide.is_active(), "the notch glides on");
        let captured = view
            .continuation_position(&doc, "6f1d2c1e-5a3b-4c7d-9e8f-0a1b2c3d4e5f")
            .expect("the view is captured");
        ui::time::testing::with_monotonic_time(start + std::time::Duration::from_secs(5), || {
            view.advance_glide();
        });
        assert!(!view.scroll.glide.is_active(), "the glide came to rest");
        assert!(
            view.scroll_y > 3.0,
            "the glide carried the lines past the notch's three: {}",
            view.scroll_y
        );
        let mut other = shown_lines(&mut doc);
        assert!(other.restore_continuation_position(&doc, &captured));
        assert!(
            (other.scroll_y - view.scroll_y).abs() < 1e-9,
            "continued at {}, the lines rest at {}",
            other.scroll_y,
            view.scroll_y
        );
    }

    /// Fingers move the lines pixel by pixel, and a host that copies what a
    /// scroll keeps on screen copies the lines they only moved: the frame
    /// repaints the strip that came into view, not the editor.
    #[test]
    fn fingers_scroll_the_lines_by_the_pixel_and_copy_them() {
        let mut doc = AutoCommit::new();
        let mut view = shown_lines(&mut doc);
        let viewport = view.rect;
        let theme = Theme::dark();
        let mut renderer = TextRenderer::new();
        let mut frame = |view: &mut EditorView| {
            view.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &PresenceCtx::empty(),
            );
        };
        frame(&mut view);
        view.mark_drawn();
        wheel(
            &mut view,
            &mut doc,
            MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -7.0)),
        );
        assert!(
            (view.scroll_px() - 7.0).abs() < 1e-9,
            "the lines moved with the fingers: {}",
            view.scroll_px()
        );
        frame(&mut view);
        let mut regions = Vec::new();
        let offers = ui::scroll_copy::accepting(|| view.collect_damage(&mut regions));
        assert_eq!(offers.len(), 1, "the scroll is offered as a copy");
        assert_eq!(offers[0].delta(), Vec2::new(0.0, -7.0));
        let area: f64 = regions.iter().map(Rect::area).sum();
        assert!(
            area * 4.0 < viewport.area(),
            "a scroll repaints a strip of the editor, not {area} of {} px: {regions:?}",
            viewport.area()
        );
    }
}
