//! Content above a bar docked at the foot of its column, as a described component.

use crate::component::{Component, Described, SlotViews};
use crate::widget::arrange::{Fit, Length, MEASURE_REM, Page, Slot, Stack};
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
    /// The bar stands in the page column the content is read at: the readable measure, its
    /// left edge and its width, so a reply answers the messages above it in their own column
    /// rather than across a wide window.
    pub page_column: bool,
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

/// The width the page column inside `node` is read at, if it holds one: a page bounds what it
/// holds to a readable measure, and a bar at the foot of that column follows the same column.
fn page_measure(node: &dyn Widget) -> Option<Length> {
    if let Some(page) = node.as_any().and_then(|any| any.downcast_ref::<Page>())
        && let Some(measure) = page.readable_measure()
    {
        return Some(measure);
    }
    node.children().into_iter().find_map(page_measure)
}

impl Described for Docked {
    type Props = DockedProps;

    fn from_props(_key: &str, props: DockedProps) -> Self {
        let DockedProps {
            content,
            foot,
            page_column,
        } = props;
        let empty =
            || Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)) as Element;
        let content = content.unwrap_or_else(empty);
        // The bar stands in the column the content is read at: a page bounds what it holds to
        // the readable measure, and the bar under it says what follows those words in their own
        // column rather than across the window. The app may state it (`page_column`) where its
        // column is a page column its content does not say of itself.
        let measure =
            page_measure(content.as_ref()).or(page_column.then_some(Length::Rem(MEASURE_REM)));
        let page = Page::full().foot();
        let page = match measure {
            Some(measure) => page.measure(measure),
            None => page,
        };
        let stack = Stack::stack()
            .fill_width()
            .fill_height()
            .push_slot(slot(content, Fit::fill(1.0)))
            .push_slot(slot(
                Box::new(page.push_slot(Slot::new(foot.unwrap_or_else(empty)))),
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

    /// A column adds no structure of its own: what changes is what it holds, and a
    /// change it can carry out in place (a list mounting the rows a scroll brought
    /// near) stays described, so the window keeps its tree.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.stack.take_structure_change()
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

#[cfg(test)]
#[path = "docked_tests.rs"]
mod tests;
