//! What a kept layout promises: it is the layout, exactly.
//!
//! A list that keeps its measurements must place every row where laying it out
//! from nothing would — the same dividers, run gaps, author bands and bubbles —
//! and must measure again exactly the lines whose measure no longer holds. A
//! scrollbar that disagrees with where the last message ends is worse than a slow
//! one, so these tests compare the kept layout to the unkept one row by row.

use super::*;

/// 2026-06-30T14:05:00Z.
const NOONISH: i64 = 1_782_828_300;

fn clock() -> ConversationClock {
    ConversationClock::at(0, NOONISH)
}

/// Where a conversation is laid out: a column inside a window.
const LIST: Rect = Rect::new(24.0, 100.0, 924.0, 900.0);

/// Two people over three days, with runs of one person's lines, a line that
/// knowingly continues the run above it, and a delivery state under your own.
fn conversation() -> Vec<ConversationLine> {
    let at = |minutes: i64, day: i64| {
        NOONISH
            .saturating_add(day.saturating_mul(86_400))
            .saturating_add(minutes.saturating_mul(60))
            .saturating_mul(1_000)
    };
    vec![
        ConversationLine::incoming("a", at(0, 0), "Ada Chen", "are you nearby?"),
        ConversationLine::incoming("b", at(1, 0), "Ada Chen", "the call starts in ten"),
        ConversationLine::outgoing("c", at(2, 0), "on my way")
            .with_delivery(DeliveryState::Delivered),
        ConversationLine::incoming("d", at(3, 0), "Ada Chen", "+1"),
        ConversationLine::incoming(
            "e",
            at(4, 0),
            "Bo Park",
            "a sentence long enough to wrap inside the column, twice over, so the bubble it sits in is taller than one line of text and its own measurement is worth keeping",
        ),
        // The next day: a divider, and a fresh run for the same person.
        ConversationLine::incoming("f", at(0, 1), "Bo Park", "morning"),
        ConversationLine::outgoing("g", at(1, 1), "morning").with_delivery(DeliveryState::Read),
        // A line that knowingly continues the run above it, with no author.
        ConversationLine::incoming("h", at(2, 1), "", "and one more thing"),
        ConversationLine::incoming("i", at(3, 2), "Ada Chen", "three days later"),
    ]
}

/// The layout of `lines` in `rect`, laid out with nothing kept.
fn reference(
    text: &mut TextRenderer,
    theme: &Theme,
    lines: &[ConversationLine],
    rect: Rect,
) -> Layout {
    ListFace::new(lines, clock()).layout_for_test(text, theme, rect)
}

fn same_layout(kept: &Layout, laid: &Layout) {
    assert_eq!(
        kept.height.to_bits(),
        laid.height.to_bits(),
        "a kept layout is the same height as a laid one"
    );
    assert_eq!(kept.rows, laid.rows, "a kept layout is the same rows");
}

/// `lines` laid out into `cache` at [`LIST`], through the face a caller hands a
/// fresh slice to: the slice's contents are the conversation.
fn laid_out(
    lines: &[ConversationLine],
    cache: &mut ListCache,
    text: &mut TextRenderer,
    theme: &Theme,
) -> Layout {
    ListFace::new(lines, clock())
        .layout(cache, text, theme, LIST.x0, LIST.width(), LIST.y0)
        .clone()
}

/// The conversation's days are the dividers, and its runs are the bands: a test
/// of the kept layout is only worth something if the layout has them.
#[test]
fn the_layout_under_test_breaks_days_and_runs() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = conversation();
    let laid = reference(&mut text, &theme, &lines, LIST);
    let dividers = laid
        .rows
        .iter()
        .filter(|row| matches!(row, Row::Divider { .. }))
        .count();
    let bands = laid
        .rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                Row::Line {
                    author: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(dividers, 3, "three days, three dividers");
    // Five runs open with a named person (your own lines open runs too, but carry
    // no name; a line with no author is its own run and carries none either).
    assert_eq!(bands, 5, "the runs that open with a named person");
    assert_eq!(laid.rows.len(), lines.len() + dividers);
}

#[test]
fn a_kept_layout_is_the_layout_it_replaces() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = conversation();
    let laid = reference(&mut text, &theme, &lines, LIST);
    let face = ListFace::new(&lines, clock());
    let mut cache = ListCache::default();
    // The first frame measures every line.
    let first = face
        .layout(
            &mut cache,
            &mut text,
            &theme,
            LIST.x0,
            LIST.width(),
            LIST.y0,
        )
        .clone();
    same_layout(&first, &laid);
    assert_eq!(cache.work().measured, lines.len());
    assert_eq!(cache.work().reused, 0);
    // The second frame is the first one, kept.
    let second = face
        .layout(
            &mut cache,
            &mut text,
            &theme,
            LIST.x0,
            LIST.width(),
            LIST.y0,
        )
        .clone();
    same_layout(&second, &laid);
    assert_eq!(
        cache.work().measured,
        0,
        "a frame that changed nothing measures nothing"
    );
    assert_eq!(cache.work().reused, lines.len());
    // And a height asked for between frames is the height the rows were placed
    // at, without moving them.
    let height = face.height_in(&mut cache, &mut text, &theme, LIST.width());
    assert_eq!(height.to_bits(), laid.height.to_bits());
    let third = face
        .layout(
            &mut cache,
            &mut text,
            &theme,
            LIST.x0,
            LIST.width(),
            LIST.y0,
        )
        .clone();
    same_layout(&third, &laid);
    assert_eq!(
        cache.work().measured,
        0,
        "asking for the height does not measure or move the rows"
    );
}

