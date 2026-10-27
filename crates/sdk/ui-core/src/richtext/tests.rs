use super::*;

#[test]
fn indented_wrap_hangs_continuation_rows_under_the_line_indent() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = crate::text_render::TextStyle::new(14.0)
        .with_family(crate::text_render::FontChoice::Monospace)
        .with_wrap_mode(crate::text_render::TextWrapMode::Indented);
    let text = "    let total = first_value + second_value + third_value + fourth_value;\nend";
    let plain = RichTextLayout::shape_plain(
        &mut fs,
        text,
        &crate::text_render::TextStyle::new(14.0)
            .with_family(crate::text_render::FontChoice::Monospace),
        Some(220.0),
    );
    let indented = RichTextLayout::shape_plain(&mut fs, text, &style, Some(220.0));
    let indent_x = indented.caret_geometry(4).0;
    assert!(indent_x > 20.0, "the line's own indent: {indent_x}");
    let rows: Vec<(usize, f32)> = indented
        .buffer
        .layout_runs()
        .filter(|run| run.line_i == 0)
        .skip(1)
        .map(|run| {
            let start = run.glyphs.iter().map(|glyph| glyph.start).min().unwrap();
            let byte = indented.glyph_global_byte(0, start);
            (byte, run.line_top)
        })
        .collect();
    assert!(!rows.is_empty(), "the first line wraps");
    for (byte, top) in rows {
        let (x, caret_top, _) =
            indented.caret_geometry_at(TextPosition::with_affinity(byte, TextAffinity::After));
        assert!(
            (x - indent_x).abs() < 0.5,
            "a wrapped row starts under the indent: {x} vs {indent_x}"
        );
        assert!((caret_top - top).abs() < 0.5);
        assert_eq!(
            indented.hit(1.0, top + 1.0),
            byte,
            "a click in the hanging indent lands at the row's start"
        );
    }
    // The text itself is unchanged: the same bytes, end to end.
    assert_eq!(indented.hit(1.0e4, 1.0e4), text.len());
    assert_eq!(plain.caret_geometry(4).0, indent_x);
}

#[test]
fn huge_paragraph_chunks_keep_source_offsets_and_reuse_shaping() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = crate::text_render::TextStyle::new(16.0);
    let text = (0..7000)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.len() > crate::text_break::HUGE_PARAGRAPH_BYTES);
    let layout = RichTextLayout::shape_styled(
        &mut fs,
        &[RichSpan::new(&text)],
        &style,
        Some(400.0),
        TextAlign::Left,
        vec![],
        vec![],
    );
    assert!(layout.buffer.lines.len() > 3, "shaped in chunks");
    let mut last_y = f32::MIN;
    for byte in (0..=text.len()).step_by(211) {
        let (x, y, h) = layout.caret_geometry(byte);
        assert!(y >= last_y && h > 0.0, "caret order at {byte}");
        last_y = y;
        let hit = layout.hit(x + 0.01, y + h / 2.0);
        assert!(hit.abs_diff(byte) <= 12, "hit {hit} for {byte}");
    }
    assert_eq!(
        layout.caret_geometry(text.len()).1,
        last_y.max(layout.caret_geometry(text.len()).1)
    );
    // An edit near the end retains the shaped allocation of the first chunk.
    let retained = layout.buffer.lines[0].layout_opt().unwrap().as_ptr();
    let edited = format!("{text} tail");
    let reused = RichTextLayout::shape_styled_reusing(
        &mut fs,
        &[RichSpan::new(&edited)],
        &style,
        Some(400.0),
        TextAlign::Left,
        vec![],
        vec![],
        Some(layout),
    );
    assert_eq!(
        retained,
        reused.buffer.lines[0].layout_opt().unwrap().as_ptr()
    );
}

#[test]
fn offscreen_visual_lines_are_culled_only_by_vertical_extent() {
    use vello::kurbo::{Affine, Rect};
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = crate::text_render::TextStyle::new(16.0);
    let text = "word ".repeat(200);
    let layout = RichTextLayout::shape_styled(
        &mut fs,
        &[RichSpan::new(&text)],
        &style,
        Some(100.0),
        TextAlign::Left,
        vec![],
        vec![],
    );
    let runs: Vec<_> = layout.buffer.layout_runs().collect();
    assert!(runs.len() > 10);
    let clip = Rect::new(0.0, 0.0, 100.0, 40.0);
    // Ink reaching two rows past each band, whichever way.
    let reach = InkReach {
        above: 2.0 * runs[0].line_height,
        below: 2.0 * runs[0].line_height,
    };
    let outside = |run: &cosmic_text::LayoutRun<'_>, transform: Affine, clip: Rect| {
        crate::text_render::rich_run_outside_clip(run, transform, clip, reach)
    };
    assert!(!outside(&runs[0], Affine::IDENTITY, clip));
    let last = runs.last().unwrap();
    assert!(outside(last, Affine::IDENTITY, clip));
    assert!(!outside(
        last,
        Affine::translate((0.0, -f64::from(last.line_top))),
        clip
    ));
    assert!(
        !outside(last, Affine::rotate(0.1), clip),
        "rotation is never culled"
    );
    assert!(!outside(
        last,
        Affine::IDENTITY,
        Rect::new(0.0, f64::NAN, 1.0, 1.0)
    ));
}

#[test]
fn batched_selection_rects_match_per_range_geometry() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = crate::text_render::TextStyle::new(16.0);
    let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau\nשלום mixed עברית text wraps here too\nemoji 👩‍💻 é end";
    let layout = RichTextLayout::shape_styled(
        &mut fs,
        &[RichSpan::new(text)],
        &style,
        Some(120.0),
        TextAlign::Left,
        vec![],
        vec![],
    );
    let bytes: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let mut ranges = vec![(5, 5), (text.len(), 0), (0, text.len())];
    for (i, &a) in bytes.iter().enumerate().step_by(3) {
        for &b in bytes.iter().skip(i).step_by(7) {
            ranges.push((a, b));
        }
    }
    let batched = layout.selection_rects_many(&ranges);
    assert_eq!(batched.len(), ranges.len());
    for (range, rects) in ranges.iter().zip(&batched) {
        assert_eq!(
            rects,
            &layout.selection_rects(range.0, range.1),
            "range {range:?}"
        );
    }
    // The band's source range covers every range that has a rectangle in it.
    let (lo, hi) = layout.source_range_within(0.0, 20.0).unwrap();
    for (range, rects) in ranges
        .iter()
        .zip(&layout.selection_rects_many_within(&ranges, 0.0, 20.0))
    {
        if !rects.is_empty() {
            assert!(range.1 > lo && range.0 < hi, "{range:?} outside {lo}..{hi}");
        }
    }
    assert_eq!(layout.source_range_within(1.0e6, 2.0e6), None);
    // A band keeps exactly the unrestricted rectangles of the lines it overlaps.
    let bottom = 20.0;
    let banded = layout.selection_rects_many_within(&ranges, 0.0, bottom);
    for (all, within) in batched.iter().zip(&banded) {
        let expected: Vec<_> = all
            .iter()
            .copied()
            .filter(|r| r.1 <= bottom && r.3 >= 0.0)
            .collect();
        assert_eq!(within, &expected);
    }
    assert!(banded.iter().any(|rects| !rects.is_empty()));
    assert!(
        batched.iter().map(Vec::len).sum::<usize>() > banded.iter().map(Vec::len).sum::<usize>()
    );
}

