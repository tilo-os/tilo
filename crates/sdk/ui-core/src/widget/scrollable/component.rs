//! [`Scrollable`] as a described component: a column's body that scrolls.

use crate::component::{Component, Described, SlotViews};
use crate::widget::Element;

use super::Scrollable;

/// A body that scrolls under its column's fixed header: its content as tall as it is, the
/// part of it the column has room for shown.
#[derive(Component, Default)]
#[component(name = "Scroll", role = "ScrollView")]
pub struct ScrollProps {
    /// Open at the end of the content and stay there while it grows, unless the person
    /// scrolled away from the end: a conversation's transcript, whose newest line is last.
    pub follow_end: bool,
    /// What scrolls.
    #[component(slot(default))]
    pub content: Option<Element>,
}

impl Described for Scrollable {
    type Props = ScrollProps;

    fn from_props(_key: &str, props: ScrollProps) -> Self {
        let ScrollProps {
            follow_end,
            content,
        } = props;
        let mut scroll = Self::new_vertical();
        if follow_end {
            scroll = scroll.following_end();
        }
        match content {
            Some(content) => scroll.with_child(content),
            None => scroll,
        }
    }

    /// The same scroll stays, and keeps its position, while its content changes in place.
    fn set_props(&mut self, props: ScrollProps) -> bool {
        props.follow_end == self.follow_end
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "content").then_some(self as &mut dyn SlotViews)
    }
}

/// The content of the scroll: one view, or none.
impl SlotViews for Scrollable {
    fn len(&self) -> usize {
        usize::from(self.child.is_some())
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index == 0 { self.child_mut() } else { None }
    }

    fn insert(&mut self, _index: usize, view: Element) {
        let _ = self.replace_child(view);
    }

    fn remove(&mut self, _index: usize) {
        let _ = self.replace_child(Box::new(crate::widget::Spacer::token(
            crate::tokens::SpacingStep::S0,
        )));
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        let _ = self.replace_child(view);
    }
}
