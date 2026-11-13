//! [`DisclosureBand`] as a described component.

use crate::component::{Component, Described, SlotViews};
use crate::tokens::Rhythm;
use crate::widget::Element;
use crate::widget::arrange::{Arranged, FlexLayout, Slot, Stack};

use super::DisclosureBand;

/// Content a view shows only while something is open, unrolling from its top.
#[derive(Component, Default)]
#[component(name = "DisclosureBand", role = "Group")]
pub struct DisclosureBandProps {
    /// What it shows, in order.
    #[component(slot(default))]
    pub content: Vec<Element>,
}

impl Described for DisclosureBand {
    type Props = DisclosureBandProps;

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

    fn set_props(&mut self, _props: DisclosureBandProps) -> bool {
        true
    }
}