#[test]
fn reused_paragraphs_keep_allocations_and_match_fresh_geometry_through_edits() {
    // Case `reused_paragraphs_keep_allocations_and_match_fresh_geometry`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let style = crate::text_render::TextStyle::new(16.0);
        let initial = "first paragraph\nunchanged שלום world\nlast line\n";
        let old = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(initial)],
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let retained = old.buffer.lines[1].layout_opt().unwrap().as_ptr();
        let edited = "xfirst paragraph\nunchanged שלום world\nlast line\n";
        let reused = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &[RichSpan::new(edited)],
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(old),
        );
        assert_eq!(
            retained,
            reused.buffer.lines[1].layout_opt().unwrap().as_ptr(),
            "unchanged paragraph must retain its shaped layout allocation"
        );
        let fresh = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(edited)],
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert_eq!(reused.width(), fresh.width());
        assert_eq!(reused.height(), fresh.height());
        for byte in edited.char_indices().map(|(i, _)| i).chain([edited.len()]) {
            assert_eq!(reused.caret_geometry(byte), fresh.caret_geometry(byte));
            assert_eq!(
                reused.selection_rects(0, byte),
                fresh.selection_rects(0, byte)
            );
        }
    }

    // Case `reused_paragraphs_match_fresh_after_edit_and_style_transitions`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let base = crate::text_render::TextStyle::new(16.0);
        let texts = [
            "alpha beta gamma\nשלום mixed text\nemoji 👩‍💻 é\n",
            "new paragraph\nalpha beta gamma\nשלום mixed text\nemoji 👩‍💻 é\n",
            "new paragraph\nalpha beta gamma\nשלום changed text\nemoji 👩‍💻 é\n",
            "alpha beta gamma\nשלום changed text\nemoji 👩‍💻 é",
            "alpha beta gamma\nשלום changed text\nemoji 👩‍💻 é\n\n",
            "",
            "one\r\ntwo\nthree",
            "one\nthree",
        ];
        let mut previous = None;
        for (index, text) in texts.into_iter().cycle().take(32).enumerate() {
            let mut style = base.clone();
            if index >= 8 {
                style.size = 20.0;
            }
            if index >= 16 {
                style.letter_spacing = 0.6;
                style.italic = true;
            }
            if index >= 24 {
                style.wrap_mode = crate::text_render::TextWrapMode::Balance;
            }
            let width = if index % 3 == 0 { 90.0 } else { 180.0 };
            let align = if index % 5 == 0 {
                TextAlign::Right
            } else {
                TextAlign::Left
            };
            let split = text.find('\n').unwrap_or(text.len());
            let spans = [
                RichSpan::new(&text[..split])
                    .bold(index % 2 == 0)
                    .background(Some([20, 30, 40])),
                RichSpan::new(&text[split..]),
            ];
            let reused = RichTextLayout::shape_styled_reusing(
                &mut fs,
                &spans,
                &style,
                Some(width),
                align,
                vec![(0, split)],
                vec![],
                previous,
            );
            let fresh = RichTextLayout::shape_styled(
                &mut fs,
                &spans,
                &style,
                Some(width),
                align,
                vec![(0, split)],
                vec![],
            );
            assert_eq!(reused.width(), fresh.width(), "case {index}");
            assert_eq!(reused.height(), fresh.height(), "case {index}");
            assert_eq!(reused.backgrounds, fresh.backgrounds);
            assert_eq!(reused.logical_line_origins(), fresh.logical_line_origins());
            // Compare full glyph layouts, not only overall dimensions.
            for (a, b) in reused.buffer.lines.iter().zip(&fresh.buffer.lines) {
                assert_eq!(
                    format!("{:?}", a.layout_opt()),
                    format!("{:?}", b.layout_opt())
                );
            }
            for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                for affinity in [TextAffinity::Before, TextAffinity::After] {
                    let position = TextPosition::with_affinity(byte, affinity);
                    let actual = reused.caret_geometry_at(position);
                    assert_eq!(
                        actual,
                        fresh.caret_geometry_at(position),
                        "case {index} byte {byte}"
                    );
                    assert_eq!(
                        reused.hit_position(actual.0, actual.1 + actual.2 / 2.0),
                        fresh.hit_position(actual.0, actual.1 + actual.2 / 2.0)
                    );
                    assert_eq!(
                        reused.visual_motion_position(position, true),
                        fresh.visual_motion_position(position, true)
                    );
                }
                assert_eq!(
                    reused.selection_rects(0, byte),
                    fresh.selection_rects(0, byte)
                );
            }
            previous = Some(reused);
        }
    }
}

