//! [`Board`] as a described component: lists of cards, a card moved by dragging it (a press and a drag
//! with a pointer, a hold and a drag on a touchscreen).

use std::collections::HashMap;

use tilo_ui_base::{component::Component, component::Described, component::Event, component::PartType, component::ResourceId, component::Source, component::Value};
use tilo_ui_base::icons::Icon;
use crate::{widget::Card, widget::CardBody, widget::CardProps, widget::ContextMenuArea, widget::MenuEntry, widget::MenuItemProps, widget::SectionHeader};
use tilo_ui_base::{widget::Element, widget::Press, widget::Widget, widget::capability::capability_mut};

use super::{Board, BoardDrop};

/// Cards in lists, side by side: a kanban board, a pipeline, tasks by status.
#[derive(Component, Default)]
#[component(name = "Board", role = "Group", label = "name")]
pub struct BoardProps {
    /// What the board is, for assistive technology.
    pub name: Option<String>,
    /// The lists, in order.
    #[component(part(default))]
    pub lists: Vec<BoardListProps>,
}

/// One list of a board: a heading and its cards, in order.
#[derive(Component, Default)]
#[component(name = "BoardList", role = "Group", label = "title", presents = "item")]
pub struct BoardListProps {
    /// The list's stable key.
    #[component(key)]
    pub key: String,
    /// The resource the list stands for: a column, a status.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// What the list is called, as its heading.
    pub title: Option<String>,
    /// How many cards the list holds, beside its heading.
    pub count: Option<i64>,
    /// The label of the one action that adds to the list, at its foot under the cards: "New card".
    pub action: Option<String>,
    /// The cards, in order.
    #[component(part(default))]
    pub cards: Vec<BoardCardProps>,
    /// What stands at the list's foot, under its cards: the entry that adds to the list.
    #[component(part)]
    pub foot: Option<Element>,
    /// The list's action was pressed.
    #[component(event)]
    pub act: Event,
    /// A card was dropped into the list: carries the card and its place among the list's other cards, from
    /// 0.
    #[component(event(value = ResourceId, at = i64))]
    pub drop: Event,
}

/// One card of a list: a summary of the resource it stands for, which a press opens.
#[derive(Component, Default)]
#[component(name = "BoardCard", role = "Generic", presents = "item")]
pub struct BoardCardProps {
    /// The card's stable key.
    #[component(key)]
    pub key: String,
    /// The resource the card stands for: what a drop carries when it is the card dropped.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// Whether it is the selected one.
    pub selected: bool,
    /// The commands of its context menu: a right-click, the Menu key or a long press opens it.
    #[component(part)]
    pub menu: Vec<MenuItemProps>,
    /// What the card shows, in order.
    #[component(part(default))]
    pub body: Vec<Element>,
    /// The card was pressed: open what it stands for.
    #[component(event(press))]
    pub activate: Event,
    /// Another card was dropped just before this one: carries that card.
    #[component(event(value = ResourceId))]
    pub drop: Event,
}

fn read_all<T>(
    source: &mut dyn Source,
    part: &str,
    read: fn(&mut dyn Source) -> Result<T, tilo_ui_base::component::Error>,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    source.components(part, &mut |child| {
        out.push(read(child).map_err(|error| error.to_string())?);
        Ok(())
    })?;
    Ok(out)
}

/// A board's lists: [`BoardListProps`] only.
impl PartType for Vec<BoardListProps> {
    const MANY: bool = true;

    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::Component(BoardListProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        *self = read_all(source, part, BoardListProps::from_source)?;
        Ok(())
    }
}

/// A list's cards: [`BoardCardProps`] only.
impl PartType for Vec<BoardCardProps> {
    const MANY: bool = true;

    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::Component(BoardCardProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        *self = read_all(source, part, BoardCardProps::from_source)?;
        Ok(())
    }
}

