//! [`Scrollable`] as a described component: a column's body that scrolls.

use crate::component::{Component, Described, SlotViews};
use crate::widget::Element;

use super::Scrollable;

/// A body that scrolls under its column's fixed header, showing the part of the content the
/// column has room for.
#[derive(Component, Default)]
#[component(name = "Scroll", role = "ScrollView")]
pub struct ScrollProps {
    /// Open at the end of the content and stay there while it grows, unless the person scrolled
    /// away: a transcript, whose newest line is last.
    pub follow_end: bool,
    /// Scroll sideways instead: a row wider than its column, such as a board's lanes.
    pub across: bool,
    /// Scroll both ways, for a 2D surface wider and taller than its column (a board of strips);
    /// takes precedence over `across`.
    pub both: bool,
    /// What scrolls.
    #[component(slot(default))]
    pub content: Option<Element>,
}

impl Described for Scrollable {
    type Props = ScrollProps;

    fn from_props(_key: &str, props: ScrollProps) -> Self {
        let ScrollProps {
            follow_end,
            across,
            both,
            content,
        } = props;
        let mut scroll = if both {
            Self::new(super::ScrollAxis::Both)
        } else if across {
            Self::new_horizontal().with_content_min_viewport_width()
        } else {
            Self::new_vertical()
        };
        if follow_end {
            scroll = scroll.following_end();
        }
        match content {
            Some(content) => scroll.with_child(content),
            None => scroll,
        }
    }

    fn set_props(&mut self, props: ScrollProps) -> bool {
        let same_axis = if props.both {
            matches!(self.axis, super::ScrollAxis::Both)
        } else {
            matches!(
                (props.across, self.axis),
                (true, super::ScrollAxis::Horizontal) | (false, super::ScrollAxis::Vertical)
            )
        };
        props.follow_end == self.follow_end && same_axis
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "content").then_some(self as &mut dyn SlotViews)
    }
}

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
