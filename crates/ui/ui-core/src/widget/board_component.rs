//! [`Board`] as a described component: lists of cards, a card moved by dragging it (a press and a drag
//! with a pointer, a hold and a drag on a touchscreen).

use std::collections::HashMap;

use crate::{
    widget::Card, widget::CardBody, widget::CardProps, widget::ContextMenuArea, widget::MenuEntry,
    widget::MenuItemProps, widget::SectionHeader,
};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::PartItem,
    component::ResourceId, component::Source, component::Value,
};
use tilo_ui_base::{
    widget::Element, widget::Press, widget::Widget, widget::capability::capability_mut,
};

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
impl PartItem for BoardListProps {
    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::Component(BoardListProps::NAME.into())
    }

    fn read_all(source: &mut dyn Source, part: &str) -> Result<Vec<Self>, String> {
        read_all(source, part, BoardListProps::from_source)
    }
}

/// A list's cards: [`BoardCardProps`] only.
impl PartItem for BoardCardProps {
    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::Component(BoardCardProps::NAME.into())
    }

    fn read_all(source: &mut dyn Source, part: &str) -> Result<Vec<Self>, String> {
        read_all(source, part, BoardCardProps::from_source)
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
    fn of(mut props: MenuItemProps) -> Self {
        let press = props.press();
        let MenuItemProps {
            label,
            icon,
            destructive,
            checked,
            disabled,
            description,
            group,
            key,
            // What choosing it does is the press, taken above.
            link: _,
            choose: _,
            value: _,
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
            press,
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
    use crate::widget::Text;

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
                Event::Action(crate::Bind::bound(bound)),
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
                            activate: Event::Action(crate::Bind::bound(bound)),
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
}
