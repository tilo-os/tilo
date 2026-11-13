//! [`Board`] as a described component: lanes of cards, a card moved by dragging it (a press and a drag
//! with a pointer, a hold and a drag on a touchscreen).

use std::collections::HashMap;

use crate::component::{Component, Described, Event, ResourceId, SlotType, Source, Value};
use crate::icons::Icon;
use crate::widget::{
    Card, CardBody, CardProps, ContextMenuArea, Element, MenuEntry, MenuItemProps, Press,
    SectionHeader, Widget, capability::capability_mut,
};

use super::{Board, BoardDrop};

/// Cards in lanes, side by side: a kanban board, a pipeline, tasks by status.
#[derive(Component, Default)]
#[component(name = "Board", role = "Group", label = "name")]
pub struct BoardProps {
    /// What the board is, for assistive technology.
    pub name: Option<String>,
    /// The lanes, in order.
    #[component(slot(default))]
    pub lanes: Vec<BoardLaneProps>,
}

/// One lane of a board: a heading and its cards, in order.
#[derive(Component, Default)]
#[component(name = "BoardLane", role = "Group", label = "title", presents = "item")]
pub struct BoardLaneProps {
    /// The lane's stable key.
    #[component(key)]
    pub key: String,
    /// The resource the lane stands for: a column, a status.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// What the lane is called, as its heading.
    pub title: Option<String>,
    /// How many cards the lane holds, beside its heading.
    pub count: Option<i64>,
    /// The label of the one action that adds to the lane, at its foot under the cards: "New card".
    pub action: Option<String>,
    /// The cards, in order.
    #[component(slot(default))]
    pub cards: Vec<BoardCardProps>,
    /// What stands at the lane's foot, under its cards: the entry that adds to the lane.
    #[component(slot)]
    pub foot: Option<Element>,
    /// The lane's action was pressed.
    #[component(event)]
    pub act: Event,
    /// A card was dropped into the lane: carries the card and its place among the lane's other cards, from
    /// 0.
    #[component(event(value = ResourceId, at = i64))]
    pub drop: Event,
}

/// One card of a lane: a summary of the resource it stands for, which a press opens.
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
    #[component(slot)]
    pub menu: Vec<MenuItemProps>,
    /// What the card shows, in order.
    #[component(slot(default))]
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
    slot: &str,
    read: fn(&mut dyn Source) -> Result<T, crate::component::Error>,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    source.components(slot, &mut |child| {
        out.push(read(child).map_err(|error| error.to_string())?);
        Ok(())
    })?;
    Ok(out)
}

/// A board's lanes: [`BoardLaneProps`] only.
impl SlotType for Vec<BoardLaneProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(BoardLaneProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        *self = read_all(source, slot, BoardLaneProps::from_source)?;
        Ok(())
    }
}

