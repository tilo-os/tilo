//! [`CardGrid`] as a described component: cards as parts in a slot, each a card of a resource with what
//! it shows, and pressing one reported with the resource it stands for.

use crate::component::{Component, Described, Event, ResourceId};
use crate::widget::Widget;
use crate::widget::{Card, CardGrid, CardWidth, Element};

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
    #[component(slot(default))]
    pub body: Vec<Element>,
    /// The card was pressed: open what it stands for.
    #[component(event)]
    pub activate: Event,
}

crate::component::part_slot!(CardGridCardProps);

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
    #[component(slot(default))]
    pub cards: Vec<CardGridCardProps>,
}

crate::component::choice!(CardWidth, "CardWidth", {
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
            .gap(crate::tokens::Rhythm::Related)
            .with_id(format!("{key}.body")),
        |column, part| column.push_slot(crate::widget::arrange::Slot::new(part)),
    );
    let mut card = Card::new(Box::new(column)).selected(selected);
    if !activate.is_none() {
        card.activation = Activation::new(key.clone());
        let label = title.clone().unwrap_or_default();
        card.activation.set_event(activate, &label);
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
    /// The key each card shown stands for, in order: the grid is changed by them.
    shown: Vec<String>,
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
    pub fn shown(&self) -> &[String] {
        &self.shown
    }
}

impl Described for DescribedCardGrid {
    type Props = CardGridProps;

    fn from_props(_key: &str, props: CardGridProps) -> Self {
        let CardGridProps {
            name,
            card_width,
            cards,
        } = props;
        let shown: Vec<String> = cards.iter().map(|card| card.item.0.clone()).collect();
        let widgets: Vec<Element> = cards.into_iter().map(|card| card_of(card).1).collect();
        let mut grid = CardGrid::new(widgets).with_card_width(card_width.unwrap_or_default());
        if let Some(name) = &name {
            grid.set_accessible_name(name.clone());
        }
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
        } = props;
        let face = Face { name, card_width };
        if face != self.face {
            return false;
        }
        let wanted: Vec<String> = cards.iter().map(|card| card.item.0.clone()).collect();
        // Those gone, from the end so the indexes before them stay.
        let mut index = self.shown.len();
        while index > 0 {
            index -= 1;
            if !wanted.iter().any(|key| key == &self.shown[index]) {
                self.grid.remove_card(index);
                self.shown.remove(index);
            }
        }
        for (at, card) in cards.into_iter().enumerate() {
            let key = card.item.0.clone();
            if let Some(had) = self.shown.iter().position(|held| held == &key) {
                if had != at {
                    self.grid.move_card(had, at);
                    let key = self.shown.remove(had);
                    self.shown.insert(at, key);
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
                self.shown.insert(at, key);
            }
        }
        true
    }
}

impl crate::widget::Decorator for DescribedCardGrid {
    type Inner = dyn crate::widget::Widget;

    fn inner(&self) -> &(dyn crate::widget::Widget + 'static) {
        &self.grid
    }

    fn inner_mut(&mut self) -> &mut (dyn crate::widget::Widget + 'static) {
        &mut self.grid
    }

    fn name(&self) -> &'static str {
        "CardGrid"
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.grid.take_structure_change()
    }
}
