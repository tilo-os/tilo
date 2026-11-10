//! The name tag beside a peer's caret: where it goes, and how a pointer
//! whose tag would meet it leaves its own name out, so one person shows
//! one tag.

use super::render::{PeerCursor, PillRequest, layout_peer_pills, peer_tag_size};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use vello::kurbo::Rect;

/// A peer's caret to label, the highest its name chip may reach, and where
/// the text around it has glyphs, which the chip never covers.
#[derive(Debug, Clone, PartialEq)]
pub struct LabeledCaret {
    /// The caret bar, in the widget's own coordinates.
    pub rect: Rect,
    /// The top of the text the caret is in, in the same coordinates: a
    /// chip that would rise above it hangs below the caret instead.
    pub top: f64,
    /// Where the text around the caret has glyphs, one rect per visual
    /// line (its own line's, and the lines above and below it). Empty when
    /// the caller does not know: the chip then hangs a line above.
    pub ink: Vec<Rect>,
    /// The right edge of the text's column: a chip beside the caret ends
    /// before it.
    pub right: f64,
}

impl LabeledCaret {
    /// A caret whose chip may rise as far as it needs.
    #[must_use]
    pub fn new(rect: Rect) -> Self {
        Self {
            rect,
            top: f64::NEG_INFINITY,
            ink: Vec::new(),
            right: f64::INFINITY,
        }
    }

    /// A caret in text whose first line starts at `top`.
    #[must_use]
    pub fn within(rect: Rect, top: f64) -> Self {
        Self {
            top,
            ..Self::new(rect)
        }
    }

    /// The glyphs around the caret, one rect per visual line, and the right
    /// edge of the text's column: the chip goes where none of them are.
    #[must_use]
    pub fn among(mut self, ink: Vec<Rect>, right: f64) -> Self {
        self.ink = ink;
        self.right = right;
        self
    }

    /// Where a `w` by `h` name chip goes.
    ///
    /// Where the glyphs around the caret are known ([`Self::among`]), the chip
    /// never covers one: right of the caret on its own line where the line has
    /// room there (an agent writing at the end of a line), else in the gap
    /// above the caret's line where no glyph is, else below the line. Hung a
    /// line above, it sat on the previous line's words ("write it."); pinned
    /// to the caret's top, across its own line's.
    ///
    /// Not knowing the glyphs, it hangs one line box clear above the caret,
    /// or below it when that would cross [`Self::top`].
    #[must_use]
    pub fn chip_rect(&self, w: f64, h: f64) -> Rect {
        if self.ink.is_empty() {
            let x = self.rect.x0;
            let line = self.rect.height().max(0.0);
            let above = self.rect.y0 - line - h - CARET_CHIP_GAP;
            let y = if above < self.top {
                self.rect.y1 + CARET_CHIP_GAP
            } else {
                above
            };
            return Rect::new(x, y, x + w, y + h);
        }
        let covers = |chip: Rect| self.ink.iter().any(|glyphs| overlaps(chip, *glyphs));
        // The caret's own line box: the caret, and its line's glyphs where it stands among them.
        let centre = self.rect.center().y;
        let (line_top, line_bottom) = self
            .ink
            .iter()
            .filter(|glyphs| glyphs.y0 <= centre && centre <= glyphs.y1)
            .fold((self.rect.y0, self.rect.y1), |(top, bottom), glyphs| {
                (top.min(glyphs.y0), bottom.max(glyphs.y1))
            });
        let beside_x = self.rect.x1 + 2.0 * CARET_CHIP_GAP;
        let beside_y = f64::midpoint(line_top, line_bottom) - h / 2.0;
        let beside = Rect::new(beside_x, beside_y, beside_x + w, beside_y + h);
        if beside.x1 <= self.right && !covers(beside) {
            return beside;
        }
        // Above and below start at the caret and keep inside the column where it allows.
        let x = self.rect.x0.min(self.right - w).max(self.rect.x0 - w);
        let above_y = line_top - CARET_CHIP_GAP - h;
        let above = Rect::new(x, above_y, x + w, above_y + h);
        if above_y >= self.top && !covers(above) {
            return above;
        }
        let below_y = line_bottom + CARET_CHIP_GAP;
        Rect::new(x, below_y, x + w, below_y + h)
    }
}

/// Whether `a` and `b` share any area.
fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// Space between a caret and its name chip.
const CARET_CHIP_GAP: f64 = 2.0;

/// A caret name tag a frame painted: the name, and the tag's rect in scene
/// coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct CaretTag {
    pub label: String,
    pub rect: Rect,
}