thread_local! {
    pub(super) static CARET_STOP_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "sweeping integer pixel positions across a fixed 400.0-wide test layout: width().ceil() is always small and non-negative, so the round trip through usize is exact"
)]
#[test]
fn caret_hit_and_selection_geometry_follow_visual_direction() {
    // Case `caret_geometry_does_not_segment_unrelated_paragraphs`.
    {
        let text = "ordinary paragraph\n".repeat(1000);
        let shaped = layout(&text, 300.0);
        CARET_STOP_RUNS.with(|count| count.set(0));
        let geometry = shaped.caret_geometry(0);
        assert_eq!(geometry.0, 0.0);
        let visited = CARET_STOP_RUNS.with(std::cell::Cell::get);
        assert!(
            visited <= 2,
            "caret search segmented {visited} unrelated paragraphs"
        );
    }

    // Case `caret_geometry_matches_full_scan_for_all_byte_affinity_boundaries`.
    {
        for text in [
            "",
            "alpha beta gamma\nnext\n",
            "a\r\nb\rc",
            "שלום mixed العربية\n👩‍💻 é\n",
            "ภาษาไทยทดสอบ\nខ្មែរ",
            "soft\u{ad}hyphen word\n\n",
        ] {
            for width in [30.0, 180.0] {
                let shaped = layout(text, width);
                for byte in 0..=text.len() + 1 {
                    for affinity in [TextAffinity::Before, TextAffinity::After] {
                        let position = TextPosition::with_affinity(byte.min(text.len()), affinity);
                        let mut fallback = None;
                        let mut exact = None;
                        'scan: for run in shaped.buffer.layout_runs() {
                            for (x, stop) in shaped.caret_stops(&run) {
                                let geometry = (x, run.line_top, run.line_height);
                                if stop == position {
                                    exact = Some(geometry);
                                    break 'scan;
                                }
                                if stop.byte == position.byte && fallback.is_none() {
                                    fallback = Some(geometry);
                                }
                            }
                        }
                        let expected = exact.or(fallback).unwrap_or((0.0, 0.0, shaped.line_height));
                        assert_eq!(
                            shaped.caret_geometry_at(TextPosition::with_affinity(byte, affinity)),
                            expected,
                            "text {text:?}, width {width}, byte {byte}, affinity {affinity:?}"
                        );
                    }
                }
            }
        }
    }

    // Case `shapes_caret_hit_and_selection`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new("hello world")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert!(layout.height() > 0.0);
        let (x0, _, h) = layout.caret_geometry(0);
        assert!(x0 < 2.0 && h > 0.0);
        let (xend, _, _) = layout.caret_geometry(11);
        assert!(xend > x0);
        assert!(layout.hit(xend - 1.0, 1.0) > 0);
        let rects = layout.selection_rects(0, 5);
        assert_eq!(rects.len(), 1);
        assert!(rects[0].2 > rects[0].0);
    }

    // Case `multiline_hit_and_caret_use_global_byte_offsets`.
    {
        let text = "first\nשני";
        let second_line = "first\n".len();
        let layout = layout(text, 400.0);
        let (_, first_top, first_h) = layout.caret_geometry(0);
        let (second_x, second_top, second_h) = layout.caret_geometry(second_line);

        assert!(second_top >= first_top + first_h * 0.9);
        assert_eq!(
            layout.hit(second_x + 1.0, second_top + second_h * 0.5),
            second_line,
            "a hit on line two must include the preceding line and newline bytes"
        );
        let rects = layout.selection_rects(0, text.len());
        assert!(rects.len() >= 2, "a cross-line selection paints each line");
    }

    // Case `rtl_caret_geometry_follows_visual_direction`.
    {
        let text = "שלום";
        let layout = layout(text, 400.0);
        let (start_x, top, height) = layout.caret_geometry(0);
        let (end_x, _, _) = layout.caret_geometry(text.len());

        assert!(
            start_x > end_x,
            "the logical start of an RTL paragraph is its visual right edge: start={start_x}, end={end_x}"
        );
        assert_eq!(layout.hit(start_x + 1.0, top + height * 0.5), 0);
        assert_eq!(
            layout.hit((end_x - 1.0).max(0.0), top + height * 0.5),
            text.len()
        );
    }

    // Case `mixed_bidi_selection_can_produce_disjoint_visual_bands`.
    {
        let text = "abc אבג xyz";
        // Logically select the Latin prefix plus the first Hebrew character.
        // Visually that Hebrew character sits at the far side of its RTL run,
        // so an unselected gap separates it from the Latin prefix.
        let selected_end = "abc א".len();
        let layout = layout(text, 400.0);
        let rects = layout.selection_rects(0, selected_end);

        assert_eq!(
            rects.len(),
            2,
            "mixed-direction selection must preserve disjoint visual ranges: {rects:?}"
        );
        assert!(rects.iter().all(|(x0, y0, x1, y1)| x1 > x0 && y1 > y0));
    }

    // Case `mixed_bidi_boundary_preserves_both_visual_affinities`.
    {
        let layout = layout("abc אבג xyz", 400.0);
        let (_, top, height) = layout.caret_geometry(0);
        let y = top + height * 0.5;
        let mut positions = Vec::new();
        for x in 0..=layout.width().ceil() as usize {
            let position = layout.hit_position(x as f32, y);
            if positions.last() != Some(&position) {
                positions.push(position);
            }
        }
        let pair = positions.iter().find_map(|before| {
            let before_x = layout.caret_geometry_at(*before).0;
            positions
                .iter()
                .find(|after| {
                    before.byte == after.byte
                        && before.affinity != after.affinity
                        && (before_x - layout.caret_geometry_at(**after).0).abs() > 1.0
                })
                .map(|after| (*before, *after))
        });
        let (a, b) = pair.expect("mixed BiDi exposes two sides of one logical byte boundary");
        let ax = layout.caret_geometry_at(a).0;
        let bx = layout.caret_geometry_at(b).0;
        assert!(
            (ax - bx).abs() > 1.0,
            "the two affinities occupy distinct visual positions: {a:?}={ax}, {b:?}={bx}"
        );

        let (left, right) = if ax < bx { (a, b) } else { (b, a) };
        let mut walked = left;
        for _ in 0..16 {
            if walked == right {
                break;
            }
            let next = layout.visual_motion_position(walked, true);
            assert_ne!(
                next, walked,
                "visual motion must not stall inside a BiDi run"
            );
            walked = next;
        }
        assert_eq!(
            walked, right,
            "walking across the intervening visual run reaches the other side of the same byte"
        );
    }
}

fn assert_near(actual: f32, expected: f32, message: &str) {
    assert!(
        (actual - expected).abs() < 0.01,
        "{message}: expected {expected}, got {actual}"
    );
}

fn layout(text: &str, width: f32) -> RichTextLayout {
    let mut fs = crate::font_catalog::fixture_font_system();
    RichTextLayout::shape_styled(
        &mut fs,
        &[RichSpan::new(text)],
        &RichTextLayout::block_style(false, 18.0, FontFamily::Sans, 1.0),
        Some(width),
        TextAlign::Left,
        vec![],
        vec![],
    )
}

/// Visual motion as a scan of every row of the document: the reference the
/// paragraph-scoped search must agree with. At a row's edge it enters the
/// row before or after in document order.
fn visual_motion_full_scan(
    layout: &RichTextLayout,
    position: TextPosition,
    right: bool,
) -> TextPosition {
    let position =
        TextPosition::with_affinity(position.byte.min(layout.text_len), position.affinity);
    let rows: Vec<_> = layout
        .buffer
        .layout_runs()
        .map(|run| (run.rtl, layout.caret_stops(&run)))
        .collect();
    let find = |matches: &dyn Fn(&TextPosition) -> bool| {
        rows.iter().enumerate().find_map(|(row, (_, stops))| {
            let index = stops.iter().position(|(_, stop)| matches(stop))?;
            Some((row, index))
        })
    };
    let Some((row, index)) =
        find(&|stop| *stop == position).or_else(|| find(&|stop| stop.byte == position.byte))
    else {
        return position;
    };
    let (rtl, stops) = &rows[row];
    let target = RichTextLayout::visual_step(stops, index, right);
    if (stops[target].0 - stops[index].0).abs() > 0.5 {
        return stops[target].1;
    }
    let next = if right == *rtl {
        row.checked_sub(1)
    } else {
        row.checked_add(1)
    };
    next.and_then(|next| rows.get(next))
        .and_then(|(_, next)| if right { next.first() } else { next.last() })
        .map(|(_, stop)| *stop)
        .filter(|stop| *stop != position)
        .unwrap_or(stops[target].1)
}

