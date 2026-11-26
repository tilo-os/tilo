//! [`CardGrid`] as a described component: cards as the parts a grid holds, each a card of a resource with what
//! it shows, and pressing one reported with the resource it stands for.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_ui_base::{component::Component, component::Described, component::Event, component::ResourceId, component::Value};
use tilo_ui_base::widget::Widget;
use crate::{widget::Card, widget::CardGrid, widget::CardWidth};
use tilo_ui_base::widget::Element;

use super::Activation;

/// One card of a grid: a summary of the resource it stands for, which a press opens.
#[derive(Component, Default)]
#[component(
    name = "CardGridCard",
    role = "ListItem",
    label = "title",
    presents = "item"
)]
pub struct CardGridCardProps {
    /// The resource the card stands for: its stable key, and what pressing it reports.
    #[component(required, positional)]
    pub item: ResourceId,
    /// What the card is called, in its head; without one the body speaks for it.
    pub title: Option<String>,
    /// How many things the card lists, as a count beside its title.
    pub count: Option<i64>,
    /// Whether it is the selected one among its peers.
    pub selected: bool,
    /// What the card shows, in order.
    #[component(part(default))]
    pub body: Vec<Element>,
    /// The card was pressed: open what it stands for.
    #[component(event)]
    pub activate: Event,
}

tilo_ui_base::component::part_type!(CardGridCardProps);

/// Cards in equal, aligned columns: the apps of a store, the devices of a network, the paragraphs of a
/// review.
#[derive(Component, Default)]
#[component(name = "CardGrid", role = "List", label = "name")]
pub struct CardGridProps {
    /// What the grid is, for assistive technology.
    pub name: Option<String>,
    /// How wide a card is at least before the grid drops a column: small, regular or wide objects.
    pub card_width: Option<CardWidth>,
    /// The cards, in order.
    #[component(part(default))]
    pub cards: Vec<CardGridCardProps>,
    /// The keyboard moved the selection to a card: carries the item it stands for.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// The edit key was pressed on the selected card: carries the item it stands for.
    #[component(event(value = ResourceId))]
    pub edit: Event,
    /// Delete was pressed on the selected card: carries the item it stands for, for the app's
    /// destructive action.
    #[component(event(value = ResourceId))]
    pub delete: Event,
}

tilo_ui_base::choice!(CardWidth, "CardWidth", {
    Compact => "compact",
    Regular => "regular",
    Wide => "wide",
});

fn card_of(props: CardGridCardProps) -> (String, Element) {
    let CardGridCardProps {
        item,
        title,
        count,
        selected,
        body,
        activate,
    } = props;
    let key = item.0.clone();
    let column = body.into_iter().fold(
        crate::widget::arrange::Stack::stack()
            .fill_width()
            .gap(tilo_ui_base::tokens::Rhythm::Related)
            .with_id(format!("{key}.body")),
        |column, part| column.push_part(crate::widget::arrange::Part::new(part)),
    );
    let mut card = Card::new(Box::new(column)).selected(selected);
    if !activate.is_none() {
        card.activation = Activation::new(key.clone());
        card.activation.set_event(activate);
    }
    let card = match title {
        Some(title) if !title.is_empty() => {
            let header = crate::widget::SectionHeader::group(title);
            let header = match count {
                Some(count) => header.count(usize::try_from(count).unwrap_or(0)),
                None => header,
            };
            card.with_section_header(header)
        }
        _ => card,
    };
    (key, Box::new(card))
}

fn say(props: CardGridCardProps, card: &mut crate::widget::Card) {
    card.set_selected(props.selected);
    let title = props.title.filter(|title| !title.is_empty());
    match title {
        Some(title) => {
            let header = crate::widget::SectionHeader::group(title);
            let header = match props.count {
                Some(count) => header.count(usize::try_from(count).unwrap_or(0)),
                None => header,
            };
            card.set_header(header);
        }
        None => card.remove_header(),
    }
}

/// A described card grid: its grid, and the keys of the cards it shows, in order.
pub struct DescribedCardGrid {
    grid: CardGrid,
    /// The key each card shown stands for, in order: the grid is changed by them, and reports
    /// the one its keyboard acts on.
    shown: Rc<RefCell<Vec<String>>>,
    face: Face,
}

#[derive(Clone, Debug, PartialEq)]
struct Face {
    name: Option<String>,
    card_width: Option<CardWidth>,
}