/// A lane's cards: [`BoardCardProps`] only.
impl SlotType for Vec<BoardCardProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(BoardCardProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        *self = read_all(source, slot, BoardCardProps::from_source)?;
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
    reason: Option<String>,
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
            reason,
            group,
            key,
            choose,
        } = props;
        Self {
            label,
            icon,
            destructive,
            checked,
            key,
            group,
            enabled: !disabled,
            reason,
            press: Press::of(choose),
        }
    }

    fn entry(&self) -> MenuEntry {
        let run = self.press.runner();
        let entry = if self.destructive {
            MenuEntry::destructive(self.label.clone(), move || run())
        } else {
            MenuEntry::new(self.label.clone(), move || run())
        }
        .for_press(self.press.clone());
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
        let entry = match &self.reason {
            Some(reason) => entry.with_description(reason.clone()),
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
            count: None,
            item,
            selected,
            body,
            activate,
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

struct Lanes {
    /// Each lane's key, heading, cards and foot, in order.
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
    /// Each lane's own drop, with the keys of its cards in order.
    lane_drops: HashMap<String, (Event, Vec<String>)>,
}

fn lanes_of(lanes: Vec<BoardLaneProps>) -> Lanes {
    // Whether any lane of the board heads itself: a lane nobody named heads nothing, but
    // keeps a heading's height while a lane beside it has one, so the lanes' cards line up.
    // On a board no lane heads, no lane keeps it, and the cards start where a list's first
    // row stands.
    let headed = lanes.iter().any(|lane| lane.title.is_some());
    let mut built = Lanes {
        columns: Vec::with_capacity(lanes.len()),
        items: HashMap::new(),
        before: HashMap::new(),
        lane_drops: HashMap::new(),
    };
    for lane in lanes {
        let BoardLaneProps {
            key: lane_key,
            item: _,
            title,
            count,
            action,
            cards,
            foot,
            act,
            drop,
        } = lane;
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
            let button = crate::widget::Button::new(&format!("{lane_key}-act"), &label)
                .with_icon(Icon::Plus);
            let button = match act {
                Event::None => button,
                Event::Action(binding) => button.on_action(binding),
                Event::View(intent) => button.on_view(intent.sender()),
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
        built.lane_drops.insert(lane_key.clone(), (drop, order));
        built.columns.push((lane_key, heading, widgets, foot));
    }
    built
}

fn drop_handler(built: &Lanes) -> Box<dyn FnMut(BoardDrop)> {
    let Lanes {
        columns: _,
        items,
        before,
        lane_drops,
    } = built;
    let (items, before, lane_drops) = (items.clone(), before.clone(), lane_drops.clone());
    Box::new(
        move |BoardDrop {
                  card,
                  lane,
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
            let Some((drop, order)) = lane_drops.get(&lane) else {
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
        let BoardProps { name, lanes } = props;
        let built = lanes_of(lanes);
        let on_drop = drop_handler(&built);
        let mut board = Self::new().with_id(key.to_owned());
        board.name = name;
        board.set_lanes(built.columns);
        board.set_on_drop(on_drop);
        board
    }

    fn set_props(&mut self, props: BoardProps) -> bool {
        let BoardProps { name, lanes } = props;
        let shaped: Vec<(String, Vec<String>)> = lanes
            .iter()
            .map(|lane| {
                (
                    lane.key.clone(),
                    lane.cards.iter().map(|card| card.key.clone()).collect(),
                )
            })
            .collect();
        if !self.holds(&shaped) {
            return false;
        }
        let built = lanes_of(lanes);
        let on_drop = drop_handler(&built);
        self.name = name;
        self.set_lanes(built.columns);
        self.set_on_drop(on_drop);
        true
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
    use crate::Intents;
    use crate::component::{Fired, Intent};
    use crate::theme::Theme;
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
            lanes: vec![
                BoardLaneProps {
                    key: "todo".into(),
                    title: Some("To do".into()),
                    count: Some(2),
                    cards: vec![card_props(queue, "a"), card_props(queue, "b")],
                    drop: Event::View(Intent::new(queue, "todo", "drop")),
                    ..BoardLaneProps::default()
                },
                BoardLaneProps {
                    key: "done".into(),
                    title: Some("Done".into()),
                    cards: vec![card_props(queue, "c")],
                    drop: Event::View(Intent::new(queue, "done", "drop")),
                    ..BoardLaneProps::default()
                },
            ],
        }
    }

    fn harness(queue: &Intents<Fired>) -> crate::headless::FrameHarness {
        laid_out(props(queue))
    }

    fn laid_out(props: BoardProps) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::for_root(
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

    #[test]
    fn a_card_bound_to_an_action_runs_it_once_as_the_person_presses_it() {
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
        let mut state = crate::widget::Edits::lend(&mut doc);
        let path = automerge::ROOT;
        assert!(
            card.perform_action(&crate::a11y::Action::Activate, &[], &mut state, &path),
            "the card takes the press"
        );
        let taken = crate::widget::controls::take_control_invocation(&mut *card);
        assert_eq!(
            taken.as_ref().map(|taken| taken.address.clone()),
            Some(address),
            "one press runs the card's action once"
        );
        assert!(
            crate::widget::controls::take_control_invocation(&mut *card).is_none(),
            "the press is taken once"
        );
    }

    #[test]
    fn a_boards_inventory_carries_the_action_its_card_binds() {
        let bound = published("res-a", "open");
        let address = bound.address().clone();
        let board: Board = Described::from_props(
            "board",
            BoardProps {
                name: Some("Plan".into()),
                lanes: vec![BoardLaneProps {
                    key: "todo".into(),
                    cards: vec![BoardCardProps {
                        key: "a".into(),
                        item: Some(ResourceId("res-a".into())),
                        body: vec![Box::new(Text::new("body"))],
                        activate: Event::Action(bound),
                        ..BoardCardProps::default()
                    }],
                    ..BoardLaneProps::default()
                }],
            },
        );
        let bindings = crate::widget::controls::control_action_bindings(&board);
        let card = bindings
            .iter()
            .find(|binding| {
                matches!(
                    &binding.kind,
                    crate::widget::ControlBindingKind::Action(found) if *found == address
                )
            })
            .expect("the card's press is a control of the board");
        assert_eq!(card.control_id, "a", "named by the card's own key");
    }

    fn lane(key: &str, keys: &[&str]) -> BoardLaneProps {
        BoardLaneProps {
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
            ..BoardLaneProps::default()
        }
    }

    /// A board no lane heads has nothing to line its cards up with: no lane keeps a heading's height,
    /// so the first card stands where the first row of a list of the same items does.
    #[test]
    fn a_board_no_lane_heads_starts_its_cards_where_a_list_starts_its_first_row() {
        let host = laid_out(BoardProps {
            name: Some("Loose".into()),
            lanes: vec![lane("loose", &["a", "b"])],
        });
        let (card, lane) = {
            let root = host.root::<Board>();
            (root.card_rects(0)[0], root.lane_rects()[0])
        };
        assert!(
            (card.y0 - lane.y0).abs() < 0.5,
            "no band stands over the cards: {card:?} in a lane from {lane:?}"
        );
        let list = crate::headless::FrameHarness::for_root(
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
            "no lane of the board heads itself, so no heading row stands over the cards"
        );
    }

    /// A lane nobody named keeps a heading's height while a lane beside it has one, so the cards of
    /// every lane line up and the unnamed lane heads nothing.
    #[test]
    fn a_lane_nobody_named_keeps_the_heading_row_of_the_lanes_beside_it() {
        let mut named = lane("doing", &["c"]);
        named.title = Some("Doing".into());
        let host = laid_out(BoardProps {
            name: Some("Plan".into()),
            lanes: vec![lane("loose", &["a"]), named],
        });
        let (loose, doing, top) = {
            let root = host.root::<Board>();
            (
                root.card_rects(0)[0],
                root.card_rects(1)[0],
                root.lane_rects()[0].y0,
            )
        };
        assert!(
            (loose.y0 - doing.y0).abs() < 0.5,
            "the lanes' cards line up: {loose:?} against {doing:?}"
        );
        assert!(
            loose.y0 > top,
            "the headings stand over the cards: {loose:?} in a lane from {top}"
        );
        assert_eq!(
            headings(&host.inspection()),
            2,
            "the unnamed lane keeps the heading row of the lane beside it"
        );
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

    #[test]
    fn a_card_whose_body_is_a_row_is_that_row_shown_as_a_card() {
        let mut props = props(&Intents::default());
        let row = crate::widget::ListRow::new("row", "Measure the new bedroom").meta("Thursday");
        props.lanes[0].cards[0].body = vec![Box::new(crate::widget::ContextMenuArea::new(
            Box::new(
                crate::widget::Container::new()
                    .with_no_padding()
                    .with_child(Box::new(row)),
            ),
            |_| Vec::new(),
        ))];
        let host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            Board::from_props("board", props),
            (900, 600),
            Theme::dark(),
        );
        let root = host.root::<Board>();
        let card = root.card_rects(0)[0];
        assert!(
            (44.0..=56.0).contains(&card.height()),
            "a card of one title line and its facts is {} tall",
            card.height()
        );
    }

    #[test]
    fn a_lane_keeps_a_comfortable_measure() {
        let theme = Theme::dark();
        let lane = f64::from(Board::lane_width(&theme, 1280.0));
        assert!((300.0..=340.0).contains(&lane), "a lane reads {lane} wide");
        // Wider panes leave the extra room open beside the lanes instead of stretching them.
        assert_eq!(
            Board::lane_width(&theme, 2000.0),
            Board::lane_width(&theme, 1280.0)
        );
        // A pane too narrow for one lane and the board's gutters gives its lane the pane, so
        // the next lane does not show as a sliver.
        let narrow = 350.0;
        assert!((f64::from(Board::lane_width(&theme, narrow)) - (narrow - 48.0)).abs() < 0.5);
        let least = f64::from(crate::widget::CardWidth::Regular.min_width(&theme));
        assert!(f64::from(Board::lane_width(&theme, least)) >= least);
    }

    #[test]
    fn a_board_in_a_sideways_pane_keeps_its_lanes_a_comfortable_measure() {
        let board = Board::from_props("board", props(&Intents::default()));
        let pane = crate::widget::Scrollable::new_horizontal()
            .with_content_min_viewport_width()
            .with_child(Box::new(board));
        // The room the pane shows through: 656 wide, a column of the window.
        let column = crate::widget::Container::new()
            .with_no_padding()
            .with_width(656.0)
            .with_child(Box::new(pane));
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            column,
            (656, 600),
            Theme::dark(),
        );
        let mut column = host.root_mut::<crate::widget::Container>();
        let mut column_children = crate::widget::Widget::children_mut(&mut *column);
        let scroll = column_children[0]
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::widget::Scrollable>())
            .expect("the column shows the pane");
        let mut pane_children = crate::widget::Widget::children_mut(scroll);
        let board = pane_children[0]
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Board>())
            .expect("the pane shows the board");
        let lanes = board.lane_rects();
        assert_eq!(lanes.len(), 2);
        let theme = Theme::dark();
        let lane = f64::from(theme.tokens.metrics.board_lane_width);
        for (index, rect) in lanes.iter().enumerate() {
            assert!(
                (rect.width() - lane).abs() < 0.5,
                "lane {index} reads {rect:?}, a comfortable measure {lane} wide"
            );
        }
        assert!(
            (lanes[1].x1 - lanes[0].x0 - (2.0 * lane + 12.0)).abs() < 0.5,
            "{lanes:?}"
        );
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
            }]
        );
    }

    #[test]
    fn a_card_dropped_below_a_lanes_last_card_is_reported_by_the_lane_with_its_place() {
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
            }]
        );
    }

    #[test]
    fn where_cards_take_no_drops_the_lane_reports_the_place_among_its_other_cards() {
        let queue = Intents::default();
        let mut props = props(&queue);
        for lane in &mut props.lanes {
            for card in &mut lane.cards {
                card.drop = Event::None;
            }
        }
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            Board::from_props("board", props),
            (900, 600),
            Theme::dark(),
        );
        let root = host.root::<Board>();
        let [a, b] = [root.card_rects(0)[0], root.card_rects(0)[1]];
        let c = root.card_rects(1)[0];
        host.drag_at((b.center().x, b.center().y), (a.center().x, a.y0 + 5.0));
        // Into the other lane, above its only card.
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
                },
                Fired {
                    key: "done".into(),
                    event: "drop".into(),
                    value: carried("a", 0),
                },
            ]
        );
    }
}
