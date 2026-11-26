//! A board's lists, headings and card rows stand where a list's do, and the
//! places a card drop reports are the lists' own.

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

use tilo_ui_base::Intents;
use tilo_ui_base::component::Event;

fn card_props(queue: &Intents<Fired>, key: &str) -> BoardCardProps {
    BoardCardProps {
        key: key.into(),
        item: Some(ResourceId(format!("res-{key}"))),
        body: vec![Box::new(Text::new("body"))],
        drop: Event::View(Intent::new(queue, key, "drop")),
        ..BoardCardProps::default()
    }
}

fn props(queue: &Intents<Fired>) -> BoardProps {
    BoardProps {
        name: Some("Plan".into()),
        lists: vec![
            BoardListProps {
                key: "todo".into(),
                title: Some("To do".into()),
                count: Some(2),
                cards: vec![card_props(queue, "a"), card_props(queue, "b")],
                drop: Event::View(Intent::new(queue, "todo", "drop")),
                ..BoardListProps::default()
            },
            BoardListProps {
                key: "done".into(),
                title: Some("Done".into()),
                cards: vec![card_props(queue, "c")],
                drop: Event::View(Intent::new(queue, "done", "drop")),
                ..BoardListProps::default()
            },
        ],
    }
}

fn harness(queue: &Intents<Fired>) -> Driver {
    laid_out(props(queue))
}

fn laid_out(props: BoardProps) -> Driver {
    Driver::for_root(
        tilo_ui::State::new(),
        <Board as Described>::from_props("board", props),
        (900, 600),
        Theme::dark(),
    )
}

fn list(key: &str, keys: &[&str]) -> BoardListProps {
    BoardListProps {
        key: key.into(),
        cards: keys
            .iter()
            .map(|card| BoardCardProps {
                key: (*card).into(),
                item: Some(ResourceId(format!("res-{card}"))),
                body: vec![Box::new(Text::new("body"))],
                ..BoardCardProps::default()
            })
            .collect(),
        ..BoardListProps::default()
    }
}

fn headings(frame: &tilo_ui::headless::InspectionSnapshot) -> usize {
    let mut children = vec![&frame.root];
    let mut found = 0;
    while let Some(node) = children.pop() {
        if node.widget == "SectionHeader" {
            found += 1;
        }
        children.extend(node.children.iter());
    }
    found
}

tilo_test_env::scenario! {
    /// List headings, measures and card rows of a board.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_board_lays_its_lists_out_like_lists;

    phase a_board_no_list_heads_starts_its_cards_where_a_list_starts_its_first_row;
    // A board no list heads has nothing to line its cards up with: no list keeps a heading's height,
    // so the first card stands where the first row of a list of the same items does.
    {
        let host = laid_out(BoardProps {
            name: Some("Loose".into()),
            lists: vec![list("loose", &["a", "b"])],
        });
        let (card, band) = {
            let root = host.root::<Board>();
            (root.card_rects_for_tests(0)[0], root.list_rects_for_tests()[0])
        };
        assert!(
            (card.y0 - band.y0).abs() < 0.5,
            "no band stands over the cards: {card:?} in a list from {band:?}"
        );
        let list = Driver::for_root(
            tilo_ui::State::new(),
            tilo_ui::widget::Column::new()
                .with_no_padding()
                .push(Box::new(tilo_ui::widget::ListRow::new("a", "body"))),
            (900, 600),
            Theme::dark(),
        );
        let list = list.inspection();
        let row = list
            .root
            .children
            .iter()
            .find(|node| node.widget == "ListRow")
            .map(|node| node.rect[1])
            .expect("the list shows its row");
        assert!(
            (card.y0 - row).abs() < 0.5,
            "the first card starts where a list's first row does: {card:?} against {row}"
        );
        assert_eq!(
            headings(&host.inspection()),
            0,
            "no list of the board heads itself, so no heading row stands over the cards"
        );
    }

    phase a_list_nobody_named_keeps_the_heading_row_of_the_lists_beside_it;
    // A list nobody named keeps a heading's height while a list beside it has one, so the cards of
    // every list line up and the unnamed list heads nothing.
    {
        let mut named = list("doing", &["c"]);
        named.title = Some("Doing".into());
        let host = laid_out(BoardProps {
            name: Some("Plan".into()),
            lists: vec![list("loose", &["a"]), named],
        });
        let (loose, doing, top) = {
            let root = host.root::<Board>();
            (
                root.card_rects_for_tests(0)[0],
                root.card_rects_for_tests(1)[0],
                root.list_rects_for_tests()[0].y0,
            )
        };
        assert!(
            (loose.y0 - doing.y0).abs() < 0.5,
            "the lists' cards line up: {loose:?} against {doing:?}"
        );
        assert!(
            loose.y0 > top,
            "the headings stand over the cards: {loose:?} in a list from {top}"
        );
        assert_eq!(
            headings(&host.inspection()),
            2,
            "the unnamed list keeps the heading row of the list beside it"
        );
    }

    phase a_card_whose_body_is_a_row_is_that_row_shown_as_a_card;
    {
        let mut props = props(&Intents::default());
        let row = tilo_ui::widget::ListRow::new("row", "Measure the new study").meta("Thursday");
        props.lists[0].cards[0].body = vec![Box::new(tilo_ui::widget::ContextMenuArea::new(
            Box::new(
                tilo_ui::widget::Container::new()
                    .with_no_padding()
                    .with_child(Box::new(row)),
            ),
            |_| Vec::new(),
        ))];
        let host = Driver::for_root(
            tilo_ui::State::new(),
            <Board as Described>::from_props("board", props),
            (900, 600),
            Theme::dark(),
        );
        let root = host.root::<Board>();
        let card = root.card_rects_for_tests(0)[0];
        assert!(
            (60.0..=70.0).contains(&card.height()),
            "a card of one title line and its facts is {} tall",
            card.height()
        );
    }


    phase a_board_in_a_sideways_pane_fits_lists_a_little_short_of_its_width;
    // Two lists at their comfortable measure fall a little short of a 656 wide column, so
    // they narrow to stand whole in it, side by side, rather than the second cut off at the
    // column's edge.
    {
        let board = Board::from_props("board", props(&Intents::default()));
        let pane = tilo_ui::widget::Scrollable::new_horizontal()
            .with_content_min_viewport_width()
            .with_child(Box::new(board));
        // The visible width of the pane: 656 wide, a column of the window.
        let window = tilo_ui::widget::Container::new()
            .with_no_padding()
            .with_width(656.0)
            .with_child(Box::new(pane));
        let mut host = Driver::for_root(
            tilo_ui::State::new(),
            window,
            (656, 600),
            Theme::dark(),
        );
        let mut window = host.root_mut::<tilo_ui::widget::Container>();
        let mut window_children = tilo_ui_base::widget::Widget::children_mut(&mut *window);
        let scroll = window_children[0]
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<tilo_ui::widget::Scrollable>())
            .expect("the window shows the pane");
        let mut pane_children = tilo_ui_base::widget::Widget::children_mut(scroll);
        let board = pane_children[0]
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Board>())
            .expect("the pane shows the board");
        let lists = board.list_rects_for_tests();
        assert_eq!(lists.len(), 2);
        let theme = Theme::dark();
        let list = f64::from(Board::lists_width_for_tests(&theme, 656.0, 2));
        let comfortable = f64::from(theme.tokens.metrics.board_list_width);
        assert!(list < comfortable, "two lists narrow to {list} in the column");
        for (index, rect) in lists.iter().enumerate() {
            assert!(
                (rect.width() - list).abs() < 0.5,
                "list {index} reads {rect:?}, {list} wide"
            );
        }
        assert!(
            (lists[1].x1 - lists[0].x0 - (2.0 * list + 12.0)).abs() < 0.5,
            "{lists:?}"
        );
        assert!(lists[1].x1 <= 656.0, "the second list stands whole: {lists:?}");
    }
}

