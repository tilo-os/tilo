//! Content above a bar docked at the foot of its column, as a described component.

use crate::component::{Component, Described, SlotViews};
use crate::widget::arrange::{Fit, Page, Slot, Stack};
use crate::widget::{Decorator, Element, Widget};

/// Content that fills its column above a bar that stays at the column's foot: a
/// conversation above its composer. The bar stands in the column's page, a section's gap under
/// the content and the page's gutter at its sides and below.
#[derive(Component, Default)]
#[component(name = "Docked", role = "Group")]
pub struct DockedProps {
    /// What fills the column; it scrolls itself.
    #[component(slot(default))]
    pub content: Option<Element>,
    /// The bar at the foot, as tall as it needs.
    #[component(slot)]
    pub foot: Option<Element>,
}

/// [`DockedProps`] built: a stack filling its column, the content taking the room the foot
/// leaves.
pub struct Docked {
    stack: Stack,
    /// The slot a host is changing: its place in the stack.
    editing: usize,
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

impl Described for Docked {
    type Props = DockedProps;

    fn from_props(_key: &str, props: DockedProps) -> Self {
        let DockedProps { content, foot } = props;
        let empty =
            || Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)) as Element;
        let stack = Stack::stack()
            .fill_width()
            .fill_height()
            .push_slot(slot(content.unwrap_or_else(empty), Fit::fill(1.0)))
            .push_slot(slot(
                Box::new(
                    Page::full()
                        .foot()
                        .push_slot(Slot::new(foot.unwrap_or_else(empty))),
                ),
                Fit::hug(),
            ));
        Self { stack, editing: 0 }
    }

    /// It takes nothing but its slots, which change in place.
    fn set_props(&mut self, _props: DockedProps) -> bool {
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        self.editing = match slot {
            "content" => 0,
            "foot" => 1,
            _ => return None,
        };
        Some(self)
    }
}

/// The view of the slot being changed: the content, or the foot.
impl SlotViews for Docked {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        if self.editing == 0 {
            self.stack.child_mut(0)
        } else {
            self.foot_page()?.child_mut(0)
        }
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put(Box::new(crate::widget::Spacer::token(
            crate::tokens::SpacingStep::S0,
        )));
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

impl Decorator for Docked {
    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.stack
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.stack
    }

    fn name(&self) -> &'static str {
        "Docked"
    }
}

impl Docked {
    /// The page the foot stands in.
    fn foot_page(&mut self) -> Option<&mut Page> {
        self.stack
            .child_mut(1)?
            .as_any_mut()?
            .downcast_mut::<Page>()
    }

    /// Put `view` in the slot being changed.
    fn put(&mut self, view: Element) {
        if self.editing == 0 {
            let _ = self.stack.replace(0, view);
        } else if let Some(page) = self.foot_page() {
            let _ = page.replace(0, view);
        }
    }

    /// The content, to read it.
    #[must_use]
    pub fn content(&self) -> Option<&dyn Widget> {
        self.stack.children().into_iter().next()
    }
}