/// One command of a card's context menu, kept so the menu can be offered each time it opens.
struct Command {
    label: String,
    icon: Option<Icon>,
    destructive: bool,
    checked: Option<bool>,
    key: Option<String>,
    group: Option<String>,
    enabled: bool,
    description: Option<String>,
    press: Press,
}

impl Command {
    fn of(props: MenuItemProps) -> Self {
        let MenuItemProps {
            label,
            icon,
            destructive,
            checked,
            disabled,
            description,
            group,
            key,
            link,
            choose,
            separator: _,
            // A board's commands are not a font picker's.
            sample: _,
        } = props;
        Self {
            label,
            icon,
            destructive,
            checked,
            key,
            group,
            enabled: !disabled,
            description,
            press: Press::of(choose).leading_to(link),
        }
    }

    fn entry(&self) -> MenuEntry {
        let entry = MenuEntry::bound(self.label.clone(), self.press.clone())
            .with_destructive(self.destructive);
        let entry = match self.icon {
            Some(icon) => entry.with_icon(icon),
            None => entry,
        };
        let entry = match self.checked {
            Some(checked) => entry.with_checked(checked),
            None => entry,
        };
        let entry = entry.with_enabled(self.enabled);
        let entry = match &self.key {
            Some(key) => entry.with_shortcut(key.clone()),
            None => entry,
        };
        let entry = match &self.description {
            Some(description) => entry.with_description(description.clone()),
            None => entry,
        };
        match &self.group {
            Some(group) => entry.with_group(group.clone()),
            None => entry,
        }
    }
}

fn card(props: BoardCardProps) -> (String, Element, Option<ResourceId>, Event) {
    let BoardCardProps {
        key,
        item,
        selected,
        menu,
        body,
        activate,
        drop,
    } = props;
    let widget = card_body(&key, body, item.clone(), selected, activate);
    let widget: Element = if menu.is_empty() {
        widget
    } else {
        let commands: Vec<Command> = menu.into_iter().map(Command::of).collect();
        Box::new(ContextMenuArea::new(widget, move |_| {
            commands.iter().map(Command::entry).collect()
        }))
    };
    (key, widget, item, drop)
}

fn card_body(
    key: &str,
    mut body: Vec<Element>,
    item: Option<ResourceId>,
    selected: bool,
    activate: Event,
) -> Element {
    if body.len() == 1 {
        let mut element = body.remove(0);
        if show_as_card(element.as_mut(), selected, activate.clone()) {
            return element;
        }
        body.push(element);
    }
    Box::new(Card::from_props(
        key,
        CardProps {
            title: None,
            accessible_name: None,
            count: None,
            item,
            selected,
            body,
            activate,
            hover: Event::None,
        },
    ))
}

fn show_as_card(widget: &mut dyn Widget, selected: bool, activate: Event) -> bool {
    if let Some(card) = capability_mut::<dyn CardBody>(widget) {
        card.show_as_card(selected, activate);
        return true;
    }
    let mut children = widget.children_mut();
    if children.len() != 1 {
        return false;
    }
    show_as_card(children.remove(0), selected, activate)
}

struct Lists {
    /// Each list's key, heading, cards and foot, in order.
    columns: Vec<(
        String,
        Option<Element>,
        Vec<(String, Element)>,
        Option<Element>,
    )>,
    /// What each card stands for.
    items: HashMap<String, ResourceId>,
    /// What lands before each card that takes drops itself.
    before: HashMap<String, Event>,
    /// Each list's own drop, with the keys of its cards in order.
    list_drops: HashMap<String, (Event, Vec<String>)>,
}