/// The gap between two tag rects: zero when they touch or overlap, else
/// the straight distance between their nearest edges.
#[must_use]
pub fn tag_gap(a: Rect, b: Rect) -> f64 {
    let dx = (a.x0 - b.x1).max(b.x0 - a.x1).max(0.0);
    let dy = (a.y0 - b.y1).max(b.y0 - a.y1).max(0.0);
    dx.hypot(dy)
}

/// Whether a caret tag and a pointer tag of the same person read as one
/// name twice: their gap is within one tag height (the taller of the two),
/// so the eye takes them as one cluster.
#[must_use]
pub fn tags_crowd(caret: Rect, pointer: Rect) -> bool {
    tag_gap(caret, pointer) <= caret.height().max(pointer.height())
}

/// Leave out the name of each pointer whose name tag would crowd a caret
/// tag with the same name ([`tags_crowd`]): one person, one tag, wherever
/// the two tags would sit within a tag height of each other. A pointer's
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
        assert!(
            chip.y1 <= caret.rect.y0 - caret.rect.height(),
            "a line above the caret, clear of its words: {chip:?}"
        );
    }

    #[test]
    fn a_chip_on_the_first_line_hangs_below_instead_of_covering_what_is_above() {
        let caret = LabeledCaret::within(Rect::new(30.0, 40.0, 32.0, 60.0), 40.0);
        let chip = caret.chip_rect(80.0, 20.0);
        assert!(chip.y0 >= caret.rect.y1, "below the caret: {chip:?}");
        assert!((chip.x0 - caret.rect.x0).abs() < f64::EPSILON);
    }

    /// Lines of text 20 px tall at y = 100, 120, 140, the column 300 px wide:
    /// each visual line's glyphs from x = 10 to `ends[i]`.
    fn lines(ends: &[f64]) -> Vec<Rect> {
        ends.iter()
            .enumerate()
            .map(|(i, end)| {
                let y = 100.0 + 20.0 * i as f64;
                Rect::new(10.0, y, *end, y + 20.0)
            })
            .collect()
    }

    fn covers_none(chip: Rect, ink: &[Rect]) -> bool {
        ink.iter().all(|glyphs| !overlaps(chip, *glyphs))
    }

    /// An agent writing at the end of a line has its name right of the caret,
    /// on that line, covering none of the text above or before it.
    #[test]
    fn a_chip_stands_beside_a_caret_at_the_end_of_its_line() {
        // A full line above, the caret's own line ending at x = 120.
        let ink = lines(&[290.0, 120.0]);
        let caret = LabeledCaret::within(Rect::new(120.0, 120.0, 122.0, 140.0), 100.0)
            .among(ink.clone(), 300.0);
        let chip = caret.chip_rect(90.0, 18.0);
        assert!(chip.x0 >= caret.rect.x1, "right of the caret: {chip:?}");
        assert!(
            chip.y0 >= 120.0 && chip.y1 <= 140.0,
            "on its own line: {chip:?}"
        );
        assert!(covers_none(chip, &ink), "covering no text: {chip:?}");
    }

    /// With text right of the caret, or no room before the column's edge,
    /// the chip goes into the gap above where that gap has no glyphs.
    #[test]
    fn a_chip_rises_into_a_free_gap_when_its_line_is_full() {
        // A short line above (to x = 40), the caret mid-way along a full line.
        let ink = lines(&[40.0, 290.0]);
        let caret = LabeledCaret::within(Rect::new(150.0, 120.0, 152.0, 140.0), 0.0)
            .among(ink.clone(), 300.0);
        let chip = caret.chip_rect(90.0, 18.0);
        assert!(chip.y1 <= 120.0, "above the caret's line: {chip:?}");
        assert!(covers_none(chip, &ink), "covering no text: {chip:?}");
        // At the end of a line that reaches the column's edge, there is no room beside it.
        let ink = lines(&[40.0, 280.0]);
        let caret = LabeledCaret::within(Rect::new(280.0, 120.0, 282.0, 140.0), 0.0)
            .among(ink.clone(), 300.0);
        let chip = caret.chip_rect(90.0, 18.0);
        assert!(chip.x1 <= 300.0, "inside the column: {chip:?}");
        assert!(covers_none(chip, &ink), "covering no text: {chip:?}");
    }

    /// Text above and to the right: the chip hangs below the caret's line,
    /// never on the words above it.
    #[test]
    fn a_chip_hangs_below_when_text_stands_above_and_beside() {
        let ink = lines(&[290.0, 290.0]);
        let caret = LabeledCaret::within(Rect::new(150.0, 120.0, 152.0, 140.0), 0.0)
            .among(ink.clone(), 300.0);
        let chip = caret.chip_rect(90.0, 18.0);
        assert!(chip.y0 >= 140.0, "below the caret's line: {chip:?}");
        assert!(covers_none(chip, &ink), "covering no text: {chip:?}");
        // The wrapped paragraph the follower saw: "...as I / write it." above, the caret after
        // "of you." on the second line of the next paragraph, with room beside it.
        let ink = vec![
            Rect::new(10.0, 60.0, 290.0, 80.0),
            Rect::new(10.0, 80.0, 60.0, 100.0),
            Rect::new(10.0, 100.0, 290.0, 120.0),
            Rect::new(10.0, 120.0, 52.0, 140.0),
        ];
        let caret = LabeledCaret::within(Rect::new(52.0, 120.0, 54.0, 140.0), 0.0)
            .among(ink.clone(), 300.0);
        let chip = caret.chip_rect(130.0, 18.0);
        assert!(
            covers_none(chip, &ink),
            "\"write it.\" and its own line stay readable: {chip:?}"
        );
        assert!(chip.x0 >= caret.rect.x1, "beside \"of you.\": {chip:?}");
    }

    #[test]
    fn a_caret_with_no_top_keeps_its_chip_above() {
        let caret = LabeledCaret::new(Rect::new(30.0, 4.0, 32.0, 24.0));
        assert!(caret.chip_rect(80.0, 20.0).y1 <= 4.0 - 20.0);
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
            caret.clone(),
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

    #[test]
    fn drawing_a_caret_notes_where_a_reader_is_told_the_peer_is_writing() {
        let mut ctx = PresenceCtx::empty();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::default();
        let user = UserId("claude".into());
        ctx.set_display_name(user.clone(), "Claude Code");
        let caret = Rect::new(30.0, 40.0, 32.0, 60.0);
        ctx.draw_action_caret_labeled_in(
            &mut scene,
            &theme,
            &mut text,
            Affine::scale(2.0),
            &user,
            "Claude Code: Appending paragraph",
            LabeledCaret::within(caret, 40.0),
        );
        let marks = ctx.presence_marks();
        let [mark] = marks.as_slice() else {
            panic!("one caret mark");
        };
        assert_eq!(mark.user, user);
        assert_eq!(mark.rect, Affine::scale(2.0).transform_rect_bbox(caret));
        assert!(!mark.selection);
        assert_eq!(
            ctx.mark_words(mark),
            "Claude Code: Appending paragraph",
            "an action's own words say who is writing and what they are doing"
        );
        ctx.clear_presence_marks();
        assert!(ctx.presence_marks().is_empty());
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
    fn tags_crowd_within_one_tag_height() {
        let caret = Rect::new(0.0, 0.0, 90.0, 20.0);
        let apart = |space: f64| Rect::new(90.0 + space, 0.0, 180.0 + space, 20.0);
        assert!(tags_crowd(caret, apart(0.0)), "touching");
        // The shots' case: the same name twice, about 20 px apart.
        assert!(tags_crowd(caret, apart(19.0)));
        assert!(tags_crowd(caret, apart(20.0)));
        assert!(
            !tags_crowd(caret, apart(21.0)),
            "more than a tag height apart"
        );
        let below = |space: f64| Rect::new(30.0, 20.0 + space, 120.0, 40.0 + space);
        assert!(tags_crowd(caret, below(12.0)));
        assert!(!tags_crowd(caret, below(25.0)));
        assert!(
            tags_crowd(caret, Rect::new(10.0, 5.0, 100.0, 25.0)),
            "overlapping"
        );
    }

    #[test]
    fn a_pointer_tag_20_px_from_its_persons_caret_tag_leaves_the_name_to_the_caret() {
        let color = vello::peniko::Color::WHITE;
        let tags = [first_line_tag("Quiet Koala", 500.0)];
        let caret = tags[0].rect;
        // Find the pointer x whose tag starts about 20 px right of the
        // caret's tag, on the same line, as in the shots.
        let mut text = TextRenderer::new();
        let theme = Theme::default();
        let gap_at = |x: f64, text: &mut TextRenderer| {
            let peer = PeerCursor::new((x, 110.0), "Quiet Koala", color);
            tag_gap(caret, pointer_tag_rect(VIEWPORT, &peer, text, &theme))
        };
        let x = (560..900)
            .map(f64::from)
            .find(|x| gap_at(*x, &mut text) >= 20.0 - 1.0)
            .expect("some pointer puts its tag 20 px away");
        assert!(gap_at(x, &mut text) <= 21.0);
        let pointers = vec![
            PeerCursor::new((x, 110.0), "Quiet Koala", color),
            PeerCursor::new((x, 110.0), "Calm Owl", color),
        ];
        assert_eq!(shown(pointers, &tags), vec![0.0, 1.0]);
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