#[test]
fn arrow_keys_walk_across_soft_wraps_and_line_breaks_both_ways() {
    // A wrapped row's last stop and the next row's first are two places on
    // screen; an arrow key at either edge moves on to the other row instead
    // of stopping there. A right-to-left paragraph reads its rows from the
    // right edge, so Left walks it forward.
    for (text, rtl) in [
        ("alpha beta gamma delta\nepsilon zeta eta theta", false),
        ("שלום עולם ספר בית מים\nאור ים", true),
    ] {
        let shaped = layout(text, 80.0);
        assert!(
            shaped.buffer.layout_runs().count() > 3,
            "the fixture wraps: {text:?}"
        );
        for forward in [true, false] {
            let right = forward != rtl;
            let (start, end) = if forward {
                (0, text.len())
            } else {
                (text.len(), 0)
            };
            let mut at = TextPosition::new(start);
            let mut bytes = vec![start];
            for _ in 0..4 * text.len() {
                let next = shaped.visual_motion_position(at, right);
                assert_eq!(
                    next,
                    visual_motion_full_scan(&shaped, at, right),
                    "{text:?} from {at:?}, right {right}"
                );
                if next == at {
                    break;
                }
                at = next;
                bytes.push(at.byte);
            }
            assert_eq!(
                at.byte, end,
                "{text:?}, right {right}: the walk stopped at {at:?} after {bytes:?}"
            );
        }
    }
}

#[test]
fn rows_of_a_line_are_the_rows_a_full_scan_places_there() {
    let text = "a first paragraph that wraps onto more rows\n\nshort\nשלום and a wrapped mixed paragraph\nlast";
    let mut shaped = layout(text, 120.0);
    let row = |run: &cosmic_text::LayoutRun<'_>| {
        (
            run.line_i,
            run.line_top,
            run.line_y,
            run.line_height,
            run.glyphs.len(),
        )
    };
    let lines = shaped.buffer.lines.len();
    let all_rows = shaped.buffer.layout_runs().count();
    assert!(all_rows > lines, "the fixture wraps");
    for height in [None, Some(60.0)] {
        // A height ends the runs early; the seek leaves them ended there too.
        shaped.buffer.set_size(Some(120.0), height);
        let rows = shaped.buffer.layout_runs().count();
        assert_eq!(rows < all_rows, height.is_some(), "height {height:?}");
        for start in 0..=lines {
            let seeked: Vec<_> = shaped
                .buffer
                .layout_runs()
                .skip_to_line(start)
                .map(|run| row(&run))
                .collect();
            let scanned: Vec<_> = shaped
                .buffer
                .layout_runs()
                .skip_while(|run| run.line_i < start)
                .map(|run| row(&run))
                .collect();
            assert_eq!(seeked, scanned, "height {height:?}, line {start}");
        }
    }
}

#[test]
fn visual_motion_reads_only_its_paragraph_and_matches_a_full_scan() {
    for text in [
        "",
        "alpha beta gamma\nnext\n",
        "a\r\nb\rc",
        "שלום mixed العربية\n👩‍💻 é\n",
        "ภาษาไทยทดสอบ\nខ្មែរ",
        "soft\u{ad}hyphen word\n\n",
    ] {
        for width in [30.0, 180.0] {
            let shaped = layout(text, width);
            for byte in 0..=text.len() + 1 {
                for affinity in [TextAffinity::Before, TextAffinity::After] {
                    let position = TextPosition::with_affinity(byte, affinity);
                    for right in [false, true] {
                        assert_eq!(
                            shaped.visual_motion_position(position, right),
                            visual_motion_full_scan(&shaped, position, right),
                            "text {text:?}, width {width}, byte {byte}, {affinity:?}, right {right}"
                        );
                    }
                }
            }
        }
    }

    // An arrow key in a long document builds the caret stops of its own
    // paragraph's rows, never of the rows above it.
    let text = "a paragraph that wraps onto a second row\n".repeat(1000);
    let shaped = layout(&text, 200.0);
    let last = text.len() - 2;
    let rows = shaped.logical_line_bands(last).len();
    assert!(rows >= 2, "the fixture paragraph wraps");
    CARET_STOP_RUNS.with(|count| count.set(0));
    let moved = shaped.visual_motion_position(TextPosition::new(last), false);
    let visited = CARET_STOP_RUNS.with(std::cell::Cell::get);
    assert!(
        visited <= rows,
        "one arrow key built the caret stops of {visited} rows; its paragraph has {rows}"
    );
    assert_eq!(
        moved,
        visual_motion_full_scan(&shaped, TextPosition::new(last), false)
    );
}

#[test]
fn bounded_selection_geometry_refuses_overflow_without_partial_rectangles() {
    let text = "first line\nsecond line\nthird line";
    let shaped = layout(text, 400.0);
    let expected = shaped.selection_rects(0, text.len());
    assert_eq!(expected.len(), 3, "fixture has three real shaped lines");
    assert_eq!(
        shaped.selection_rects_bounded(0..text.len(), f32::NEG_INFINITY..f32::INFINITY, 3),
        Some(expected.clone())
    );
    assert!(
        shaped
            .selection_rects_bounded(0..text.len(), f32::NEG_INFINITY..f32::INFINITY, 2)
            .is_none()
    );
    assert!(
        shaped
            .selection_rects_bounded(0..text.len(), f32::NEG_INFINITY..f32::INFINITY, 0)
            .is_none()
    );
    let middle = expected[1];
    assert_eq!(
        shaped.selection_rects_bounded(0..text.len(), (middle.1 + 0.1)..(middle.3 - 0.1), 1),
        Some(vec![middle])
    );
    assert_eq!(
        shaped.selection_rects_bounded(0..text.len(), -100.0..-1.0, 0),
        Some(vec![])
    );
    assert!(
        shaped
            .selection_rects_bounded(0..text.len(), f32::NAN..1.0, 3)
            .is_none()
    );
}