fn lists_of(lists: Vec<BoardListProps>) -> Lists {
    // Whether any list of the board heads itself: a list nobody named heads nothing, but
    // keeps a heading's height while a list beside it has one, so the lists' cards line up.
    // On a board no list heads, no list keeps it, and the cards start where a list's first
    // row stands.
    let headed = lists.iter().any(|list| list.title.is_some());
    let mut built = Lists {
        columns: Vec::with_capacity(lists.len()),
        items: HashMap::new(),
        before: HashMap::new(),
        list_drops: HashMap::new(),
    };
    for list in lists {
        let BoardListProps {
            key: list_key,
            item: _,
            title,
            count,
            action,
            cards,
            foot,
            act,
            drop,
        } = list;
        let named = title.is_some();
        let heading = (named || headed).then(|| {
            let heading = SectionHeader::group(title.unwrap_or_default());
            match count {
                Some(count) => heading.count(usize::try_from(count).unwrap_or(0)),
                None => heading,
            }
            .into_element()
        });
        let foot = foot.or_else(|| {
            let label = action?;
            // Quiet under the cards: what adds to a list is always there, so it never
            // outweighs the cards it adds to.
            let button = crate::widget::Button::new(&format!("{list_key}-act"), &label)
                .with_icon(Icon::Plus)
                .role(crate::widget::ButtonRole::Plain);
            let button = match act {
                Event::None => button,
                Event::Action(binding) => button.on_action(binding),
                Event::View(intent) => button.on_open(intent.sender()),
            };
            Some(Box::new(button) as Element)
        });
        let mut widgets = Vec::with_capacity(cards.len());
        let mut order = Vec::with_capacity(cards.len());
        for props in cards {
            let (card_key, widget, item, drop) = card(props);
            if let Some(item) = item {
                built.items.insert(card_key.clone(), item);
            }
            if !matches!(drop, Event::None) {
                built.before.insert(card_key.clone(), drop);
            }
            order.push(card_key.clone());
            widgets.push((card_key, widget));
        }
        built.list_drops.insert(list_key.clone(), (drop, order));
        built.columns.push((list_key, heading, widgets, foot));
    }
    built
}

fn drop_handler(built: &Lists) -> Box<dyn FnMut(BoardDrop)> {
    let Lists {
        columns: _,
        items,
        before,
        list_drops,
    } = built;
    let (items, before, list_drops) = (items.clone(), before.clone(), list_drops.clone());
    Box::new(
        move |BoardDrop {
                  card,
                  list,
                  before: next,
              }| {
            let Some(moved) = items.get(&card) else {
                return;
            };
            let moved = Value::Resource(moved.0.clone());
            if let Some(target) = next.as_ref().and_then(|next| before.get(next)) {
                target.fire(Some(moved));
                return;
            }
            let Some((drop, order)) = list_drops.get(&list) else {
                return;
            };
            let others = order.iter().filter(|key| **key != card);
            let at = match &next {
                Some(next) => others.take_while(|key| *key != next).count(),
                None => others.count(),
            };
            let at = i64::try_from(at).unwrap_or(i64::MAX);
            drop.fire(Some(Value::List(vec![moved, Value::Int(at)])));
        },
    )
}

impl Described for Board {
    type Props = BoardProps;

    fn from_props(key: &str, props: BoardProps) -> Self {
        let BoardProps { name, lists } = props;
        let built = lists_of(lists);
        let on_drop = drop_handler(&built);
        let mut board = Self::new().with_id(key.to_owned());
        board.name = name;
        board.set_lists(built.columns);
        board.set_on_drop(on_drop);
        board
    }

