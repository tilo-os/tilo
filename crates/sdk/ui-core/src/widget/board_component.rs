//! [`Board`] as a described component: lanes of cards, a card moved by dragging it (a press
//! and a drag with a pointer, a hold and a drag on a touchscreen).
//!
//! The board changes nothing itself. A drop is an event of where the card lands: the lane
//! ([`BoardLaneProps::drop`]), carrying the card and its place among the lane's other cards,
//! which the owner moves it to (`move(card, to: at, among: lane)`); or, when the card it lands
//! before takes drops itself ([`BoardCardProps::drop`]), that card, carrying the dropped one, so
//! the owner can name the place by its neighbour (`move(card, before: other)`). Either way the
//! drop is one action on its model; the board then shows the card where the model puts it.

use std::collections::HashMap;
use std::rc::Rc;

use crate::component::{Component, Described, Event, ResourceId, SlotType, Source, Value};
use crate::icons::Icon;
use crate::widget::{
    Card, CardProps, ContextMenuArea, Element, MenuEntry, MenuItemProps, SectionHeader,
};

use super::{Board, BoardDrop};

/// Cards in lanes, side by side: a kanban board, a pipeline, tasks by status. Dragging a card
/// to another place in its lane or into another lane is reported by the card or lane it lands
/// at; each card also opens, and offers its commands on its context menu, so nothing needs a
/// drag.
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
    /// The label of the one action that adds to the lane, at the end of its heading: "New
    /// card".
    pub action: Option<String>,
    /// The cards, in order.
    #[component(slot(default))]
    pub cards: Vec<BoardCardProps>,
    /// The lane's action was pressed.
    #[component(event)]
    pub act: Event,
    /// A card was dropped into the lane: carries the card and its place among the lane's other
    /// cards, from 0. A card that takes drops itself reports those that land just before it.
    #[component(event(value = ResourceId, at = i64))]
    pub drop: Event,
}

/// One card of a lane: a summary of the resource it stands for, which a press opens.
#[derive(Component, Default)]
#[component(
    name = "BoardCard",
    role = "ListItem",
    label = "title",
    presents = "item"
)]
pub struct BoardCardProps {
    /// The card's stable key.
    #[component(key)]
    pub key: String,
    /// The resource the card stands for: what a drop carries when it is the card dropped.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// What the card is called, in its head; without one the body speaks for it.
    pub title: Option<String>,
    /// Whether it is the selected one.
    pub selected: bool,
    /// The commands of its context menu: a right-click, the Menu key or a long press opens it.
    #[component(slot)]
    pub menu: Vec<MenuItemProps>,
    /// What the card shows, in order.
    #[component(slot(default))]
    pub body: Vec<Element>,
    /// The card was pressed: open what it stands for.
    #[component(event)]
    pub activate: Event,
    /// Another card was dropped just before this one: carries that card.
    #[component(event(value = ResourceId))]
    pub drop: Event,
}

/// Read every component of `slot` with `read`.
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
    run: Rc<dyn Fn()>,
}

impl Command {
    fn of(props: MenuItemProps) -> Self {
        let MenuItemProps {
            label,
            icon,
            destructive,
            checked: _,
            choose,
        } = props;
        Self {
            label,
            icon,
            destructive,
            run: choose.command().unwrap_or_else(|| Rc::new(|| {})),
        }
    }

    fn entry(&self) -> MenuEntry {
        let run = Rc::clone(&self.run);
        let entry = if self.destructive {
            MenuEntry::destructive(self.label.clone(), move || run())
        } else {
            MenuEntry::new(self.label.clone(), move || run())
        };
        match self.icon {
            Some(icon) => entry.with_icon(icon),
            None => entry,
        }
    }
}

/// The card widget of `props`, and what a drop on it or of it needs: the resource it stands
/// for and its own drop event.
fn card(props: BoardCardProps) -> (String, Element, Option<ResourceId>, Event) {
    let BoardCardProps {
        key,
        item,
        title,
        selected,
        menu,
        body,
        activate,
        drop,
    } = props;
    let widget = Card::from_props(
        &key,
        CardProps {
            title,
            count: None,
            item: item.clone(),
            selected,
            body,
            activate,
        },
    );
    let widget: Element = if menu.is_empty() {
        Box::new(widget)
    } else {
        let commands: Vec<Command> = menu.into_iter().map(Command::of).collect();
        Box::new(ContextMenuArea::new(Box::new(widget), move |_| {
            commands.iter().map(Command::entry).collect()
        }))
    };
    (key, widget, item, drop)
}

impl Described for Board {
    type Props = BoardProps;

    fn from_props(key: &str, props: BoardProps) -> Self {
        let BoardProps { name, lanes } = props;
        let mut board = Self::new().with_id(key.to_owned());
        board.name = name;
        // What each card stands for and what lands before it, and each lane's drop and its
        // cards in order, by key.
        let mut items: HashMap<String, ResourceId> = HashMap::new();
        let mut before: HashMap<String, Event> = HashMap::new();
        let mut lane_drops: HashMap<String, (Event, Vec<String>)> = HashMap::new();
        for lane in lanes {
            let BoardLaneProps {
                key: lane_key,
                item: _,
                title,
                count,
                action,
                cards,
                act,
                drop,
            } = lane;
            let heading = SectionHeader::group(title.unwrap_or_default());
            let heading = match count {
                Some(count) => heading.count(usize::try_from(count).unwrap_or(0)),
                None => heading,
            };
            let heading = match action {
                Some(label) => {
                    let button = crate::widget::Button::new(&format!("{lane_key}-act"), &label)
                        .with_icon(Icon::Plus);
                    heading.action(match act {
                        Event::None => button,
                        Event::Action(binding) => button.on_action(binding),
                        Event::View(intent) => button.on_view(intent.sender()),
                    })
                }
                None => heading,
            };
            let mut widgets = Vec::with_capacity(cards.len());
            let mut order = Vec::with_capacity(cards.len());
            for props in cards {
                let (card_key, widget, item, drop) = card(props);
                if let Some(item) = item {
                    items.insert(card_key.clone(), item);
                }
                if !matches!(drop, Event::None) {
                    before.insert(card_key.clone(), drop);
                }
                order.push(card_key.clone());
                widgets.push((card_key, widget));
            }
            lane_drops.insert(lane_key.clone(), (drop, order));
            board = board.lane(lane_key, heading.into_element(), widgets);
        }
        board.on_drop(
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
                // Its place among the lane's other cards: before `next`, or after them all.
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
            title: Some(key.to_uppercase()),
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
        crate::headless::FrameHarness::for_root(
            crate::State::new(),
            Board::from_props("board", props(queue)),
            (900, 600),
            Theme::dark(),
        )
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
        // Within its lane: b above a is the first place.
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
