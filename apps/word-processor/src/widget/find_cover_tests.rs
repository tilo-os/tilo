//! The window's search stands its options over the top of the document; the
//! editor shows the current match clear of them, scrolling before the
//! document's start where the match is near it.

use super::*;
use ui::headless::Framed;
use ui::widget::{FindOptions, Findable};

const W: u32 = 420;
const H: u32 = 760;

/// The current match's rects on screen.
fn current_match(wp: &WordProcessorWidget) -> Vec<Rect> {
    let (block, start, end) = wp.find.active_match().cloned().expect("a match is current");
    let anchor = tilo_actions::InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block,
            start: u64::try_from(start).expect("a small offset"),
            end: u64::try_from(end).expect("a small offset"),
        }],
    };
    let transform = wp.content_transform(Affine::IDENTITY);
    wp.semantic_text_rects(&anchor, transform, wp.zoom_viewport)
        .expect("the match is laid out")
        .into_iter()
        .map(|rect| transform.transform_rect_bbox(rect))
        .collect()
}

fn draw(framed: &mut Framed<WordProcessorWidget>) -> &mut WordProcessorWidget {
    super::tests::pipeline_draw(framed, W, H).0
}

#[test]
fn the_current_match_is_revealed_clear_of_the_search_options_over_it() {
    let mut state = crate::initialized_state();
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    let mut edits = ui::widget::Edits::lend(&mut state.doc);
    wp.model_mut().insert_str(&mut edits, "Quarterly report");
    wp.model_mut().enter(&mut edits);
    wp.model_mut()
        .insert_str(&mut edits, "The report covers costs and hiring.");
    let mut framed = super::tests::mount(state, wp, W, H);
    // The paragraphs are read with the document, on the next frame.
    Findable::find(draw(&mut framed), "report", FindOptions::default());
    let wp = draw(&mut framed);
    assert_eq!(
        Findable::find(wp, "report", FindOptions::default()).len(),
        2
    );
    Findable::reveal(wp, 0);
    let wp = draw(&mut framed);
    let shown = current_match(wp);
    let first = shown.first().copied().expect("the match is on screen");

    // The options open under the bar's field, over the match.
    let cover = Rect::new(first.x0 - 24.0, 0.0, first.x1 + 24.0, first.y1 + 120.0);
    Findable::set_find_cover(wp, Some(cover));
    Findable::reveal(wp, 0);
    let wp = draw(&mut framed);
    for rect in current_match(wp) {
        assert!(
            rect.y0 >= cover.y1,
            "the first match {rect:?} stands clear of the options {cover:?}"
        );
    }
    let metrics = wp.canvas_scroll_metrics();
    assert!(
        metrics.offset >= 0.0 && metrics.offset <= metrics.max_scroll(),
        "the room before the start counts as content: {metrics:?}"
    );
    Findable::reveal(wp, 1);
    let wp = draw(&mut framed);
    for rect in current_match(wp) {
        assert!(
            rect.y0 >= cover.y1,
            "the next match {rect:?} stands clear of the options {cover:?}"
        );
    }

    // The options close: the document comes back to its start.
    Findable::set_find_cover(wp, None);
    let wp = draw(&mut framed);
    assert!(
        wp.scroll_y >= 0.0,
        "nothing covers the document: it starts at its start ({})",
        wp.scroll_y
    );
}