    /// Props given in place carry none of the views a board's cards and list feet show, and
    /// each card is its view (a row shown as a card, under its menu): lists made from those
    /// props would show every card empty and no entry under any list. The board is built
    /// again instead, from what the view says with its views.
    fn set_props(&mut self, props: BoardProps) -> bool {
        let _ = props;
        false
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use tilo_ui_base::Intents;
    use tilo_ui_base::{component::Fired, component::Intent};
    use tilo_ui_base::theme::Theme;
    use tilo_ui_base::tokens::Rhythm;
    use crate::widget::Text;

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

    fn harness(queue: &Intents<Fired>) -> crate::app::driver::Driver {
        laid_out(props(queue))
    }

    fn laid_out(props: BoardProps) -> crate::app::driver::Driver {
        crate::app::driver::Driver::for_root(
            crate::State::new(),
            Board::from_props("board", props),
            (900, 600),
            Theme::dark(),
        )
    }

    fn published(resource: &str, action: &str) -> tilo_actions::ActionBinding<()> {
        let mut spec = tilo_actions::ResourceSpec::new(resource, "test.card", "Card");
        spec = spec.with_action(
            tilo_actions::ActionSpec::operation(
                action,
                action,
                tilo_actions::OperationKind::Effect,
                "test.card.effect",
                &action,
            )
            .expect("a test action"),
        );
        tilo_actions::ActionBinding::unit_for(&spec, action).expect("the card publishes it")
    }

    tilo_test_env::scenario! {
        /// What a board's cards run and report.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_boards_cards_bind_actions_and_report_drops;

        phase a_card_bound_to_an_action_runs_it_once_as_the_person_presses_it;
        {
            let bound = published("res-a", "open");
            let address = bound.address().clone();
            let mut card = card_body(
                "a",
                vec![Box::new(Text::new("body"))],
                Some(ResourceId("res-a".into())),
                false,
                Event::Action(bound),
            );
            let mut doc = automerge::AutoCommit::new();
            let mut state = tilo_ui_base::widget::Edits::lend(&mut doc);
            let path = automerge::ROOT;
            assert!(
                card.perform_action(&tilo_ui_base::a11y::Action::Activate, &[], &mut state, &path),
                "the card takes the press"
            );
            let taken = tilo_ui_base::widget::controls::take_control_invocation(&mut *card);
            assert_eq!(
                taken.as_ref().map(|taken| taken.address.clone()),
                Some(address),
                "one press runs the card's action once"
            );
            assert!(
                tilo_ui_base::widget::controls::take_control_invocation(&mut *card).is_none(),
                "the press is taken once"
            );
        }

        phase a_boards_inventory_carries_the_action_its_card_binds;
        {
            let bound = published("res-a", "open");
            let address = bound.address().clone();
            let board: Board = Described::from_props(
                "board",
                BoardProps {
                    name: Some("Plan".into()),
                    lists: vec![BoardListProps {
                        key: "todo".into(),
                        cards: vec![BoardCardProps {
                            key: "a".into(),
                            item: Some(ResourceId("res-a".into())),
                            body: vec![Box::new(Text::new("body"))],
                            activate: Event::Action(bound),
                            ..BoardCardProps::default()
                        }],
                        ..BoardListProps::default()
                    }],
                },
            );
            let bindings = tilo_ui_base::widget::controls::control_action_bindings(&board);
            let card = bindings
                .iter()
                .find(|binding| {
                    matches!(
                        &binding.kind,
                        tilo_ui_base::widget::ControlBindingKind::Action(found) if *found == address
                    )
                })
                .expect("the card's press is a control of the board");
            assert_eq!(card.control_id, "a", "named by the card's own key");
        }
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

    fn headings(frame: &crate::headless::InspectionSnapshot) -> usize {
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
                (root.card_rects(0)[0], root.list_rects()[0])
            };
            assert!(
                (card.y0 - band.y0).abs() < 0.5,
                "no band stands over the cards: {card:?} in a list from {band:?}"
            );
            let list = crate::app::driver::Driver::for_root(
                crate::State::new(),
                crate::widget::Column::new()
                    .with_no_padding()
                    .push(Box::new(crate::widget::ListRow::new("a", "body"))),
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
                    root.card_rects(0)[0],
                    root.card_rects(1)[0],
                    root.list_rects()[0].y0,
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
            let row = crate::widget::ListRow::new("row", "Measure the new study").meta("Thursday");
            props.lists[0].cards[0].body = vec![Box::new(crate::widget::ContextMenuArea::new(
                Box::new(
                    crate::widget::Container::new()
                        .with_no_padding()
                        .with_child(Box::new(row)),
                ),
                |_| Vec::new(),
            ))];
            let host = crate::app::driver::Driver::for_root(
                crate::State::new(),
                Board::from_props("board", props),
                (900, 600),
                Theme::dark(),
            );
            let root = host.root::<Board>();
            let card = root.card_rects(0)[0];
            assert!(
                (60.0..=70.0).contains(&card.height()),
                "a card of one title line and its facts is {} tall",
                card.height()
            );
        }

        phase a_list_keeps_a_comfortable_measure;
        {
            let theme = Theme::dark();
            let list = f64::from(Board::lists_width(&theme, 1280.0, 1));
            assert!((300.0..=340.0).contains(&list), "a list reads {list} wide");
            // Wider panes leave the extra width open beside the lists instead of stretching them.
            assert_eq!(
                Board::lists_width(&theme, 2000.0, 1),
                Board::lists_width(&theme, 1280.0, 1)
            );
            // A pane too narrow for one list and the board's gutters gives its list the pane, so
            // the next list does not show as a sliver.
            let narrow = 350.0;
            assert!((f64::from(Board::lists_width(&theme, narrow, 1)) - (narrow - 48.0)).abs() < 0.5);
            let least = f64::from(crate::widget::CardWidth::Regular.min_width(&theme));
            assert!(f64::from(Board::lists_width(&theme, least, 1)) >= least);
            // Three lists a little short of fitting a pane narrow so all three stand whole, the
            // last not cut off at the pane's edge; three that cannot fit at a card's measure keep
            // their comfortable one and scroll.
            let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
            let gap = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Sibling));
            let pane = 2.0 * gutter + 3.0 * list + 2.0 * gap - 40.0;
            let fitted = f64::from(Board::lists_width(&theme, pane, 3));
            assert!(fitted < list && fitted >= least, "three lists read {fitted} wide in {pane}");
            assert!(
                (2.0 * gutter + 3.0 * fitted + 2.0 * gap - pane).abs() < 0.5,
                "three lists fill the pane exactly"
            );
            assert_eq!(f64::from(Board::lists_width(&theme, 2.0 * least, 3)), list);
        }

