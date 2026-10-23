//! The name tag beside a peer's caret: where it goes, and how a pointer
//! beside it leaves its own name out, so one person shows one tag.

use super::render::PeerCursor;
use vello::kurbo::{Point, Rect};

/// A peer's caret to label, and the highest its name chip may reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabeledCaret {
    /// The caret bar, in the widget's own coordinates.
    pub rect: Rect,
    /// The top of the text the caret is in, in the same coordinates: a
    /// chip that would rise above it hangs below the caret instead.
    pub top: f64,
}

impl LabeledCaret {
    /// A caret whose chip may rise as far as it needs.
    #[must_use]
    pub fn new(rect: Rect) -> Self {
        Self {
            rect,
            top: f64::NEG_INFINITY,
        }
    }

    /// A caret in text whose first line starts at `top`.
    #[must_use]
    pub fn within(rect: Rect, top: f64) -> Self {
        Self { rect, top }
    }

    /// Where a `w` by `h` name chip goes: above the caret, or below it when
    /// above would cross [`Self::top`].
    #[must_use]
    pub fn chip_rect(&self, w: f64, h: f64) -> Rect {
        let x = self.rect.x0;
        let above = self.rect.y0 - h - CARET_CHIP_GAP;
        let y = if above < self.top {
            self.rect.y1 + CARET_CHIP_GAP
        } else {
            above
        };
        Rect::new(x, y, x + w, y + h)
    }
}

/// Space between a caret and its name chip.
const CARET_CHIP_GAP: f64 = 2.0;

/// How close, in logical pixels, a person's pointer may come to their own
/// caret before the two share one name tag, the caret's.
pub const CARET_POINTER_TAG_REACH: f64 = 40.0;

/// A caret name tag a frame painted: the name, and the caret's middle left
/// edge in scene coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct CaretTag {
    pub label: String,
    pub at: Point,
}

/// Leave out the name of each pointer within [`CARET_POINTER_TAG_REACH`] of
/// a caret tag with the same name: one person, one tag. `carets` are in the
/// pointers' coordinates. A pointer saying something keeps its bubble.
pub fn hide_names_beside_caret_tags(peers: &mut [PeerCursor], carets: &[CaretTag]) {
    for peer in peers.iter_mut().filter(|peer| peer.chat.is_none()) {
        let at = Point::new(peer.pos.0, peer.pos.1);
        if carets
            .iter()
            .any(|tag| tag.label == peer.label && tag.at.distance(at) <= CARET_POINTER_TAG_REACH)
        {
            peer.name_opacity = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::UserId;
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use vello::Scene;
    use vello::kurbo::Affine;

    #[test]
    fn a_chip_rises_above_its_caret_while_there_is_room() {
        let caret = LabeledCaret::within(Rect::new(30.0, 100.0, 32.0, 120.0), 40.0);
        let chip = caret.chip_rect(80.0, 20.0);
        assert!(chip.y1 <= caret.rect.y0, "above the caret: {chip:?}");
        assert!(chip.y0 >= caret.top, "inside the text: {chip:?}");
    }

    #[test]
    fn a_chip_on_the_first_line_hangs_below_instead_of_covering_what_is_above() {
        let caret = LabeledCaret::within(Rect::new(30.0, 40.0, 32.0, 60.0), 40.0);
        let chip = caret.chip_rect(80.0, 20.0);
        assert!(chip.y0 >= caret.rect.y1, "below the caret: {chip:?}");
        assert!((chip.x0 - caret.rect.x0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_caret_with_no_top_keeps_its_chip_above() {
        let caret = LabeledCaret::new(Rect::new(30.0, 4.0, 32.0, 24.0));
        assert!(caret.chip_rect(80.0, 20.0).y1 <= 4.0);
    }

    #[test]
    fn drawing_a_labeled_caret_notes_its_tag_in_scene_coordinates() {
        let ctx = PresenceCtx::empty();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        ctx.draw_peer_caret_labeled_in(
            &mut scene,
            &Theme::default(),
            &mut text,
            Affine::scale(2.0),
            &UserId("ada".into()),
            "Ada",
            LabeledCaret::within(Rect::new(30.0, 40.0, 32.0, 60.0), 40.0),
        );
        assert_eq!(
            ctx.caret_tags(),
            vec![CaretTag {
                label: "Ada".into(),
                at: Point::new(60.0, 100.0),
            }]
        );
        ctx.clear_caret_tags();
        assert!(ctx.caret_tags().is_empty());
    }

    #[test]
    fn a_pointer_beside_its_persons_caret_leaves_the_name_to_the_caret() {
        let color = vello::peniko::Color::WHITE;
        let tags = [CaretTag {
            label: "Ada".into(),
            at: Point::new(100.0, 100.0),
        }];
        let mut pointers = vec![
            PeerCursor::new((120.0, 110.0), "Ada", color),
            PeerCursor::new((200.0, 100.0), "Ada", color),
            PeerCursor::new((105.0, 100.0), "Grace", color),
        ];
        hide_names_beside_caret_tags(&mut pointers, &tags);
        let shown: Vec<f32> = pointers.iter().map(|p| p.name_opacity).collect();
        assert_eq!(shown, vec![0.0, 1.0, 1.0]);
    }

    #[test]
    fn a_pointer_saying_something_keeps_its_bubble_beside_the_caret() {
        let mut talking = PeerCursor::new((100.0, 100.0), "Ada", vello::peniko::Color::WHITE);
        talking.chat = Some(crate::presence::ChatBubble {
            text: "here".into(),
            typing: false,
            opacity: 1.0,
        });
        let tags = [CaretTag {
            label: "Ada".into(),
            at: Point::new(100.0, 100.0),
        }];
        let mut pointers = vec![talking];
        hide_names_beside_caret_tags(&mut pointers, &tags);
        assert!((pointers[0].name_opacity - 1.0).abs() < f32::EPSILON);
    }
}
