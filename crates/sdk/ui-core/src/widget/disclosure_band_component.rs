//! [`DisclosureBand`] as a described component.

use crate::component::{Component, Described, SlotViews};
use crate::tokens::Rhythm;
use crate::widget::Element;
use crate::widget::arrange::{Arranged, FlexLayout, Slot, Stack};

use super::DisclosureBand;

/// Content a view shows only while something is open (a tile's details under it, a row's
/// section), standing in a column. When the view starts showing it, it unrolls from its top
/// while what follows moves down with its edge; when the view stops, it rolls up. Under reduced
/// motion it lands at once.
#[derive(Component, Default)]
#[component(name = "DisclosureBand", role = "Group")]
pub struct DisclosureBandProps {
    /// What it shows, in order.
    #[component(slot(default))]
    pub content: Vec<Element>,
}

impl Described for DisclosureBand {
    type Props = DisclosureBandProps;

    /// Its content is a column whose views change by key.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        if slot != "content" {
            return None;
        }
        let column = self
            .child
            .as_any_mut()?
            .downcast_mut::<Arranged<FlexLayout>>()?;
        Some(column)
    }

    /// Its key is its identity across rebuilds: built again with the same key it goes on from
    /// where it was, and one with another key arrives as the other leaves.
    fn from_props(key: &str, props: DisclosureBandProps) -> Self {
        let column = props.content.into_iter().fold(
            Stack::stack()
                .fill_width()
                .gap(Rhythm::Related)
                .with_id(format!("{key}.content")),
            |column, view| column.push_slot(Slot::new(view)),
        );
        Self::new(key, Box::new(column))
    }

    /// It has no props of its own: what it shows changes through [`Self::slot_views`].
    fn set_props(&mut self, _props: DisclosureBandProps) -> bool {
        true
    }
}