#[test]
fn a_card_dropped_before_another_is_reported_by_that_card_carrying_the_dropped_one() {
    let queue = Intents::default();
    let mut host = harness(&queue);
    let root = host.root::<Board>();
    let a = root.card_rects_for_tests(0)[0];
    let c = root.card_rects_for_tests(1)[0];
    host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
    assert_eq!(
        queue.take(),
        [Fired {
            key: "c".into(),
            event: "drop".into(),
            value: Some(Value::Resource("res-a".into())),
            gesture: None,
        }]
    );
}

#[test]
fn a_card_dropped_below_a_lists_last_card_is_reported_by_the_list_with_its_place() {
    let queue = Intents::default();
    let mut host = harness(&queue);
    let root = host.root::<Board>();
    let b = root.card_rects_for_tests(0)[1];
    let c = root.card_rects_for_tests(1)[0];
    host.drag_at((b.center().x, b.center().y), (c.center().x, c.y1 + 40.0));
    assert_eq!(
        queue.take(),
        [Fired {
            key: "done".into(),
            event: "drop".into(),
            value: Some(Value::List(vec![
                Value::Resource("res-b".into()),
                Value::Int(1)
            ])),
            gesture: None,
        }]
    );
}

#[test]
fn where_cards_take_no_drops_the_list_reports_the_place_among_its_other_cards() {
    let queue = Intents::default();
    let mut props = props(&queue);
    for list in &mut props.lists {
        for card in &mut list.cards {
            card.drop = Event::None;
        }
    }
    let mut host = Driver::for_root(
        tilo_ui::State::new(),
        <Board as Described>::from_props("board", props),
        (900, 600),
        Theme::dark(),
    );
    let root = host.root::<Board>();
    let [a, b] = [
        root.card_rects_for_tests(0)[0],
        root.card_rects_for_tests(0)[1],
    ];
    let c = root.card_rects_for_tests(1)[0];
    host.drag_at((b.center().x, b.center().y), (a.center().x, a.y0 + 5.0));
    // Into the other list, above its only card.
    let root = host.root::<Board>();
    let a = root.card_rects_for_tests(0)[0];
    host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
    let carried = |card: &str, at: i64| {
        Some(Value::List(vec![
            Value::Resource(format!("res-{card}")),
            Value::Int(at),
        ]))
    };
    assert_eq!(
        queue.take(),
        [
            Fired {
                key: "todo".into(),
                event: "drop".into(),
                value: carried("b", 0),
                gesture: None,
            },
            Fired {
                key: "done".into(),
                event: "drop".into(),
                value: carried("a", 0),
                gesture: None,
            },
        ]
    );
}
