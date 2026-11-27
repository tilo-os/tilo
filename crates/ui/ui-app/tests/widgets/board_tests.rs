//! The board widget: the lists and cards it shows, the cards it builds as
//! they come into view, and the drops it reports.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion"
)]
fn card() -> Element {
    Box::new(
        Container::new()
            .with_size(200.0, 60.0)
            .with_child(Box::new(Spacer::fixed(1.0))),
    )
}

fn board(drops: &Rc<RefCell<Vec<BoardDrop>>>) -> Board {
    let sink = Rc::clone(drops);
    Board::new()
        .list(
            "todo",
            Some(Box::new(Text::new("Todo")) as Element),
            vec![("a".into(), card()), ("b".into(), card())],
            None,
        )
        .list(
            "done",
            Some(Box::new(Text::new("Done")) as Element),
            vec![("c".into(), card())],
            None,
        )
        .on_drop(move |drop| sink.borrow_mut().push(drop))
}

fn harness(board: Board) -> Driver {
    Driver::for_root(tilo_ui::State::new(), board, (900, 600), Theme::dark())
}

tilo_test_env::scenario! {
    /// The board widget: lists, gutters and drops.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_board_lays_out_lists_and_reports_drops;

    phase a_list_shows_every_card_it_fits;
    // A list as tall as the pane it is in shows every card it fits under its heading,
    // and the ones past that scroll in the view under it rather than being lost.
    {
        let cards: Vec<(String, Element)> = (0..8)
            .map(|index| (format!("card-{index}"), card()))
            .collect();
        let board = Board::new().list("todo", Some(Box::new(Text::new("Todo"))), cards, None);
        let host = harness(board);
        let root = host.root::<Board>();
        let shown = root.card_rects_for_tests(0);
        assert_eq!(
            shown.len(),
            8,
            "eight 60 tall cards stand in a 600 tall pane under their heading, and are all shown"
        );
        let list = root.list_rects_for_tests()[0];
        for (index, rect) in shown.iter().enumerate() {
            assert!(
                rect.y0 >= list.y0 && rect.y1 <= list.y1,
                "card {index} stands in its list: {rect:?} in {list:?}"
            );
            assert!(
                rect.y0 > list.y0,
                "card {index} stands below the list's heading"
            );
        }
        assert!(
            shown.windows(2).all(|pair| pair[0].y1 <= pair[1].y0),
            "the cards are in order, one under the other"
        );
    }

    phase a_board_keeps_a_gutter_at_both_ends_of_its_lists;
    // The lists stand left-aligned at their comfortable measure: the first a gutter in from
    // the leading edge, each the sibling gap from the last, and the width a wider pane leaves
    // open beside them at the trailing edge.
    {
        let theme = Theme::dark();
        let list = f64::from(Board::lists_width_for_tests(&theme, 1280.0, 1));
        let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
        let gap = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Sibling));
        let pane = 1280u32;
        let mut lists = Board::new();
        for key in ["todo", "doing", "done"] {
            let header = Some(Box::new(Text::new(key)) as Element);
            lists = lists.list(key, header, vec![(format!("{key}-card"), card())], None);
        }
        let host =
            Driver::for_root(tilo_ui::State::new(), lists, (pane, 600), theme);
        let root = host.root::<Board>();
        let rects = root.list_rects_for_tests();
        assert_eq!(rects.len(), 3, "three lists");
        let board = Widget::get_rect(root);
        assert!(
            (rects[0].x0 - board.x0 - gutter).abs() < 1.0,
            "the first list stands a gutter in from the leading edge: {rects:?} in {board:?}"
        );
        for (index, rect) in rects.iter().enumerate() {
            assert!(
                (rect.width() - list).abs() < 1.0,
                "list {index} keeps its comfortable measure: {rect:?}"
            );
            if let Some(next) = rects.get(index + 1) {
                assert!(
                    (next.x0 - rect.x1 - gap).abs() < 1.0,
                    "list {index} is a sibling gap from the next: {rects:?}"
                );
            }
        }
        assert!(
            board.x1 - rects[2].x1 > gutter,
            "a wider pane leaves its extra area open beside the lists: {rects:?} in {board:?}"
        );
    }
}