#[test]
fn bounded_caret_geometry_matches_real_stops_without_origin_fallback() {
    let shaped = layout("a😀éZ", 400.0);
    for byte in [0, 1, 5, 7, 8] {
        assert_eq!(
            shaped.caret_geometry_bounded(byte),
            Some(shaped.caret_geometry(byte))
        );
    }
    assert!(shaped.caret_geometry_bounded(9).is_none());
    assert!(
        shaped.caret_geometry_bounded(2).is_none(),
        "a byte inside the emoji is not an observed caret stop"
    );
    let empty = layout("", 400.0);
    assert_eq!(
        empty.caret_geometry_bounded(0),
        Some(empty.caret_geometry(0))
    );
}

#[test]
fn bounded_geometry_refuses_oversized_shaped_runs_before_caret_allocation() {
    let text = "a".repeat(65_537);
    let mut fs = crate::font_catalog::fixture_font_system();
    let shaped = RichTextLayout::shape_plain(
        &mut fs,
        &text,
        &crate::text_render::TextStyle::new(18.0),
        None,
    );
    let runs: Vec<_> = shaped.buffer.layout_runs().collect();
    assert_eq!(runs.len(), 1, "fixture is one actual unwrapped run");
    assert_eq!(runs[0].glyphs.len(), text.len());
    CARET_STOP_RUNS.with(|count| count.set(0));
    assert!(shaped.caret_geometry_bounded(0).is_none());
    assert_eq!(CARET_STOP_RUNS.with(std::cell::Cell::get), 0);
    assert!(
        shaped
            .selection_rects_bounded(0..text.len(), 0.0..100.0, 4096)
            .is_none()
    );
    assert_eq!(
        shaped.selection_rects_bounded(0..text.len(), -100.0..-1.0, 0),
        Some(vec![]),
        "offscreen runs do not allocate highlight geometry"
    );
}

#[test]
fn paragraph_wrapping_alignment_and_final_rows_follow_controls() {
    // Case `center_alignment_offsets_text`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let left = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new("hi")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let center = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new("hi")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(400.0),
            TextAlign::Center,
            vec![],
            vec![],
        );
        let lx = left.caret_geometry(0).0;
        let cx = center.caret_geometry(0).0;
        assert!(
            cx > lx + 50.0,
            "centered text should start further right: left={lx} center={cx}"
        );
    }

    // Case `wrapping_produces_multiple_lines`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let long = "word ".repeat(60);
        let one = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new("word")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(1000.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let wrapped = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(long)],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(120.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert!(wrapped.height() > one.height() * 3.0);
    }

    // Case `paragraph_controls_shape_and_fragment_the_authoritative_layout`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let text = "Good typography makes a paragraph feel even before it is read. Spacing supports the argument without calling attention to itself.";
        let justified = RichTextLayout::shape_plain(
            &mut fs,
            text,
            &crate::TextStyle::new(16.0)
                .with_align(crate::ParagraphAlign::Justified)
                .with_fragmentation(2, 2),
            Some(260.0),
        );
        let widths: Vec<_> = justified
            .buffer
            .layout_runs()
            .map(|run| run.line_w)
            .collect();
        assert!(widths.len() >= 3);
        assert!(
            widths[..widths.len() - 1]
                .iter()
                .all(|width| (*width - 260.0).abs() < 1.0),
            "every eligible line should fill the justified measure: {widths:?}"
        );
        assert_eq!(justified.fragment_line_count(1), 0);
        assert_eq!(justified.fragment_line_count(2), 2);
        assert_eq!(justified.fragment_line_count(widths.len()), widths.len());
        let widow_safe = widths.len().saturating_sub(2);
        assert_eq!(
            justified.fragment_line_count(widths.len() - 1),
            if widow_safe >= 2 { widow_safe } else { 0 }
        );
        assert_eq!(
            justified.fragment_line_count_from(2, widths.len()),
            widths.len() - 2
        );
    }

    // Case `balanced_wrap_avoids_a_short_final_line_without_adding_lines`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let text = "A balanced heading wraps its thought into lines of comparable visual weight.";
        let ordinary =
            RichTextLayout::shape_plain(&mut fs, text, &crate::TextStyle::new(21.0), Some(520.0));
        let balanced = RichTextLayout::shape_plain(
            &mut fs,
            text,
            &crate::TextStyle::new(21.0).with_wrap_mode(crate::TextWrapMode::Balance),
            Some(520.0),
        );
        let ordinary_widths: Vec<_> = ordinary
            .buffer
            .layout_runs()
            .map(|run| run.line_w)
            .collect();
        let balanced_widths: Vec<_> = balanced
            .buffer
            .layout_runs()
            .map(|run| run.line_w)
            .collect();
        assert_eq!(balanced_widths.len(), ordinary_widths.len());
        let ordinary_range = ordinary_widths.iter().copied().fold(0.0, f32::max)
            - ordinary_widths.iter().copied().fold(f32::MAX, f32::min);
        let balanced_range = balanced_widths.iter().copied().fold(0.0, f32::max)
            - balanced_widths.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            balanced_range < ordinary_range,
            "balanced widths {balanced_widths:?} should improve {ordinary_widths:?}"
        );
    }

    // Case `attributed_terminal_lf_preserves_the_plain_layout_final_row`.
    {
        use crate::text_render::TextStyle;

        for scale in [1.0, 2.0] {
            let style = TextStyle::new(16.0 * scale).with_line_height(1.25);
            let fixtures = [
                ("x", vec![RichSpan::new("x")], 1usize),
                ("x\ny", vec![RichSpan::new("x\ny")], 2),
                ("x\n", vec![RichSpan::new("x\n")], 2),
                ("\n", vec![RichSpan::new("\n")], 2),
                ("x\n\n", vec![RichSpan::new("x\n\n")], 3),
                (
                    "x\n",
                    vec![
                        RichSpan::new("x").bold(true),
                        RichSpan::new("\n").italic(true),
                    ],
                    2,
                ),
            ];

            for (text, spans, expected_rows) in fixtures {
                let mut fs = crate::font_catalog::fixture_font_system();
                let plain = RichTextLayout::shape_plain(&mut fs, text, &style, Some(300.0));
                let styled = RichTextLayout::shape_styled(
                    &mut fs,
                    &spans,
                    &style,
                    Some(300.0),
                    TextAlign::Left,
                    vec![],
                    vec![],
                );

                assert_near(
                    styled.height(),
                    plain.height(),
                    &format!("{text:?} styled and plain heights at {scale}x"),
                );
                let plain_eof = plain.caret_geometry(text.len());
                let styled_eof = styled.caret_geometry(text.len());
                assert_near(
                    styled_eof.1,
                    plain_eof.1,
                    &format!("{text:?} EOF caret top at {scale}x"),
                );
                assert_near(
                    styled_eof.2,
                    plain_eof.2,
                    &format!("{text:?} EOF caret height at {scale}x"),
                );
                assert_eq!(
                    styled.visual_line_index(text.len()),
                    expected_rows - 1,
                    "{text:?} EOF visual row at {scale}x"
                );
                assert_eq!(
                    styled.logical_line_bands(text.len()).len(),
                    1,
                    "{text:?} EOF logical line has one shaped band at {scale}x"
                );
                assert_eq!(
                    styled.hit(0.0, styled.height() + style.line_height_px()),
                    text.len(),
                    "{text:?} hit below content resolves to EOF at {scale}x"
                );
            }
        }
    }

    // Case `terminal_empty_row_inherits_the_final_run_metrics_and_alignment`.
    {
        use crate::text_render::TextStyle;

        let style = TextStyle::new(16.0).with_line_height(1.25);
        let mut final_run = RichSpan::new("\n").bold(true).italic(true);
        final_run.size = Some(24.0);
        let mut fs = crate::font_catalog::fixture_font_system();
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new("x"), final_run],
            &style,
            Some(300.0),
            TextAlign::Center,
            vec![],
            vec![],
        );

        let final_line = layout.buffer.lines.last().expect("terminal empty line");
        let attrs = final_line.attrs_list().defaults();
        assert_eq!(final_line.text(), "");
        assert_eq!(final_line.ending(), cosmic_text::LineEnding::None);
        assert_eq!(final_line.align(), Some(Align::Center));
        assert_eq!(attrs.weight, Weight::BOLD);
        assert_eq!(attrs.style, Style::Italic);
        assert_eq!(
            attrs.metrics_opt,
            Some(Metrics::new(24.0, 30.0).into()),
            "the final empty row keeps the last effective run's metrics"
        );
    }
}

