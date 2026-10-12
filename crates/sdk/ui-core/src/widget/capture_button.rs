//! The capture button over a camera's picture: the shutter.
//!
//! A ring in media ink around a disc (a photo), a dot in danger red (a
//! video), or a square in danger red inside a red ring (recording). It sits
//! on the media itself, so it is inked from `colors.media`, never from the
//! theme's surfaces; it lifts under the pointer, takes your focus ring and
//! names what pressing it does.

use vello::kurbo::{Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use crate::paint::PaintCtx;
use crate::theme::Theme;

/// What pressing the capture button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureKind {
    /// Take a photo.
    Photo,
    /// Start recording a video.
    Video,
    /// Stop the recording in progress.
    Recording,
}

impl CaptureKind {
    /// What the button is called: what pressing it does.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Photo => "Take photo",
            Self::Video => "Start recording",
            Self::Recording => "Stop recording",
        }
    }
}

/// Immediate-mode capture button.
#[derive(Clone, Copy, Debug)]
pub struct CaptureButtonFace {
    kind: CaptureKind,
    /// The camera is live: the ring is full media ink, else dimmed.
    live: bool,
    hovered: bool,
    pressed: bool,
    focused: bool,
}

impl CaptureButtonFace {
    #[must_use]
    pub fn new(kind: CaptureKind) -> Self {
        Self {
            kind,
            live: true,
            hovered: false,
            pressed: false,
            focused: false,
        }
    }

    /// Whether the camera is live; a button over no picture is dimmed.
    #[must_use]
    pub fn live(mut self, live: bool) -> Self {
        self.live = live;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    #[must_use]
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.pressed = pressed;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The button's diameter in `theme`.
    #[must_use]
    pub fn diameter(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.min_pointer_target) * 2.0
    }

    /// Where the button is when centred on `center`: its hit rect.
    #[must_use]
    pub fn rect(center: Point, theme: &Theme) -> Rect {
        let side = Self::diameter(theme);
        Rect::from_center_size(center, (side, side))
    }

    /// Paint the button centred on `center` and return its hit rect.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, center: Point) -> Rect {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let rect = Self::rect(center, theme);
        let radius = rect.width() / 2.0;
        let ring = f64::from(spacing.s1);
        let ink = if self.live {
            colors.media.ink
        } else {
            colors.media.ink_dim
        };
        let danger = colors.semantic.danger;
        let recording = self.kind == CaptureKind::Recording;
        ctx.scene.stroke(
            &Stroke::new(ring),
            ctx.transform,
            if recording { danger } else { ink },
            None,
            &Circle::new(center, radius - ring / 2.0),
        );
        // The core gives under the pointer: smaller while pressed, a touch
        // larger while hovered.
        let gap = f64::from(spacing.s1);
        let mut core = radius - ring - gap;
        if self.pressed {
            core -= gap;
        } else if self.hovered && self.live {
            core += gap / 2.0;
        }
        match self.kind {
            CaptureKind::Recording => {
                let half = core / 2.0;
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    danger,
                    None,
                    &RoundedRect::from_rect(
                        Rect::from_center_size(center, (half * 2.0, half * 2.0)),
                        f64::from(theme.tokens.radii.detail),
                    ),
                );
            }
            CaptureKind::Video => {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    danger,
                    None,
                    &Circle::new(center, core),
                );
            }
            CaptureKind::Photo => {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    ink,
                    None,
                    &Circle::new(center, core),
                );
            }
        }
        if self.focused {
            crate::widget::paint_focus_ring(ctx.scene, ctx.transform, rect, radius, theme);
        }
        rect
    }

    /// The button's node for assistive technology at `rect`.
    pub fn accessibility(&self, rect: Rect) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        AccessNode::new(Role::Button)
            .with_name(self.kind.name().to_owned())
            .with_bounds(rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.focused)
                    .disabled(!self.live),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::TextRenderer;
    use vello::Scene;
    use vello::kurbo::Affine;

    #[test]
    fn the_capture_button_names_what_it_does_and_rings_its_focus() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let center = Point::new(100.0, 100.0);
        let paint = |face: CaptureButtonFace, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            let rect = face.paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
                center,
            );
            (rect, scene.encoding().draw_tags.len())
        };
        let (rect, plain) = paint(CaptureButtonFace::new(CaptureKind::Photo), &mut text);
        let (_, focused) = paint(
            CaptureButtonFace::new(CaptureKind::Photo).focused(true),
            &mut text,
        );
        assert!(focused > plain, "focus adds its ring");
        assert!((rect.width() - CaptureButtonFace::diameter(&theme)).abs() < 1e-9);
        let node = CaptureButtonFace::new(CaptureKind::Recording).accessibility(rect);
        assert_eq!(node.name.as_deref(), Some("Stop recording"));
    }
}
