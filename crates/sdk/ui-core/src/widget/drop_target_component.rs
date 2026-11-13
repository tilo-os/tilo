//! [`DropTarget`](super::DropTarget) as a described component.

use crate::component::{Component, Described, Event, ResourceId, SlotViews};
use crate::widget::Element;

use super::DropTarget;

/// A view that takes what another view dragged to it: a chat composer takes an attachment, a
/// dock takes a resource. A drag over it draws the toolkit's drop line, and letting go inside
/// it reports what the drag carried.
///
/// It takes a resource dragged out of any [`ui.DragSource`](super::DragSource) and a run's item
/// ([`ui.DragItem`](super::DragItem)); nothing else, so a file dragged in from another
/// application is not silently swallowed (`ui.DropZone` is the component for files).
#[derive(Component, Default)]
#[component(name = "DropTarget", role = "Group")]
pub struct DropTargetProps {
    /// What it draws.
    #[component(slot(default))]
    pub content: Option<Element>,
    /// A resource was dropped on it: carries the resource the drag carried, as its address.
    /// The view decides what to do with it — a composer attaches it to the draft, a canvas
    /// places it where it was dropped.
    #[component(event(value = ResourceId))]
    pub drop: Event,
}

impl Described for DropTarget {
    type Props = DropTargetProps;

    fn from_props(key: &str, props: DropTargetProps) -> Self {
        let DropTargetProps { content, drop } = props;
        let empty =
            || Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)) as Element;
        Self::new(key, content.unwrap_or_else(empty), drop)
    }

    fn set_props(&mut self, props: DropTargetProps) -> bool {
        let DropTargetProps { content, drop } = props;
        self.set_content(content, drop);
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "content").then_some(self as &mut dyn SlotViews)
    }
}
