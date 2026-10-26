//! The editor's vertical scroll as every toolkit scroller does it: the wheel
//! and fingers move the lines through the shared momentum
//! ([`ui::ScrollMomentum`]), in whole device pixels and through the rubber
//! band past either end; and each frame says which lines it drew where, as
//! if unscrolled ([`ui::scroll_copy::PaintedFrames`]), so a scroll repaints
//! only the lines that came into view and a host copies the rest.

use ui::scroll_copy::{Around, Painted, PaintedFrames, Piece, Showing, piece_key};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use winit::event::{MouseScrollDelta, TouchPhase};

use super::{EditorView, PaintCtx, VisibleRows};

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
    /// the same. Returns the lines as the frame's pieces.
    pub(super) fn draw_lines(
        &self,
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
        let mut pieces = Vec::with_capacity(rows.n_visible);
        for line in rows.first_line..rows.first_line + rows.n_visible {
            if line >= self.lines.len() {
                break;
            }
            let one = VisibleRows {
                first_line: line,
                n_visible: 1,
                ..unscrolled
            };
            let mut drawn = Scene::new();
            let mut row = PaintCtx::new(&mut drawn, transform, &mut *text, theme);
            self.draw_line_backgrounds(&mut row, &one, primary_line);
            self.draw_line_text(&mut row, &one, primary_line);
            self.draw_diagnostics(&mut drawn, transform, theme, &one);
            self.draw_carets(&mut drawn, transform, theme, &one);
            scene.append(&drawn, Some(scrolled));
            // A line's glyphs may reach a line past it either way.
            let top = one.line_top(line, self.line_h) - scroll;
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
        }
        pieces
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
