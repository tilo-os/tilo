//! A step's status label stands where it covers no words the frame shows.
//!
//! Its surface, its ink and the chip it paints are all the frame's own: a
//! label must stay inside the surface and leave every worded line (a window's
//! heading beside the control, a notice above it) readable.

use super::PresenceCtx;
use crate::offscreen::OffscreenRenderer;
use crate::paint::PaintCtx;
use crate::permissions::UserId;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

/// A label that would sit over words the frame already shows (a window's own
/// heading beside the control, a notice above it) stands somewhere else inside
/// the surface instead.
#[test]
fn an_action_label_stands_clear_of_the_words_a_frame_shows() {
    let presence = PresenceCtx::empty();
    let user = UserId("charlie".into());
    let theme = Theme::default();
    let viewport = Rect::new(20.0, 20.0, 480.0, 180.0);
    let control = Rect::new(100.0, 60.0, 200.0, 90.0);
    // Everywhere a first place would go: beside the control and the whole strip
    // above it, which is the frame's own heading.
    let ink = [
        Rect::new(20.0, 20.0, 480.0, 59.0),
        Rect::new(204.0, 20.0, 480.0, 180.0),
    ];
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    presence.draw_action_label_within(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        &user,
        "Helper (Failed)",
        control,
        viewport,
        &ink,
    );
    let rgba = OffscreenRenderer::new(500, 200).render(&scene, Color::BLACK);
    let mut painted = 0;
    for y in 0..200 {
        for x in 0..500 {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 500, x, y);
            if r == 0 && g == 0 && b == 0 {
                continue;
            }
            painted += 1;
            let point = Point::new(f64::from(x), f64::from(y));
            assert!(viewport.contains(point), "activity escaped its surface");
            assert!(
                !ink.iter().any(|words| words.contains(point)),
                "the label covered words the frame shows, at {point:?}"
            );
        }
    }
    assert!(painted > 100, "the action label must actually be painted");
}