impl DescribedCardGrid {
    #[must_use]
    pub fn grid(&self) -> &CardGrid {
        &self.grid
    }

    pub fn grid_mut(&mut self) -> &mut CardGrid {
        &mut self.grid
    }

    /// The keys of the cards it shows, in order.
    #[must_use]
    pub fn shown(&self) -> std::cell::Ref<'_, Vec<String>> {
        self.shown.borrow()
    }
}

/// Have `grid` report what its keyboard did with the card at an index as the item it shows.
fn wire(
    grid: &mut CardGrid,
    shown: &Rc<RefCell<Vec<String>>>,
    select: Event,
    edit: Event,
    delete: Event,
) {
    if !select.is_none() {
        let keys = Rc::clone(shown);
        grid.set_on_select(move |index| {
            if let Some(item) = keys.borrow().get(index) {
                select.fire(Some(Value::Resource(item.clone())));
            }
        });
    }
    if !edit.is_none() {
        let keys = Rc::clone(shown);
        grid.set_on_edit(move |index| {
            if let Some(item) = keys.borrow().get(index) {
                edit.fire(Some(Value::Resource(item.clone())));
            }
        });
    }
    if !delete.is_none() {
        let keys = Rc::clone(shown);
        grid.set_on_delete(move |index| {
            if let Some(item) = keys.borrow().get(index) {
                delete.fire(Some(Value::Resource(item.clone())));
            }
        });
    }
}

impl Described for DescribedCardGrid {
    type Props = CardGridProps;

    fn from_props(_key: &str, props: CardGridProps) -> Self {
        let CardGridProps {
            name,
            card_width,
            cards,
            select,
            edit,
            delete,
        } = props;
        let shown: Vec<String> = cards.iter().map(|card| card.item.0.clone()).collect();
        let widgets: Vec<Element> = cards.into_iter().map(|card| card_of(card).1).collect();
        let mut grid = CardGrid::new(widgets).with_card_width(card_width.unwrap_or_default());
        if let Some(name) = &name {
            grid.set_accessible_name(name.clone());
        }
        grid.adopt_selection();
        let shown = Rc::new(RefCell::new(shown));
        wire(&mut grid, &shown, select, edit, delete);
        Self {
            grid,
            shown,
            face: Face { name, card_width },
        }
    }

    fn set_props(&mut self, props: CardGridProps) -> bool {
        let CardGridProps {
            name,
            card_width,
            cards,
            select,
            edit,
            delete,
        } = props;
        let face = Face { name, card_width };
        if face != self.face {
            return false;
        }
        let wanted: Vec<String> = cards.iter().map(|card| card.item.0.clone()).collect();
        let mut shown = self.shown.borrow_mut();
        // Those gone, from the end so the indexes before them stay.
        let mut index = shown.len();
        while index > 0 {
            index -= 1;
            if !wanted.iter().any(|key| key == &shown[index]) {
                self.grid.remove_card(index);
                shown.remove(index);
            }
        }
        for (at, card) in cards.into_iter().enumerate() {
            let key = card.item.0.clone();
            if let Some(had) = shown.iter().position(|held| held == &key) {
                if had != at {
                    self.grid.move_card(had, at);
                    let key = shown.remove(had);
                    shown.insert(at, key);
                }
                if let Some(held) = self.grid.card_mut(at).and_then(|card| {
                    card.as_any_mut()
                        .and_then(|any| any.downcast_mut::<crate::widget::Card>())
                }) {
                    say(card, held);
                }
            } else {
                let (_, widget) = card_of(card);
                self.grid.insert_card(at, widget);
                shown.insert(at, key);
            }
        }
        drop(shown);
        // The card the props select is where the keyboard then stands; a grid with no selection
        // of its own keeps the one the person moved to.
        self.grid.adopt_selection();
        let shown = Rc::clone(&self.shown);
        wire(&mut self.grid, &shown, select, edit, delete);
        true
    }
}

impl tilo_ui_base::widget::Decorator for DescribedCardGrid {
    type Inner = dyn tilo_ui_base::widget::Widget;

    fn inner(&self) -> &(dyn tilo_ui_base::widget::Widget + 'static) {
        &self.grid
    }

    fn inner_mut(&mut self) -> &mut (dyn tilo_ui_base::widget::Widget + 'static) {
        &mut self.grid
    }

    fn name(&self) -> &'static str {
        "CardGrid"
    }

    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        self.grid.take_structure_change()
    }
}