#[test]
fn script_aware_breaks_keep_document_offsets() {
    // Case `offset_map_round_trips_document_boundaries_around_virtual_breaks`.
    {
        let text = "ทุกสองสัปดาห์";
        let map = OffsetMap::for_text_with_hyphenation(text, None);
        assert_eq!(
            map.insertions
                .iter()
                .map(|insertion| insertion.source)
                .collect::<Vec<_>>(),
            vec![9, 18]
        );

        for source in text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
        {
            assert_eq!(map.shaped_to_source(map.source_to_shaped(source)), source);
        }
        for insertion in &map.insertions {
            let source = insertion.source;
            let len = insertion.ch.len_utf8();
            let before = map.source_to_shaped(source) - len;
            assert_eq!(map.shaped_to_source(before), source);
            assert_eq!(map.shaped_to_source(before + 1), source);
            assert_eq!(map.shaped_to_source(before + len), source);
        }
    }

    // Case `attributed_thai_wraps_at_dictionary_boundaries_in_document_byte_space`.
    {
        let text = "ทุกสองสัปดาห์";
        let first = &text[..9];
        let second = &text[9..18];
        let third = &text[18..];
        let width = [first, second, third]
            .into_iter()
            .map(|word| layout(word, 400.0).width())
            .fold(0.0f32, f32::max)
            + 0.5;

        let mut fs = crate::font_catalog::fixture_font_system();
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[
                RichSpan::new(first).background(Some([240, 220, 80])),
                RichSpan::new(format!("{second}{third}")).bold(true),
            ],
            &RichTextLayout::block_style(false, 18.0, FontFamily::Sans, 1.0),
            Some(width),
            TextAlign::Left,
            vec![(9, 18)],
            vec![],
        );

        assert_eq!(layout.visual_line_index(0), 0);
        assert_eq!(layout.visual_line_index(9), 1);
        assert_eq!(layout.visual_line_index(18), 2);
        assert_eq!(layout.visual_line_index(text.len()), 2);
        assert_eq!(layout.backgrounds[0].0..layout.backgrounds[0].1, 0..9);
        assert_eq!(layout.selection_rects(9, 18).len(), 1);

        for source in [0, 9, 18, text.len()] {
            let (x, top, height) = layout.caret_geometry(source);
            assert_eq!(
                layout.hit(x, top + height * 0.5),
                source,
                "hit testing must never expose a virtual-break byte"
            );
        }
    }

    // Case `plain_thai_layout_uses_the_same_dictionary_wrapping`.
    {
        use crate::text_render::TextStyle;

        let text = "ทุกสองสัปดาห์";
        let style = TextStyle::new(18.0);
        let mut fs = crate::font_catalog::fixture_font_system();
        let probe = RichTextLayout::shape_plain(&mut fs, &text[..9], &style, None);
        let width = probe.width() + 0.5;
        let layout = RichTextLayout::shape_plain(&mut fs, text, &style, Some(width));

        assert_eq!(layout.visual_line_index(0), 0);
        assert_eq!(layout.visual_line_index(9), 1);
        assert_eq!(layout.visual_line_index(18), 2);
        assert_eq!(
            layout.hit(layout.caret_geometry(18).0, layout.caret_geometry(18).1),
            18
        );
    }

    // Case `virtual_dictionary_hints_have_no_visible_advance`.
    {
        use crate::text_render::TextStyle;

        let text = "ทุกสองสัปดาห์";
        let style = TextStyle::new(18.0);
        let mut fs = crate::font_catalog::fixture_font_system();
        let source = RichTextLayout::shape_plain(&mut fs, text, &style, None);
        let hinted = RichTextLayout::shape_plain(&mut fs, text, &style, Some(10_000.0));

        assert!(
            (source.width() - hinted.width()).abs() < 0.01,
            "zero-width break hints must not alter visible text metrics"
        );
    }

    // Case `real_layout_wraps_cjk_but_preserves_nbsp_and_emoji_zwj_clusters`.
    {
        use crate::text_render::TextStyle;

        let style = TextStyle::new(18.0);
        let mut fs = crate::font_catalog::fixture_font_system();

        let cjk = "東京京都";
        let cjk_probe = RichTextLayout::shape_plain(&mut fs, "東京", &style, None);
        let cjk_layout =
            RichTextLayout::shape_plain(&mut fs, cjk, &style, Some(cjk_probe.width() + 0.5));
        assert_eq!(cjk_layout.visual_line_index(0), 0);
        assert!(cjk_layout.visual_line_index("東京".len()) >= 1);

        let nbsp = "10\u{a0}km away";
        let narrow = RichTextLayout::shape_plain(&mut fs, "10", &style, None).width() + 0.5;
        let nbsp_layout = RichTextLayout::shape_plain(&mut fs, nbsp, &style, Some(narrow));
        let protected_end = "10\u{a0}km".len();
        assert_eq!(nbsp_layout.visual_line_index(0), 0);
        assert_eq!(nbsp_layout.visual_line_index(protected_end), 0);
        assert!(nbsp_layout.visual_line_index(protected_end + 1) >= 1);

        let family = "👨‍👩‍👧‍👦";
        let emoji = format!("{family} {family}");
        let emoji_width = RichTextLayout::shape_plain(&mut fs, family, &style, None).width() + 0.5;
        let emoji_layout = RichTextLayout::shape_plain(&mut fs, &emoji, &style, Some(emoji_width));
        for (byte, _) in family.char_indices() {
            assert_eq!(emoji_layout.visual_line_index(byte), 0);
        }
        assert!(emoji_layout.visual_line_index(family.len() + 1) >= 1);
    }

    // Case `hyphenated_attributed_layout_keeps_document_offsets_and_decorations`.
    {
        use crate::text_render::TextStyle;

        let text = "extensive";
        let style = TextStyle::new(20.0).with_auto_hyphenation("en-US");
        let mut fs = crate::font_catalog::fixture_font_system();
        let probe = RichTextLayout::shape_plain(&mut fs, "exten-", &TextStyle::new(20.0), None);
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &[
                RichSpan::new("exten").background(Some([240, 220, 80])),
                RichSpan::new("sive").bold(true),
            ],
            &style,
            Some(probe.width() + 0.5),
            TextAlign::Left,
            vec![(2, 5)],
            vec![],
        );

        assert!(layout.visual_line_index(text.len()) >= 1);
        assert!(layout.width() <= probe.width() + 0.51);
        assert_eq!(layout.backgrounds[0].0..layout.backgrounds[0].1, 0..5);
        assert!(!layout.selection_rects(2, 5).is_empty());
        for source in 0..=text.len() {
            let (x, top, height) = layout.caret_geometry(source);
            assert_eq!(
                layout.hit(x, top + height * 0.5),
                source,
                "virtual soft-hyphen bytes must never escape into editor state"
            );
        }
    }
}