/// A board whose lists build their cards as they come into view, counting every card built.
fn listed(cards: usize, built: &Rc<std::cell::Cell<usize>>) -> Board {
    let mut board = Board::new();
    for list in ["todo", "done"] {
        let keys: Vec<String> = (0..cards).map(|index| format!("{list}-{index}")).collect();
        let counted = Rc::clone(built);
        board = board.long_list(
            list.to_owned(),
            Some(Box::new(Text::new(list)) as Element),
            keys,
            Box::new(move |_| {
                counted.set(counted.get() + 1);
                card()
            }),
            None,
        );
    }
    board
}

#[test]
fn a_list_of_ten_thousand_cards_builds_the_few_it_shows_and_more_as_it_scrolls() {
    let built = Rc::new(std::cell::Cell::new(0));
    let mut host = harness(listed(10_000, &built));
    // Before its first layout a list holds the cards of a tall screen; then the nine or so
    // 60 tall cards its 600 tall pane shows, with a margin either side. However long it is.
    let tall_screen = (tilo_ui_base::virtual_viewport::UNPLACED_VIEWPORT
        / f64::from(tilo_ui::widget::card_estimate_for_tests()))
    .ceil() as usize;
    let opened = built.get();
    assert!(
        opened <= 2 * (tall_screen + 3 * 10 + 16),
        "two lists of 10,000 cards built {opened} of them as the board opened"
    );
    let shown = host.root::<Board>().card_rects_for_tests(0);
    assert!(
        (5..=12).contains(&shown.len()),
        "the list shows the cards its height fits: {}",
        shown.len()
    );
    let list = host.root::<Board>().list_rects_for_tests()[0];
    host.send(&tilo_ui_base::synthetic_input::cursor_moved(
        list.center().x,
        list.center().y,
    ));
    for _ in 0..20 {
        let before = built.get();
        host.scroll(0.0, -10.0);
        let _ = host.poll_frame();
        assert!(
            built.get() - before <= 40,
            "a step of the list's scroll built {} cards",
            built.get() - before
        );
    }
    let shown = host.root::<Board>().shown_cards_for_tests(0);
    let keys: Vec<&str> = shown.iter().map(|(key, _)| key.as_str()).collect();
    assert!(
        (5..=12).contains(&shown.len()),
        "scrolled, the list shows the cards its height fits: {keys:?}"
    );
    assert!(
        !keys.contains(&"todo-0"),
        "scrolled, the list shows cards further down: {keys:?}"
    );
}

#[test]
fn a_card_dragged_between_long_lists_drops_before_the_card_under_it() {
    let built = Rc::new(std::cell::Cell::new(0));
    let drops = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&drops);
    let board = listed(3, &built).on_drop(move |drop| sink.borrow_mut().push(drop));
    let mut host = harness(board);
    let root = host.root::<Board>();
    let a = root.card_rects_for_tests(0)[0];
    let c = root.card_rects_for_tests(1)[0];
    host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
    assert_eq!(
        drops.borrow().as_slice(),
        [BoardDrop {
            card: "todo-0".into(),
            list: "done".into(),
            before: Some("done-0".into()),
        }]
    );
}

#[test]
fn dragging_a_card_into_another_list_reports_one_drop_before_the_card_under_it() {
    let drops = Rc::new(RefCell::new(Vec::new()));
    let mut host = harness(board(&drops));
    let root = host.root::<Board>();
    let a = root.card_rects_for_tests(0)[0];
    let c = root.card_rects_for_tests(1)[0];
    host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
    assert_eq!(
        drops.borrow().as_slice(),
        [BoardDrop {
            card: "a".into(),
            list: "done".into(),
            before: Some("c".into()),
        }]
    );
    assert!(!host.root::<Board>().is_dragging());
}

#[test]
fn dropping_below_every_card_lands_at_the_end_and_a_click_is_no_drop() {
    let drops = Rc::new(RefCell::new(Vec::new()));
    let mut host = harness(board(&drops));
    let root = host.root::<Board>();
    let a = root.card_rects_for_tests(0)[0];
    let b = root.card_rects_for_tests(0)[1];
    host.click_at(a.center().x, a.center().y);
    assert!(drops.borrow().is_empty(), "a click moves nothing");
    host.drag_at((a.center().x, a.center().y), (b.center().x, b.y1 + 40.0));
    assert_eq!(
        drops.borrow().as_slice(),
        [BoardDrop {
            card: "a".into(),
            list: "todo".into(),
            before: None,
        }]
    );
}
