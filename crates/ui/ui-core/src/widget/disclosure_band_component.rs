//! [`DisclosureBand`] as a described component.

use crate::widget::arrange::{Arranged, FlexLayout, Part, Stack};
use tilo_ui_base::tokens::Rhythm;
use tilo_ui_base::widget::Element;
use tilo_ui_base::{component::Component, component::Described, component::PartViews};

use super::DisclosureBand;

/// Content a view shows only while something is open, unrolling from its top.
#[derive(Component, Default)]
#[component(name = "DisclosureBand", role = "Group")]
pub struct DisclosureBandProps {
    /// What it shows, in order.
    #[component(part(default))]
    pub content: Vec<Element>,
}

impl Described for DisclosureBand {
    type Props = DisclosureBandProps;

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        if part != "content" {
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
            |column, view| column.push_part(Part::new(view)),
        );
        Self::new(key, Box::new(column))
    }

    fn set_props(&mut self, _props: DisclosureBandProps) -> bool {
        true
    }
}