#[test]
fn run_styles_paint_backgrounds_superscripts_and_line_heights() {
    // Case `background_span_records_a_paintable_band`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        // "AAA" plain, "BBB" highlighted, "CCC" plain.
        let spans = [
            RichSpan::new("AAA"),
            RichSpan::new("BBB").background(Some([255, 230, 0])),
            RichSpan::new("CCC"),
        ];
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &spans,
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        // Exactly one background band, covering the "BBB" byte range [3, 6).
        assert_eq!(layout.backgrounds.len(), 1);
        let (s, e, rgb) = layout.backgrounds[0];
        assert_eq!((s, e), (3, 6));
        assert_eq!(rgb, [255, 230, 0]);
        // The draw path fills exactly `selection_rects(s, e)`; that band must be
        // a real, non-degenerate rectangle (x1 > x0, y1 > y0).
        let rects = layout.selection_rects(s, e);
        assert_eq!(rects.len(), 1, "highlight is a single contiguous band");
        let (x0, y0, x1, y1) = rects[0];
        assert!(x1 > x0, "highlight band must have positive width");
        assert!(y1 > y0, "highlight band must have positive height");
        // A span without a background contributes no band.
        let plain = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new("AAA")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert!(plain.backgrounds.is_empty());
    }

    // Case `super_run_shapes_smaller_and_offsets_up`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        let base_size = 20.0f32;
        // "x" normal, "2" superscript.
        let spans = [
            RichSpan::new("x"),
            RichSpan::new("2").baseline(BaselineShift::Super),
        ];
        let layout = RichTextLayout::shape_styled(
            &mut fs,
            &spans,
            &RichTextLayout::block_style(false, base_size, FontFamily::Sans, 1.0),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        // Recorded as a baseline range over the "2" byte [1, 2).
        assert_eq!(layout.baselines.len(), 1);
        assert_eq!((layout.baselines[0].0, layout.baselines[0].1), (1, 2));
        assert_eq!(layout.baselines[0].2, BaselineShift::Super);
        assert_eq!(layout.baseline_at(1), BaselineShift::Super);
        assert_eq!(layout.baseline_at(0), BaselineShift::None);

        // The superscript glyph is shaped at the reduced size; the base glyph
        // keeps the full size.
        let mut base_glyph_size = None;
        let mut super_glyph_size = None;
        for run in layout.buffer.layout_runs() {
            for g in run.glyphs {
                if g.start == 0 {
                    base_glyph_size = Some(g.font_size);
                } else if g.start == 1 {
                    super_glyph_size = Some(g.font_size);
                }
            }
        }
        let bg = base_glyph_size.expect("base glyph present");
        let sg = super_glyph_size.expect("super glyph present");
        assert!(
            (bg - base_size).abs() < 0.01,
            "base run keeps full size: {bg}"
        );
        assert!(sg < bg, "super run shapes smaller: super={sg} base={bg}");
        assert!(
            (sg - base_size * BaselineShift::SCALE).abs() < 0.5,
            "super ~0.7x: {sg}"
        );

        // The draw-time vertical offset moves super UP (negative screen-y) and
        // sub DOWN (positive screen-y); None never moves.
        assert!(
            BaselineShift::Super.offset(sg) < 0.0,
            "super lifts above baseline"
        );
        assert!(
            BaselineShift::Sub.offset(sg) > 0.0,
            "sub drops below baseline"
        );
        assert_eq!(BaselineShift::None.offset(sg), 0.0);
    }

    // Case `line_height_mult_scales_line_advance`.
    {
        let mut fs = crate::font_catalog::fixture_font_system();
        // Force two lines so the gap between line tops reflects the line advance.
        let long = "word ".repeat(40);
        let single = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(long.clone())],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(120.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let doubled = RichTextLayout::shape_styled(
            &mut fs,
            &[RichSpan::new(long)],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 2.0),
            Some(120.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let advance = |layout: &RichTextLayout| -> f32 {
            let tops: Vec<f32> = layout.buffer.layout_runs().map(|r| r.line_top).collect();
            assert!(
                tops.len() >= 2,
                "need at least two lines to measure advance"
            );
            tops[1] - tops[0]
        };
        let a1 = advance(&single);
        let a2 = advance(&doubled);
        assert!(
            (a2 - 2.0 * a1).abs() < a1 * 0.1,
            "2.0 multiple should ~double the per-line advance: 1.0={a1} 2.0={a2}",
        );
        assert!(doubled.height() > single.height() * 1.8);
    }
}
#[test]
fn batched_carets_match_existing_geometry_and_visit_each_row_once() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = crate::text_render::TextStyle::new(16.0);
    for text in [
        "",
        "ordinary text and ffi ligatures",
        "שלום mixed עברית words and English",
        "emoji 👩‍💻 and é é
second line",
        "  indented wrapped text has spaces and several visual rows",
    ] {
        for width in [None, Some(90.0)] {
            let layout = RichTextLayout::shape_styled(
                &mut fs,
                &[RichSpan::new(text)],
                &style,
                width,
                TextAlign::Left,
                vec![],
                vec![],
            );
            let positions: Vec<_> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([text.len(), text.len() + 1])
                .flat_map(|byte| {
                    [
                        TextPosition::with_affinity(byte, TextAffinity::Before),
                        TextPosition::with_affinity(byte, TextAffinity::After),
                    ]
                })
                .collect();
            let expected: Vec<_> = positions
                .iter()
                .map(|position| layout.caret_geometry_at(*position))
                .collect();
            CARET_STOP_RUNS.with(|count| count.set(0));
            let actual = layout.caret_geometries(&positions);
            assert_eq!(actual, expected, "{text:?} width={width:?}");
            assert_eq!(
                CARET_STOP_RUNS.with(std::cell::Cell::get),
                layout.buffer.layout_runs().count(),
                "one stop construction per visual row, not per requested caret"
            );
        }
    }
}
#[test]
fn caret_cluster_fast_path_matches_unicode_segmenter() {
    let mut clusters: Vec<String> = (0u8..=127)
        .map(|byte| char::from(byte).to_string())
        .collect();
    clusters.extend(["", "ffi", "\r\n", "é", "é", "👩‍💻", "שלום"].map(str::to_owned));
    for cluster in clusters {
        let mut expected: Vec<_> = crate::text_break::grapheme_indices(&cluster)
            .map(|(at, _)| at)
            .collect();
        expected.push(cluster.len());
        assert_eq!(
            caret_cluster_boundaries(&cluster).as_ref(),
            expected.as_slice(),
            "{cluster:?}"
        );
    }
}

