//! [`Card`] as a described component.

use crate::component::{Component, Described, Event, ResourceId, SlotViews};
use crate::tokens::Rhythm;
use crate::widget::{Element, SectionHeader};

use super::{Activation, Card};

/// A summary of one thing on a surface of its own: a dashboard's tile, a resource's preview.
/// Pressing it opens what it stands for.
#[derive(Component, Default)]
#[component(name = "Card", role = "Group", label = "title", presents = "item")]
pub struct CardProps {
    /// What the card stands for, named in its head.
    pub title: Option<String>,
    /// How many things the card lists, as a count beside its title.
    pub count: Option<i64>,
    /// The resource the card summarizes.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// Whether it is the selected one among its peers.
    pub selected: bool,
    /// What the card shows, in order.
    #[component(slot(default))]
    pub body: Vec<Element>,
    /// The card was pressed: open what it stands for.
    #[component(event)]
    pub activate: Event,
}

impl Described for Card {
    type Props = CardProps;

    /// The body is a column whose views change by key.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        if slot != "body" {
            return None;
        }
        let at = usize::from(self.has_header && !self.media_first);
        let column = self
            .parts
            .children
            .get_mut(at)?
            .as_any_mut()?
            .downcast_mut::<crate::widget::arrange::Arranged<crate::widget::arrange::FlexLayout>>(
        )?;
        Some(column)
    }

    fn from_props(key: &str, props: CardProps) -> Self {
        let CardProps {
            title,
            count,
            item: _,
            selected,
            body,
            activate,
        } = props;
        let column = body.into_iter().fold(
            crate::widget::arrange::Stack::stack()
                .fill_width()
                .gap(Rhythm::Related)
                .with_id(key.to_owned()),
            |column, part| column.push_slot(crate::widget::arrange::Slot::new(part)),
        );
        let mut card = Self::new(Box::new(column)).selected(selected);
        if !activate.is_none() {
            card.activation = Activation::new(key);
            let label = title.clone().unwrap_or_default();
            card.activation.set_event(activate, &label);
        }
        match title {
            Some(title) if !title.is_empty() => {
                let header = SectionHeader::group(title);
                let header = match count {
                    Some(count) => header.count(usize::try_from(count).unwrap_or(0)),
                    None => header,
                };
                card.with_section_header(header)
            }
            _ => card,
        }
    }
}
