//! What a frame paints, and what it leaves alone.
//!
//! A conversation longer than its window shows a few rows; the rest lie above and
//! below the viewport, out of sight and — because a row is only ever shaped when
//! it is painted — out of the frame's work. These tests hold the list to that: the
//! rows a frame paints are the rows that meet the viewport, and a row outside it
//! is not shaped.

use super::*;

/// 2026-06-30T14:05:00Z.
const NOONISH: i64 = 1_782_828_300;

const WIDTH: f64 = 900.0;

/// A conversation long enough that its window shows a fraction of it.
fn thread(count: usize) -> Vec<ConversationLine> {
    (0..count)
        .map(|index| {
            let at_ms = NOONISH
                .saturating_add(i64::try_from(index).unwrap_or_default() * 60)
                .saturating_mul(1_000);
            let body = format!("Message {index}: the frame is on the table.");
            if index % 2 == 0 {
                ConversationLine::outgoing(format!("m{index}"), at_ms, body)
            } else {
                ConversationLine::incoming(format!("m{index}"), at_ms, "Ada Chen", body)
            }
        })
        .collect()
}

#[test]
fn a_frame_paints_the_rows_that_meet_its_viewport() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = thread(200);
    let face = ListFace::new(&lines, ConversationClock::at(0, NOONISH));
    let mut cache = ListCache::default();
    let height = face.height_in(&mut cache, &mut text, &theme, WIDTH);
    let list = Rect::new(0.0, 0.0, WIDTH, height);
    let rows = cache.layout.rows.len();
    let mut scene = Scene::new();

    // Nothing scrolled yet: the window is the conversation's top.
    let top = Rect::new(0.0, 0.0, WIDTH, 800.0);
    {
        let mut paint = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        face.paint_in(&mut paint, list, Some(top), &mut cache);
    }
    let work = cache.work();
    assert!(work.painted > 0, "the top of the thread is in view");
    assert_eq!(
        work.painted + work.culled,
        rows,
        "every row is accounted for"
    );
    // The rows painted are exactly the rows in the window.
    let in_view = cache
        .layout
        .rows
        .iter()
        .filter(|row| row.meets(top))
        .count();
    assert_eq!(
        work.painted, in_view,
        "the painted rows are the ones in view"
    );

    // Scrolled a long way down, the same window paints the same number of rows
    // out of a conversation ten times longer.
    let mut longer = ListCache::default();
    let longer_lines = thread(2_000);
    let longer_face = ListFace::new(&longer_lines, ConversationClock::at(0, NOONISH));
    let longer_height = longer_face.height_in(&mut longer, &mut text, &theme, WIDTH);
    let longer_list = Rect::new(0.0, 0.0, WIDTH, longer_height);
    let middle = Rect::new(0.0, 4_000.0, WIDTH, 4_800.0);
    {
        let mut paint = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        face.paint_in(&mut paint, list, Some(middle), &mut cache);
    }
    let near = cache.work();
    {
        let mut paint = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        longer_face.paint_in(&mut paint, longer_list, Some(middle), &mut longer);
    }
    let far = longer.work();
    assert_eq!(
        near.painted, far.painted,
        "the same window paints the same rows of a ten times longer thread"
    );
    assert!(
        far.culled > near.culled,
        "the longer thread has more rows to leave alone: {near:?} {far:?}"
    );
}

/// The rows outside the window are not shaped at all: the frame's text work is
/// the work of the screen.
#[test]
fn rows_outside_the_viewport_are_not_shaped() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = thread(200);
    let face = ListFace::new(&lines, ConversationClock::at(0, NOONISH));
    let mut cache = ListCache::default();
    let height = face.height_in(&mut cache, &mut text, &theme, WIDTH);
    let list = Rect::new(0.0, 0.0, WIDTH, height);
    let mut scene = Scene::new();

    // Every body is unique, so a paint of the whole conversation shapes every
    // line of it.
    {
        let mut paint = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        face.paint_in(&mut paint, list, None, &mut cache);
        let (_, misses, _) = text.take_shape_stats();
        assert!(
            misses >= 200,
            "painting 200 unseen bodies shapes them: {misses} misses"
        );
        assert_eq!(cache.work().painted, cache.layout.rows.len());
        assert_eq!(cache.work().culled, 0);
    }

    // A window over the middle of the same conversation shapes only what it shows:
    // a handful of bodies and their captions.
    scene.reset();
    let middle = Rect::new(0.0, 4_000.0, WIDTH, 4_800.0);
    let (hits, misses, _) = {
        let mut paint = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        face.paint_in(&mut paint, list, Some(middle), &mut cache);
        text.take_shape_stats()
    };
    let shapes = hits.saturating_add(misses);
    assert!(
        shapes <= 40,
        "a window of eight hundred points shapes the rows it shows, not two hundred: \
         {shapes} shapes for {} painted rows",
        cache.work().painted
    );
    assert!(cache.work().painted <= 20, "{:?}", cache.work());
}
