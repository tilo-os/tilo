//! [`DragSource`](super::DragSource) as a described component.

use crate::component::{Component, Described, Event, SlotViews};
use crate::widget::Element;

use super::DragSource;

/// A view a person can pick up and drag to a drop target in the same window: a search hit, a
/// row of a list, a message's attachment. What it carries is `resource`; the ghost under the
/// pointer says `title`.
///
/// A press that does not travel is the content's own, so a dragged row still opens when clicked
/// and a dragged chip still presses.
#[derive(Component, Default)]
#[component(name = "DragSource", role = "Group", label = "title")]
pub struct DragSourceProps {
    /// The resource the drag carries, as its published address
    /// (`chat.workspace/messages/m1`): what a drop target receives.
    #[component(required, positional, data)]
    pub resource: String,
    /// What the drag's ghost says under the pointer, and what assistive technology reads
    /// instead of the resource's address.
    pub title: Option<String>,
    /// What it draws.
    #[component(slot(default))]
    pub content: Option<Element>,
    /// It was picked up and let go without travelling: the content's own press did not take
    /// it. Reported so a source that wraps something inert still says what happened.
    #[component(event(press))]
    pub activate: Event,
}

impl Described for DragSource {
    type Props = DragSourceProps;

    fn from_props(key: &str, props: DragSourceProps) -> Self {
        let DragSourceProps {
            resource,
            title,
            content,
            ..
        } = props;
        let empty =
            || Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)) as Element;
        Self::new(
            key,
            content.unwrap_or_else(empty),
            resource,
            title.unwrap_or_default(),
        )
    }

    fn set_props(&mut self, props: DragSourceProps) -> bool {
        let DragSourceProps {
            resource,
            title,
            content,
            ..
        } = props;
        self.set_content(content, resource, title.unwrap_or_default());
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "content").then_some(self as &mut dyn SlotViews)
    }
}
