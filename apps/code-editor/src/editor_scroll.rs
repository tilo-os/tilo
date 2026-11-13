//! The editor's vertical scroll, through the toolkit's shared momentum and rubber band.

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

#[derive(Default)]
pub(super) struct Scroll {
    glide: ui::ScrollMomentum,
    device_scale: Option<f64>,
    motion: ui::tokens::EffectiveMotion,
    shown: PaintedFrames,
}

const ROWS_KEPT: usize = 256;
const ROW_KEPT_BYTES: usize = 512 * 1024;
const ROWS_KEPT_BYTES: usize = 8 * 1024 * 1024;
const ROW_KEPT_LINE: usize = 4096;

/// The scenes of the rows in view, as the frame painted them at their place in the buffer. A frame
/// whose looks and rows are unchanged appends them instead of shaping every glyph again.
///
/// A row with a caret, a selection, the composition, a find match or a diagnostic is painted fresh
/// and keeps nothing, and so is a row of another theme, font, geometry, source or spans.
#[derive(Default)]
pub(super) struct RowsKept {
    look: Option<RowLook>,
    rows: Vec<KeptRow>,
    bytes: usize,
    reused: usize,
    painted: usize,
}

struct RowLook {
    theme: Theme,
    shape: LookShape,
}

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

struct KeptRow {
    line: usize,
    text: String,
    spans: Vec<Span>,
    scene: Scene,
    bytes: usize,
}

impl KeptRow {
    fn is(&self, line: usize, text: &str, spans: &[Span]) -> bool {
        self.line == line && self.text == text && self.spans == spans
    }
}

impl RowsKept {
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

    fn take(&mut self, line: usize, text: &str, spans: &[Span]) -> Option<Scene> {
        let at = self.at(line).ok()?;
        let kept = self.rows.get(at)?;
        if !kept.is(line, text, spans) {
            return None;
        }
        let scene = kept.scene.clone();
        self.reused = self.reused.saturating_add(1);
        Some(scene)
    }

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

pub(super) struct Over {
    pub fade: Scene,
    pub recovery: Scene,
    pub popups: Scene,
}

impl EditorView {
    fn scroll_px(&self) -> f64 {
        self.scroll_y * self.line_h
    }

    fn max_scroll_px(&self) -> f64 {
        (self.lines.len() as f64 - 1.0).max(0.0) * self.line_h
    }

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

    fn drawn_scroll_px(&self) -> f64 {
        let scale = self.scroll.device_scale.unwrap_or(1.0);
        let at = (self.scroll_px() * scale).round() / scale;
        at + self.scroll.glide.give(ui::motion::frame_now())
    }

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
        let step = match delta {
            MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 3.0 * self.line_h,
            MouseScrollDelta::PixelDelta(_) => step,
        };
        let moved = self.scroll_by_px(step);
        self.scroll.glide.wheel_applied(step, moved);
    }

    pub(super) fn advance_glide(&mut self) {
        if !self.scroll.glide.is_active() {
            return;
        }
        let step = self.scroll.glide.advance(ui::motion::frame_now());
        if step != 0.0 && self.scroll_by_px(step) == 0.0 {
            self.scroll.glide.stop();
        }
    }

    pub(super) fn resting_scroll_y(&self) -> f64 {
        if self.line_h <= 0.0 {
            return self.scroll_y;
        }
        let rest =
            (self.scroll_px() + self.scroll.glide.rest_distance()).clamp(0.0, self.max_scroll_px());
        rest / self.line_h
    }

    pub(super) fn stop_glide(&mut self) {
        self.scroll.glide.stop();
    }

    pub(super) fn glide_needs_frame(&self) -> bool {
        self.scroll.glide.is_active() || self.scroll.glide.give_needs_frame(ui::motion::frame_now())
    }

    /// Paint the lines in view, each with its background, text, diagnostics and carets, at its place in
    /// the buffer so a scroll only moves it. A row nothing changed for comes from [`RowsKept`].
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
            // A row past the bottom edge paints nothing and keeps nothing, so a scroll that brings it in paints
            // it.
            let paints = one.line_top(line, self.line_h) <= one.bottom;
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
            let top = one.line_top(line, self.line_h) - scroll;
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
        self.rows_kept
            .retain(rows.first_line..rows.first_line.saturating_add(rows.n_visible));
        pieces
    }

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

    pub(super) fn note_motion(&mut self, theme: &Theme) {
        self.scroll.motion = theme.motion();
    }

    pub(super) fn forget_shown(&mut self) {
        self.scroll.shown.forget();
    }

    #[cfg(test)]
    pub(super) fn rows_kept_counts(&self) -> (usize, usize) {
        (self.rows_kept.reused, self.rows_kept.painted)
    }

    /// From the test's draw: the lines [`RowsKept`] holds, and the bytes that take.
    #[cfg(test)]
    pub(super) fn rows_kept_lines(&self) -> Vec<usize> {
        self.rows_kept.rows.iter().map(|row| row.line).collect()
    }

    #[cfg(test)]
    pub(super) fn rows_kept_bytes(&self) -> usize {
        self.rows_kept.bytes
    }

    #[cfg(test)]
    pub(super) fn forget_rows_kept(&mut self) {
        self.rows_kept.clear();
    }

    /// From `collect_damage`: the lines that changed, came or went, and a scroll as a copy of the lines
    /// it only moved.
    pub(super) fn damage(&self, regions: &mut Vec<Rect>) {
        self.scroll.shown.damage_within(self, self.rect, regions);
    }

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
    use ui::widget::DrawChild;
    use ui::widget::Widget as _;
    use vello::Scene;
    use vello::kurbo::{Affine, Rect, Vec2};
    use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};

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

    /// Fingers move the lines pixel by pixel; a host that copies what a scroll keeps on screen repaints
    /// only the strip that came into view.
    #[test]
    fn fingers_scroll_the_lines_by_the_pixel_and_copy_them() {
        let mut doc = AutoCommit::new();
        let mut view = shown_lines(&mut doc);
        let viewport = view.rect;
        let theme = Theme::dark();
        let mut renderer = TextRenderer::new();
        let mut frame = |view: &mut EditorView| {
            view.draw_child(
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
