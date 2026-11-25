//! [`Card`] as a described component.

use crate::component::{Component, Described, Event, PartViews, ResourceId};
use crate::tokens::Rhythm;
use crate::widget::{Element, SectionHeader};

use super::{Activation, Card};

/// A summary of one thing on a surface of its own: a dashboard's tile, a resource's preview.
#[derive(Component, Default)]
#[component(name = "Card", role = "Group", label = "title", presents = "item")]
pub struct CardProps {
    /// What the card stands for, named in its head.
    pub title: Option<String>,
    /// What assistive technology calls the card, when its title alone does not say it: the title
    /// unless given.
    pub accessible_name: Option<String>,
    /// How many things the card lists, as a count beside its title.
    pub count: Option<i64>,
    /// The resource the card summarizes.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// Whether it is the selected one among its peers.
    pub selected: bool,
    /// What the card shows, in order.
    #[component(part(default))]
    pub body: Vec<Element>,
    /// The card was pressed: open what it stands for.
    #[component(event(press))]
    pub activate: Event,
    /// The pointer came over the card: it is the one under it now.
    #[component(event)]
    pub hover: Event,
}

impl Described for Card {
    type Props = CardProps;

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        if part != "body" {
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
            accessible_name,
            count,
            item: _,
            selected,
            body,
            activate,
            hover,
        } = props;
        let column = body.into_iter().fold(
            crate::widget::arrange::Stack::stack()
                .fill_width()
                .gap(Rhythm::Related)
                .with_id(key.to_owned()),
            |column, part| column.push_part(crate::widget::arrange::Part::new(part)),
        );
        let mut card = Self::new(Box::new(column)).selected(selected);
        if let Some(name) = accessible_name.filter(|name| !name.is_empty()) {
            card = card.with_accessible_name(name);
        }
        if !activate.is_none() {
            card.activation = Activation::new(key);
            card.activation.set_event(activate);
        }
        card.hover = hover;
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

    fn set_props(&mut self, props: CardProps) -> bool {
        let CardProps {
            title,
            accessible_name,
            count,
            item: _,
            selected,
            body: _,
            activate,
            hover,
        } = props;
        self.accessible_name = accessible_name.filter(|name| !name.is_empty());
        self.hover = hover;
        self.set_selected(selected);
        let title = title.filter(|title| !title.is_empty());
        let count = count.map(|count| usize::try_from(count.max(0)).unwrap_or(0));
        if title != self.title || count != self.header_count {
            if let Some(title) = &title {
                self.header_count = count;
                let header = SectionHeader::group(title.as_str());
                let header = match count {
                    Some(count) => header.count(count),
                    None => header,
                };
                self.set_header(header);
            } else {
                self.header_count = None;
                self.remove_header();
            }
        }
        self.activation.set_event(activate);
        true
    }
}
