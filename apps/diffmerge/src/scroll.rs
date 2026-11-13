//! Scrolling the diff and merge panes share: how far a wheel event asks a pane
//! to move, and the horizontal offset the code columns of a pane have in
//! common, with the framework's scroll bar model along the foot of each.

use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

const WHEEL_ROWS: u8 = 3;

pub fn wheel_pixels(delta: MouseScrollDelta, row_h: f64, shift: bool) -> (f64, f64) {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (
            -f64::from(x) * row_h * f64::from(WHEEL_ROWS),
            -f64::from(y) * row_h * f64::from(WHEEL_ROWS),
        ),
        MouseScrollDelta::PixelDelta(position) => (-position.x, -position.y),
    };
    if shift && dx == 0.0 {
        (dy, 0.0)
    } else {
        (dx, dy)
    }
}

#[derive(Debug, Default)]
pub struct HScroll {
    offset: f64,
    bar: Option<Scrollbar>,
    tracks: Vec<Rect>,
    content: f64,
    viewport: f64,
}

impl HScroll {
    pub fn offset(&self) -> f64 {
        self.offset
    }

    fn bar(&mut self) -> &mut Scrollbar {
        self.bar.get_or_insert_with(Scrollbar::horizontal)
    }

    fn metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content, self.viewport, self.offset)
    }

    pub fn set_extent(&mut self, content: f64, viewport: f64) {
        self.content = content;
        self.viewport = viewport;
        self.offset = self.offset.clamp(0.0, self.metrics().max_scroll());
    }

    pub fn reset(&mut self) {
        self.offset = 0.0;
    }

    pub fn scroll_by(&mut self, dx: f64) -> bool {
        let next = (self.offset + dx).clamp(0.0, self.metrics().max_scroll());
        let moved = (next - self.offset).abs() > f64::EPSILON;
        self.offset = next;
        moved
    }

    pub fn overflows(&self) -> bool {
        self.metrics().overflows()
    }

    pub fn track_in(view: Rect, theme: &Theme) -> Rect {
        let margin = f64::from(theme.tokens.spacing.s1) / 2.0;
        let thickness = f64::from(theme.tokens.spacing.s2);
        Rect::new(
            view.x0 + margin,
            view.y1 - margin - thickness,
            view.x1 - margin,
            view.y1 - margin,
        )
    }

    pub fn draw(&mut self, ctx: &mut PaintCtx<'_>, tracks: &[Rect]) {
        self.tracks.clear();
        if !self.overflows() {
            return;
        }
        self.tracks.extend_from_slice(tracks);
        let metrics = self.metrics();
        let bar = *self.bar();
        for track in tracks {
            bar.draw(ctx.scene, ctx.transform, *track, metrics, ctx.theme, false);
        }
    }

    pub fn pointer(&mut self, event: &WindowEvent, cursor: Point, moved: &mut bool) -> bool {
        let metrics = self.metrics();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                for track in self.tracks.clone() {
                    match self.bar().press(track, metrics, cursor) {
                        PressOutcome::Miss => {}
                        PressOutcome::Thumb => return true,
                        PressOutcome::Page(offset) => {
                            *moved |= (offset - self.offset).abs() > f64::EPSILON;
                            self.offset = offset;
                            return true;
                        }
                    }
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.bar().release(),
            WindowEvent::CursorMoved { .. } => {
                let dragged = self
                    .tracks
                    .first()
                    .copied()
                    .and_then(|track| self.bar().drag_to(track, metrics, cursor));
                if let Some(offset) = dragged {
                    *moved |= (offset - self.offset).abs() > f64::EPSILON;
                    self.offset = offset;
                    return true;
                }
                false
            }
            _ => false,
        }
    }
}