#[test]
fn editing_a_line_measures_only_that_line() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let mut lines = conversation();
    let mut cache = ListCache::default();
    laid_out(&lines, &mut cache, &mut text, &theme);
    // The line is edited in place: the same slice, the same address, a different
    // body. Every line but it keeps the measurement it had.
    lines[5].body = "morning — the frame is up, take a look when you can".into();
    let edited = laid_out(&lines, &mut cache, &mut text, &theme);
    assert_eq!(
        cache.work().measured,
        1,
        "one line changed, one line measured"
    );
    assert_eq!(cache.work().reused, lines.len() - 1);
    same_layout(&edited, &reference(&mut text, &theme, &lines, LIST));
}

#[test]
fn an_arriving_line_measures_only_itself() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let mut lines = conversation();
    let mut cache = ListCache::default();
    laid_out(&lines, &mut cache, &mut text, &theme);
    lines.push(ConversationLine::outgoing("j", 0, "one more, just arrived"));
    let arrived = laid_out(&lines, &mut cache, &mut text, &theme);
    assert_eq!(
        cache.work().measured,
        1,
        "the new line is the only new measure"
    );
    assert_eq!(cache.work().reused, lines.len() - 1);
    same_layout(&arrived, &reference(&mut text, &theme, &lines, LIST));
}

#[test]
fn a_narrower_column_measures_every_line() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = conversation();
    let face = ListFace::new(&lines, clock());
    let mut cache = ListCache::default();
    face.layout(
        &mut cache,
        &mut text,
        &theme,
        LIST.x0,
        LIST.width(),
        LIST.y0,
    );
    let narrow = Rect::new(LIST.x0, LIST.y0, LIST.x0 + 420.0, LIST.y1);
    let laid = face
        .layout(
            &mut cache,
            &mut text,
            &theme,
            narrow.x0,
            narrow.width(),
            narrow.y0,
        )
        .clone();
    assert_eq!(
        cache.work().measured,
        lines.len(),
        "every bubble is a different shape in a narrower column"
    );
    assert_eq!(cache.work().reused, 0);
    same_layout(&laid, &reference(&mut text, &theme, &lines, narrow));
    // A height asked for at the narrow width is that column's height.
    assert_eq!(
        face.height_in(&mut cache, &mut text, &theme, narrow.width())
            .to_bits(),
        laid.height.to_bits()
    );
}

#[test]
fn a_larger_text_scale_measures_every_line() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = conversation();
    let face = ListFace::new(&lines, clock());
    let mut cache = ListCache::default();
    face.layout(
        &mut cache,
        &mut text,
        &theme,
        LIST.x0,
        LIST.width(),
        LIST.y0,
    );
    let larger = theme.with_text_scale(1.5);
    text.set_text_scale(1.5);
    let laid = face
        .layout(
            &mut cache,
            &mut text,
            &larger,
            LIST.x0,
            LIST.width(),
            LIST.y0,
        )
        .clone();
    assert_eq!(
        cache.work().measured,
        lines.len(),
        "a text scale changes every role's type"
    );
    same_layout(&laid, &reference(&mut text, &larger, &lines, LIST));
}

#[test]
fn a_moved_list_places_again_without_measuring() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let lines = conversation();
    let face = ListFace::new(&lines, clock());
    let mut cache = ListCache::default();
    face.layout(
        &mut cache,
        &mut text,
        &theme,
        LIST.x0,
        LIST.width(),
        LIST.y0,
    );
    // A scroller moves its content up the screen: the rows move, no line changes.
    let moved = Rect::new(LIST.x0, LIST.y0 - 640.0, LIST.x1, LIST.y1 - 640.0);
    let laid = face
        .layout(
            &mut cache,
            &mut text,
            &theme,
            moved.x0,
            moved.width(),
            moved.y0,
        )
        .clone();
    assert_eq!(cache.work().measured, 0, "moving a list measures nothing");
    assert_eq!(cache.work().reused, lines.len());
    same_layout(&laid, &reference(&mut text, &theme, &lines, moved));
}

#[test]
fn lines_that_leave_take_their_measurements_with_them() {
    let mut text = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let mut lines = conversation();
    let mut cache = ListCache::default();
    laid_out(&lines, &mut cache, &mut text, &theme);
    assert_eq!(cache.measures.len(), lines.len());
    lines.truncate(4);
    laid_out(&lines, &mut cache, &mut text, &theme);
    assert_eq!(
        cache.measures.len(),
        lines.len(),
        "a measurement nobody asks for is dropped"
    );
    assert_eq!(cache.work().measured, 0);
}