        phase a_board_in_a_sideways_pane_fits_lists_a_little_short_of_its_width;
        // Two lists at their comfortable measure fall a little short of a 656 wide column, so
        // they narrow to stand whole in it, side by side, rather than the second cut off at the
        // column's edge.
        {
            let board = Board::from_props("board", props(&Intents::default()));
            let pane = crate::widget::Scrollable::new_horizontal()
                .with_content_min_viewport_width()
                .with_child(Box::new(board));
            // The visible width of the pane: 656 wide, a column of the window.
            let window = crate::widget::Container::new()
                .with_no_padding()
                .with_width(656.0)
                .with_child(Box::new(pane));
            let mut host = crate::app::driver::Driver::for_root(
                crate::State::new(),
                window,
                (656, 600),
                Theme::dark(),
            );
            let mut window = host.root_mut::<crate::widget::Container>();
            let mut window_children = tilo_ui_base::widget::Widget::children_mut(&mut *window);
            let scroll = window_children[0]
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::Scrollable>())
                .expect("the window shows the pane");
            let mut pane_children = tilo_ui_base::widget::Widget::children_mut(scroll);
            let board = pane_children[0]
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Board>())
                .expect("the pane shows the board");
            let lists = board.list_rects();
            assert_eq!(lists.len(), 2);
            let theme = Theme::dark();
            let list = f64::from(Board::lists_width(&theme, 656.0, 2));
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
        let a = root.card_rects(0)[0];
        let c = root.card_rects(1)[0];
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
        let b = root.card_rects(0)[1];
        let c = root.card_rects(1)[0];
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
        let mut host = crate::app::driver::Driver::for_root(
            crate::State::new(),
            Board::from_props("board", props),
            (900, 600),
            Theme::dark(),
        );
        let root = host.root::<Board>();
        let [a, b] = [root.card_rects(0)[0], root.card_rects(0)[1]];
        let c = root.card_rects(1)[0];
        host.drag_at((b.center().x, b.center().y), (a.center().x, a.y0 + 5.0));
        // Into the other list, above its only card.
        let root = host.root::<Board>();
        let a = root.card_rects(0)[0];
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
}