/// Skipping the visual rows a range cannot touch returns exactly the
/// rectangles a scan of every row does, for every range of wrapped,
/// multi-line, mixed-direction and clustered text.
#[test]
fn selection_rects_match_a_scan_of_every_row() {
    let scan = |shaped: &RichTextLayout, start: usize, end: usize| {
        let mut rects = Vec::new();
        if start >= end {
            return rects;
        }
        let (cursor_start, cursor_end) = (shaped.cursor_at(start), shaped.cursor_at(end));
        for run in shaped.buffer.layout_runs() {
            if run.line_i < cursor_start.line || run.line_i > cursor_end.line {
                continue;
            }
            for (x, width) in run.highlight(cursor_start, cursor_end) {
                rects.push((x, run.line_top, x + width, run.line_top + run.line_height));
            }
        }
        rects
    };
    for text in [
        "one long paragraph that wraps across several visual rows of the layout",
        "first line\nsecond line wraps once it is long enough\n\nfourth",
        "שלום mixed العربية words wrapping 👩‍💻 é ffi",
    ] {
        for width in [60.0, 140.0, 400.0] {
            let shaped = layout(text, width);
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(at, _)| at)
                .chain([text.len()])
                .collect();
            for &start in &boundaries {
                for &end in &boundaries {
                    assert_eq!(
                        shaped.selection_rects(start, end),
                        scan(&shaped, start, end),
                        "{text:?} at {width}: {start}..{end}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_rows_within_a_view_of_a_long_text_are_found_without_walking_those_above() {
    // Wrapped and unwrapped lines, so lines hold one row or several.
    let text = (0..600)
        .map(|line| {
            if line % 7 == 0 {
                format!("line {line}: a longer line that wraps onto a second row and maybe a third")
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let shaped = layout(&text, 220.0);
    let walked = |top: f32, bottom: f32| -> Vec<(usize, u32)> {
        shaped
            .buffer
            .layout_runs()
            .skip_while(|run| run.line_top + run.line_height * 3.0 < top)
            .take_while(|run| run.line_top - run.line_height * 2.0 <= bottom)
            .map(|run| (run.line_i, run.line_top.to_bits()))
            .collect()
    };
    let height = shaped.height();
    for (top, bottom) in [
        (0.0, 300.0),
        (-50.0, 10.0),
        (1234.5, 1800.0),
        (height * 0.5, height * 0.5 + 400.0),
        (height - 100.0, height + 100.0),
        (height + 500.0, height + 900.0),
    ] {
        let found: Vec<(usize, u32)> = shaped
            .rows_within(top, bottom)
            .map(|run| (run.line_i, run.line_top.to_bits()))
            .collect();
        assert_eq!(found, walked(top, bottom), "rows within {top}..{bottom}");
    }
}

/// Caret geometry as a scan of every row of the document: the first exact
/// stop, else the first stop of the byte.
fn caret_geometry_full_scan(layout: &RichTextLayout, position: TextPosition) -> (f32, f32, f32) {
    let mut fallback = None;
    for run in layout.buffer.layout_runs() {
        for (x, stop) in layout.caret_stops(&run) {
            let geometry = (x, run.line_top, run.line_height);
            if stop == position {
                return geometry;
            }
            if stop.byte == position.byte && fallback.is_none() {
                fallback = Some(geometry);
            }
        }
    }
    fallback.unwrap_or((0.0, 0.0, layout.line_height))
}

#[test]
fn a_caret_at_a_huge_paragraphs_chunk_boundary_stands_and_moves_as_at_a_wrap() {
    // A paragraph past the chunking threshold is shaped as several lines;
    // the end of one chunk and the start of the next are the same source
    // byte, two sides of a soft wrap. The caret there must still find the
    // earlier chunk's end, or Left from it stops dead at the later start.
    let text = (0..7000)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.len() > crate::text_break::HUGE_PARAGRAPH_BYTES);
    let shaped = layout(&text, 400.0);
    let chunks = shaped.buffer.lines.len();
    assert!(chunks > 2, "shaped in chunks");
    for line in 1..chunks {
        let boundary = shaped.global_byte(Cursor::new(line, 0));
        for byte in [boundary - 1, boundary, boundary + 1] {
            for affinity in [TextAffinity::Before, TextAffinity::After] {
                let position = TextPosition::with_affinity(byte, affinity);
                assert_eq!(
                    shaped.caret_geometry_at(position),
                    caret_geometry_full_scan(&shaped, position),
                    "caret at {position:?}, chunk {line}"
                );
                for right in [false, true] {
                    assert_eq!(
                        shaped.visual_motion_position(position, right),
                        visual_motion_full_scan(&shaped, position, right),
                        "{position:?}, right {right}, chunk {line}"
                    );
                }
            }
        }
        // Left from the end of a chunk leaves the boundary.
        let end = TextPosition::with_affinity(boundary, TextAffinity::Before);
        assert!(
            shaped.visual_motion_position(end, false).byte < boundary,
            "Left from the end of chunk {}",
            line - 1
        );
    }
}
