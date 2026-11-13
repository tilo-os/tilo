//! The page slide: a page put in place of another arrives on the drill
//! motion, the page it replaced leaves, and a rebuild at rest moves nothing.

use super::*;
use crate::component::Described;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::time::testing::with_monotonic_time;
use crate::widget::{Column, Text};

fn rooted(column: Column) -> FrameHarness {
    let mut d = FrameHarness::for_root(State::new(), column, (640, 400), Theme::dark());
    d.relayout();
    d
}

/// Build the view again with a page `level` deep in place of what stood
/// there.
fn show(d: &mut FrameHarness, level: usize) {
    let page = Text::new(if level == 0 { "First" } else { "Second" }).with_id("page");
    d.root_mut::<Column>().set_children(vec![
        Box::new(PageSlide::new(level, Box::new(page))),
        Box::new(Text::new("Below").with_id("below")),
    ]);
    d.relayout();
}

fn leaving(d: &FrameHarness) -> bool {
    crate::widget::Widget::children(d.root::<Column>())
        .first()
        .and_then(|child| child.as_any())
        .and_then(|any| any.downcast_ref::<PageSlide>())
        .is_some_and(PageSlide::is_leaving)
}

#[test]
fn a_page_put_in_anothers_place_arrives_on_the_motion_and_the_old_one_leaves() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    show(&mut d, 0);
    assert!(
        !d.root_widget().needs_animation_frame(),
        "the first page rests: nothing moves on first paint"
    );
    let frozen = crate::motion::pin_frame();
    show(&mut d, 1);
    assert!(
        d.root_widget().needs_animation_frame(),
        "the deeper page arrives on the drill motion"
    );
    assert!(leaving(&d), "and the page it replaced is drawn leaving");
    drop(frozen);
    let later = start + crate::time::Duration::from_secs(2);
    with_monotonic_time(later, || {
        d.relayout();
        assert!(!d.root_widget().needs_animation_frame(), "it rests");
        assert!(!leaving(&d), "and the page it replaced is let go");
    });
}

#[test]
fn going_back_rests_the_first_page_with_nothing_left_beside_it() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    show(&mut d, 0);
    show(&mut d, 1);
    let later = start + crate::time::Duration::from_secs(2);
    with_monotonic_time(later, || {
        d.relayout();
        assert!(!d.root_widget().needs_animation_frame());
    });
    with_monotonic_time(later, || {
        show(&mut d, 0);
        assert!(
            d.root_widget().needs_animation_frame(),
            "Back moves the other way"
        );
        assert!(leaving(&d), "and the page it left is drawn going out");
    });
    let rested = later + crate::time::Duration::from_secs(2);
    with_monotonic_time(rested, || {
        d.relayout();
        assert!(!d.root_widget().needs_animation_frame());
        assert!(!leaving(&d));
    });
}

#[test]
fn a_page_rebuilt_where_it_stands_never_draws_a_second_page() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    show(&mut d, 0);
    show(&mut d, 1);
    let later = start + crate::time::Duration::from_secs(2);
    with_monotonic_time(later, || {
        d.poll_frame().expect("the frame draws");
        for _ in 0..2 {
            show(&mut d, 1);
            d.poll_frame().expect("the frame draws");
            assert!(!leaving(&d), "a slide at rest shows one page");
        }
    });
}

fn props(level: i64, page: &str) -> PageSlideProps {
    PageSlideProps {
        key: "deck".to_owned(),
        level,
        page: Some(Box::new(Text::new(page).with_id("page"))),
    }
}

fn show_props(d: &mut FrameHarness, props: PageSlideProps) {
    d.root_mut::<Column>().set_children(vec![
        Box::new(PageSlide::from_props("deck", props)),
        Box::new(Text::new("Below").with_id("below")),
    ]);
    d.relayout();
}

#[test]
fn a_described_slide_takes_its_page_and_level_and_a_deeper_page_leaves() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    show_props(&mut d, props(0, "First"));
    assert!(
        !d.root_widget().needs_animation_frame(),
        "the first page rests"
    );
    let frozen = crate::motion::pin_frame();
    show_props(&mut d, props(1, "Second"));
    assert!(
        d.root_widget().needs_animation_frame(),
        "the deeper page arrives on the drill motion"
    );
    assert!(leaving(&d), "and the page the view put in its place leaves");
    drop(frozen);
}

#[test]
fn a_slide_at_its_level_changes_where_it_stands_and_a_deeper_one_is_built_again() {
    let mut slide = PageSlide::from_props("deck", props(0, "First"));
    assert_eq!(slide.level, 0);
    assert_eq!(
        slide.kept_as, "deck",
        "it leaves its motion under the key its view gave it, so the slide built in its place \
         takes it up"
    );
    let slot = slide.slot_views(PAGE_SLOT).expect("the page is a slot");
    assert_eq!(slot.len(), 1);
    slot.replace(0, Box::new(Text::new("Second").with_id("next")));
    assert!(slide.needs_redraw(), "the page it shows changed");
    assert!(
        slide.set_props(props(0, "First")),
        "the same level takes its props where it stands"
    );
    assert!(
        !slide.set_props(props(1, "First")),
        "a deeper page builds the slide again, so the page it showed leaves"
    );
}
