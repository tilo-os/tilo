use super::*;

/// `count` lines `height` tall, stacked from the block's top.
fn lines(count: usize, height: f64) -> Vec<(f64, f64)> {
    (0..count)
        .map(|i| (f64::from(u32::try_from(i).unwrap_or(0)) * height, height))
        .collect()
}

/// A text block of `count` lines `height` tall.
fn text(count: usize, height: f64) -> Block {
    Block::new(f64::from(u32::try_from(count).unwrap_or(0)) * height)
}

#[test]
fn fragmentation_keeps_orphans_and_widows() {
    let rule = Fragmentation::default();
    assert_eq!(rule.lines_on_page(5, 0, 5), 5);
    assert_eq!(rule.lines_on_page(5, 0, 4), 3, "two lines follow the break");
    assert_eq!(rule.lines_on_page(5, 0, 1), 0, "one line cannot stay alone");
    assert_eq!(rule.lines_on_page(3, 0, 2), 0, "a short block never splits");
    assert_eq!(rule.lines_on_page(8, 3, 3), 3);
    assert_eq!(Fragmentation::ANYWHERE.lines_on_page(3, 0, 1), 1);
}

#[test]
fn blocks_that_fit_stack_and_space_before_drops_after_a_break() {
    let mut flow = PageFlow::new(100.0);
    let a = flow.place(&Block::new(40.0).with_lead(5.0));
    assert_eq!(
        (a.page, a.top),
        (0, 5.0),
        "space before the first block stays"
    );
    flow.skip(10.0);
    let b = flow.place(&Block::new(41.0).with_lead(5.0));
    assert_eq!((b.page, b.top), (1, 0.0), "moved block starts at the top");
    let c = flow.place(&Block::new(10.0).with_lead(5.0));
    assert_eq!((c.page, c.top), (1, 46.0));
}

#[test]
fn a_manual_break_starts_a_page_unless_the_page_is_empty() {
    let mut flow = PageFlow::new(100.0);
    let first = flow.place(&Block::new(10.0).breaking_before(true));
    assert_eq!(first.page, 0);
    let second = flow.place(&Block::new(10.0).breaking_before(true));
    assert_eq!((second.page, second.top), (1, 0.0));
}

#[test]
fn an_indivisible_block_taller_than_a_page_overflows_its_own_page() {
    let mut flow = PageFlow::new(100.0);
    flow.place(&Block::new(30.0));
    let tall = flow.place(&Block::new(250.0));
    assert_eq!((tall.page, tall.top), (1, 0.0));
    let next = flow.place(&Block::new(10.0));
    assert_eq!(next.page, 2);
}

#[test]
fn text_splits_between_lines_filling_the_page() {
    let mut flow = PageFlow::new(65.0);
    flow.place(&Block::new(20.0));
    // Five lines from y = 20: one would stay, so the block moves whole.
    let moved = flow.place_lines(&text(5, 20.0).with_lead(20.0), || lines(5, 20.0));
    assert_eq!(
        moved.fragments,
        vec![
            Fragment {
                page: 1,
                top: 0.0,
                lines: 0..3
            },
            Fragment {
                page: 2,
                top: 0.0,
                lines: 3..5
            },
        ]
    );
    assert_eq!(moved.locate(&lines(5, 20.0), 4), (2, 20.0));
    assert!((flow.y() - 40.0).abs() < f64::EPSILON);

    // Five lines on a page of four: two follow the break, not one.
    let mut flow = PageFlow::new(85.0);
    let widow = flow.place_lines(&text(5, 20.0), || lines(5, 20.0));
    let ranges: Vec<_> = widow.fragments.iter().map(|f| f.lines.clone()).collect();
    assert_eq!(ranges, vec![0..3, 3..5]);
}

#[test]
fn lines_fill_an_empty_page_when_the_rules_cannot_hold() {
    let mut flow = PageFlow::new(25.0);
    let placed = flow.place_lines(&text(5, 20.0), || lines(5, 20.0));
    assert_eq!(placed.fragments.len(), 5, "one line per page");
}

#[test]
fn line_boxes_are_only_asked_for_when_the_block_does_not_fit() {
    let mut flow = PageFlow::new(100.0);
    let mut asked = false;
    let placed = flow.place_lines(&text(3, 10.0), || {
        asked = true;
        lines(3, 10.0)
    });
    assert!(placed.fragments.is_empty());
    assert!(!asked, "a block that fits whole needs no line boxes");
}

#[test]
fn a_heading_moves_with_the_text_that_follows_it() {
    let mut flow = PageFlow::new(55.0);
    flow.place(&Block::new(20.0));
    let heading = flow.place_lines(
        &text(2, 10.0).with_lead(5.0).keeping_with_next(true),
        || lines(2, 10.0),
    );
    assert_eq!((heading.page, heading.top), (0, 25.0));
    let body = flow.place_lines(&text(4, 10.0), || lines(4, 10.0));
    assert_eq!(
        body.carry,
        Some(Carry {
            block: 1,
            from_page: 0,
            from_top: 25.0
        })
    );
    // The heading now takes 0..20 of page 1; two body lines fit after it.
    assert_eq!(
        body.fragments.first().map(|f| (f.page, f.top)),
        Some((1, 20.0))
    );
}

#[test]
fn consecutive_headings_move_together_but_never_off_their_own_page() {
    let mut flow = PageFlow::new(50.0);
    flow.place(&Block::new(10.0));
    flow.place(&Block::new(10.0).keeping_with_next(true));
    flow.place(&Block::new(10.0).keeping_with_next(true));
    let body = flow.place(&Block::new(30.0));
    assert_eq!(body.carry.map(|c| (c.block, c.from_top)), Some((1, 10.0)));
    assert_eq!((body.page, body.top), (1, 20.0));

    // A heading that began its page stays there.
    let mut flow = PageFlow::new(50.0);
    flow.place(&Block::new(20.0).keeping_with_next(true));
    let body = flow.place(&Block::new(40.0));
    assert_eq!(body.carry, None);
    assert_eq!(body.page, 1);
}

#[test]
fn a_manual_break_leaves_the_heading_behind() {
    let mut flow = PageFlow::new(50.0);
    flow.place(&Block::new(10.0));
    flow.place(&Block::new(10.0).keeping_with_next(true));
    let body = flow.place(&Block::new(10.0).breaking_before(true));
    assert_eq!(body.carry, None);
    assert_eq!((body.page, body.top), (1, 0.0));
}

#[test]
fn resuming_mid_page_counts_what_is_already_there() {
    let mut flow = PageFlow::resume(100.0, 3, 90.0, true);
    let placed = flow.place(&Block::new(20.0));
    assert_eq!((placed.page, placed.top), (4, 0.0));
}
