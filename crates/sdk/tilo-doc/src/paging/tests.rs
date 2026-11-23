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

/// Whole blocks placed one after another.
#[test]
fn whole_blocks_stack_break_and_keep_with_what_follows() {
    // Blocks that fit stack, and space before drops after a break.
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
    assert_eq!(
        (c.page, c.top),
        (1, 46.0),
        "a block that fits stacks below, its space before kept"
    );

    // A manual break starts a page unless the page is empty.
    let mut flow = PageFlow::new(100.0);
    let first = flow.place(&Block::new(10.0).breaking_before(true));
    assert_eq!(first.page, 0, "a manual break on an empty page starts none");
    let second = flow.place(&Block::new(10.0).breaking_before(true));
    assert_eq!(
        (second.page, second.top),
        (1, 0.0),
        "a manual break starts a page"
    );

    // An indivisible block taller than a page overflows its own page.
    let mut flow = PageFlow::new(100.0);
    flow.place(&Block::new(30.0));
    let tall = flow.place(&Block::new(250.0));
    assert_eq!(
        (tall.page, tall.top),
        (1, 0.0),
        "a block taller than a page starts a page of its own"
    );
    let next = flow.place(&Block::new(10.0));
    assert_eq!(next.page, 2, "what follows the tall block starts the next");

    // Consecutive headings move together but never off their own page.
    let mut flow = PageFlow::new(50.0);
    flow.place(&Block::new(10.0));
    flow.place(&Block::new(10.0).keeping_with_next(true));
    flow.place(&Block::new(10.0).keeping_with_next(true));
    let body = flow.place(&Block::new(30.0));
    assert_eq!(
        body.carry.map(|c| (c.block, c.from_top)),
        Some((1, 10.0)),
        "consecutive headings move together"
    );
    assert_eq!(
        (body.page, body.top),
        (1, 20.0),
        "the text follows the headings it carried"
    );
    let mut flow = PageFlow::new(50.0);
    flow.place(&Block::new(20.0).keeping_with_next(true));
    let body = flow.place(&Block::new(40.0));
    assert_eq!(
        body.carry, None,
        "a heading that began its page stays there"
    );
    assert_eq!(
        body.page, 1,
        "the text after a heading that stays moves alone"
    );

    // A manual break leaves the heading behind.
    let mut flow = PageFlow::new(50.0);
    flow.place(&Block::new(10.0));
    flow.place(&Block::new(10.0).keeping_with_next(true));
    let body = flow.place(&Block::new(10.0).breaking_before(true));
    assert_eq!(body.carry, None, "a manual break carries no heading");
    assert_eq!(
        (body.page, body.top),
        (1, 0.0),
        "a manual break after a heading starts a page"
    );

    // Resuming mid-page counts what is already there.
    let mut flow = PageFlow::resume(100.0, 3, 90.0, true);
    let placed = flow.place(&Block::new(20.0));
    assert_eq!(
        (placed.page, placed.top),
        (4, 0.0),
        "resuming mid-page counts what is already there"
    );
}

/// Text placed line by line.
#[test]
fn text_splits_between_lines_and_a_heading_moves_with_what_follows() {
    // Text splits between lines, filling the page.
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
        ],
        "a block one line of which would stay moves whole, then splits"
    );
    assert_eq!(
        moved.locate(&lines(5, 20.0), 4),
        (2, 20.0),
        "a line is located in its fragment"
    );
    assert!(
        (flow.y() - 40.0).abs() < f64::EPSILON,
        "the flow continues below the last fragment"
    );

    // Five lines on a page of four: two follow the break, not one.
    let mut flow = PageFlow::new(85.0);
    let widow = flow.place_lines(&text(5, 20.0), || lines(5, 20.0));
    let ranges: Vec<_> = widow.fragments.iter().map(|f| f.lines.clone()).collect();
    assert_eq!(ranges, vec![0..3, 3..5], "two lines follow the break");

    // Lines fill an empty page when the rules cannot hold.
    let mut flow = PageFlow::new(25.0);
    let placed = flow.place_lines(&text(5, 20.0), || lines(5, 20.0));
    assert_eq!(placed.fragments.len(), 5, "one line per page");

    // Line boxes are only asked for when the block does not fit.
    let mut flow = PageFlow::new(100.0);
    let mut asked = false;
    let placed = flow.place_lines(&text(3, 10.0), || {
        asked = true;
        lines(3, 10.0)
    });
    assert!(
        placed.fragments.is_empty(),
        "a block that fits whole is not fragmented"
    );
    assert!(!asked, "a block that fits whole needs no line boxes");

    // A heading moves with the text that follows it.
    let mut flow = PageFlow::new(55.0);
    flow.place(&Block::new(20.0));
    let heading = flow.place_lines(
        &text(2, 10.0).with_lead(5.0).keeping_with_next(true),
        || lines(2, 10.0),
    );
    assert_eq!(
        (heading.page, heading.top),
        (0, 25.0),
        "the heading is placed where it fits"
    );
    let body = flow.place_lines(&text(4, 10.0), || lines(4, 10.0));
    assert_eq!(
        body.carry,
        Some(Carry {
            block: 1,
            from_page: 0,
            from_top: 25.0
        }),
        "the text carries its heading to the next page"
    );
    // The heading now takes 0..20 of page 1; two body lines fit after it.
    assert_eq!(
        body.fragments.first().map(|f| (f.page, f.top)),
        Some((1, 20.0)),
        "the text starts below the heading it carried"
    );
}
