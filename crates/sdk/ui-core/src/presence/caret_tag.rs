//! The name tag beside a peer's caret: where it goes, and how a pointer
//! whose tag would meet it leaves its own name out, so one person shows
//! one tag.

use super::render::{PeerCursor, PillRequest, layout_peer_pills, peer_tag_size};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use vello::kurbo::Rect;

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

/// The room, in logical pixels, each name tag keeps clear around itself:
/// a person's pointer tag and caret tag closer than twice this read as one
/// cluster of the same name, so only the caret's shows.
pub const TAG_CLEARANCE: f64 = 6.0;

/// A caret name tag a frame painted: the name, and the tag's rect in scene
/// coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct CaretTag {
    pub label: String,
    pub rect: Rect,
}

/// Whether a caret tag and a pointer tag, each padded by
/// [`TAG_CLEARANCE`], overlap.
#[must_use]
pub fn tags_crowd(caret: Rect, pointer: Rect) -> bool {
    !caret
        .inflate(TAG_CLEARANCE, TAG_CLEARANCE)
        .intersect(pointer.inflate(TAG_CLEARANCE, TAG_CLEARANCE))
        .is_zero_area()
}

/// Leave out the name of each pointer whose name tag would crowd a caret
/// tag with the same name ([`tags_crowd`]): one person, one tag, however
/// far the caret and pointer are, only whether their tags meet. A pointer's
/// tag is measured where it goes beside its pointer within `viewport`, as
/// [`super::draw_peer_cursors`] places a lone name. `carets` are in the
/// pointers' coordinates. A pointer saying something keeps its bubble.
pub fn hide_names_beside_caret_tags(
    peers: &mut [PeerCursor],
    carets: &[CaretTag],
    viewport: Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    for peer in peers.iter_mut().filter(|peer| peer.chat.is_none()) {
        let mine: Vec<Rect> = carets
            .iter()
            .filter(|tag| tag.label == peer.label)
            .map(|tag| tag.rect)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let pointer = pointer_tag_rect(viewport, peer, text_renderer, theme);
        if mine.iter().any(|caret| tags_crowd(*caret, pointer)) {
            peer.name_opacity = 0.0;
        }
    }
}

/// Where `peer`'s name tag goes beside its pointer within `viewport`.
fn pointer_tag_rect(
    viewport: Rect,
    peer: &PeerCursor,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> Rect {
    let request = PillRequest {
        at: peer.pos,
        size: peer_tag_size(text_renderer, theme, peer),
    };
    layout_peer_pills(viewport, &[request])
        .first()
        .map_or(Rect::ZERO, |place| place.rect)
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
        let theme = Theme::default();
        let caret = LabeledCaret::within(Rect::new(30.0, 40.0, 32.0, 60.0), 40.0);
        ctx.draw_peer_caret_labeled_in(
            &mut scene,
            &theme,
            &mut text,
            Affine::scale(2.0),
            &UserId("ada".into()),
            "Ada",
            caret,
        );
        let (w, h) = crate::presence::render::peer_pill_size(&mut text, &theme, "Ada");
        assert_eq!(
            ctx.caret_tags(),
            vec![CaretTag {
                label: "Ada".into(),
                rect: Affine::scale(2.0).transform_rect_bbox(caret.chip_rect(w, h)),
            }]
        );
        ctx.clear_caret_tags();
        assert!(ctx.caret_tags().is_empty());
    }

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);

    /// Which of `pointers` keep their names beside `tags`.
    fn shown(mut pointers: Vec<PeerCursor>, tags: &[CaretTag]) -> Vec<f32> {
        let mut text = TextRenderer::new();
        hide_names_beside_caret_tags(&mut pointers, tags, VIEWPORT, &mut text, &Theme::default());
        pointers.iter().map(|p| p.name_opacity).collect()
    }

    /// A caret at `x` on a note's first line, from y 100 to 120: its 90 px
    /// wide tag hangs below it.
    fn first_line_tag(label: &str, x: f64) -> CaretTag {
        let caret = LabeledCaret::within(Rect::new(x, 100.0, x + 2.0, 120.0), 100.0);
        CaretTag {
            label: label.into(),
            rect: caret.chip_rect(90.0, 20.0),
        }
    }

    #[test]
    fn a_pointer_86_px_along_the_line_from_its_persons_caret_leaves_the_name_to_the_caret() {
        // The pointer a little right of the caret on the same line puts its
        // tag below right of its tip, a few pixels from the caret's tag:
        // the same name twice, side by side.
        let color = vello::peniko::Color::WHITE;
        let tags = [first_line_tag("Nimble Puffin", 500.0)];
        let pointers = vec![
            PeerCursor::new((586.0, 110.0), "Nimble Puffin", color),
            PeerCursor::new((586.0, 110.0), "Calm Owl", color),
        ];
        assert_eq!(shown(pointers, &tags), vec![0.0, 1.0]);
    }

    #[test]
    fn a_pointer_whose_tag_stays_clear_of_its_persons_caret_tag_keeps_its_name() {
        let color = vello::peniko::Color::WHITE;
        let tags = [first_line_tag("Nimble Puffin", 500.0)];
        let pointers = vec![
            PeerCursor::new((760.0, 110.0), "Nimble Puffin", color),
            PeerCursor::new((540.0, 400.0), "Nimble Puffin", color),
        ];
        assert_eq!(shown(pointers, &tags), vec![1.0, 1.0]);
    }

    #[test]
    fn tags_crowd_when_closer_than_their_clearance() {
        let caret = Rect::new(0.0, 0.0, 90.0, 20.0);
        let apart = |space: f64| Rect::new(90.0 + space, 0.0, 180.0 + space, 20.0);
        assert!(tags_crowd(caret, apart(2.0 * TAG_CLEARANCE - 1.0)));
        assert!(!tags_crowd(caret, apart(2.0 * TAG_CLEARANCE + 1.0)));
    }

    #[test]
    fn a_pointer_saying_something_keeps_its_bubble_beside_the_caret() {
        let mut talking =
            PeerCursor::new((586.0, 110.0), "Nimble Puffin", vello::peniko::Color::WHITE);
        talking.chat = Some(crate::presence::ChatBubble {
            text: "here".into(),
            typing: false,
            opacity: 1.0,
        });
        let tags = [first_line_tag("Nimble Puffin", 500.0)];
        assert_eq!(shown(vec![talking], &tags), vec![1.0]);
    }
}
