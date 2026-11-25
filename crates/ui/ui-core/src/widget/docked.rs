//! Content above a bar docked at the foot of its column, as a described component.

use crate::component::{Component, Described, PartViews};
use crate::widget::Length;
use crate::widget::arrange::{Fit, Page, Part, Stack};
use crate::widget::{Decorator, Element, Widget};

/// Content that fills its column above a bar that stays at the column's foot: a
/// conversation above its composer.
#[derive(Component, Default)]
#[component(name = "Docked", role = "Group")]
pub struct DockedProps {
    /// What fills the column; it scrolls itself.
    #[component(part(default))]
    pub content: Option<Element>,
    /// The bar at the foot, as tall as it needs.
    #[component(part)]
    pub foot: Option<Element>,
}

/// [`DockedProps`] built: a stack filling its column, the content taking the height the foot
/// leaves.
pub struct Docked {
    stack: Stack,
    /// The part a host is changing: its place in the stack.
    editing: usize,
}

fn part(element: Element, fit: Fit) -> Part {
    let mut part = Part::new(element);
    part.fit = fit;
    part
}

/// The readable column `node` holds its content in, at any depth: a page bounds what it
/// holds to a measure, and a list that reads its rows at one keeps it too.
fn column_of(node: &dyn Widget) -> Option<Length> {
    node.readable_column()
        .or_else(|| node.children().into_iter().find_map(column_of))
}

impl Described for Docked {
    type Props = DockedProps;

    fn from_props(_key: &str, props: DockedProps) -> Self {
        let DockedProps { content, foot } = props;
        let empty =
            || Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)) as Element;
        let content = content.unwrap_or_else(empty);
        // The bar stands in the column the content is read at: what follows those words in their
        // own column rather than across the window.
        let measure = column_of(content.as_ref());
        let mut stack = Stack::stack()
            .fill_width()
            .fill_height()
            .push_part(part(content, Fit::fill(1.0)));
        // A dock with nothing at its foot draws no bar: an empty page would paint its band over
        // the content for no words at all.
        if let Some(foot) = foot {
            let page = Page::full().foot();
            let page = match measure {
                Some(measure) => page.measure(measure),
                None => page,
            };
            stack = stack.push_part(part(Box::new(page.push_part(Part::new(foot))), Fit::hug()));
        }
        Self { stack, editing: 0 }
    }

    /// It takes nothing but its parts, which change in place.
    fn set_props(&mut self, _props: DockedProps) -> bool {
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        self.editing = match part {
            "content" => 0,
            "foot" => 1,
            _ => return None,
        };
        Some(self)
    }
}

/// The view of the part being changed: the content, or the foot.
impl PartViews for Docked {
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
        if self.editing == 0 {
            self.put(Box::new(crate::widget::Spacer::token(
                crate::tokens::SpacingStep::S0,
            )));
        } else {
            // Nothing stands at the foot: the bar goes with what it held.
            self.stack.remove(1);
        }
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

    /// A column adds no structure of its own: what changes is what it holds, and a change
    /// it can carry out in place (a list mounting the rows a scroll brought near) stays
    /// described, so the window keeps its tree.
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

    /// Put `view` in the part being changed.
    fn put(&mut self, view: Element) {
        if self.editing == 0 {
            let _ = self.stack.replace(0, view);
            // The content is what says the column: a bar that adds to it stands where it is read,
            // whether the content came with the column or the host put it in after it was built.
            let measure = self.column();
            if let Some(page) = self.foot_page() {
                page.set_readable_measure(measure);
            }
        } else if let Some(page) = self.foot_page() {
            let _ = page.replace(0, view);
        } else {
            // The dock stood with nothing at its foot and is given something: the bar stands at
            // the column's foot, where the content is read.
            let measure = self.column();
            let page = Page::full().foot();
            let page = match measure {
                Some(measure) => page.measure(measure),
                None => page,
            };
            let bar = part(Box::new(page.push_part(Part::new(view))), Fit::hug());
            let stack = std::mem::replace(&mut self.stack, Stack::stack());
            self.stack = stack.push_part(bar);
        }
    }

    /// The content, to read it.
    #[must_use]
    pub fn content(&self) -> Option<&dyn Widget> {
        self.stack.children().into_iter().next()
    }

    /// The column the content is read in, as it stands now.
    #[must_use]
    fn column(&self) -> Option<Length> {
        self.content().and_then(column_of)
    }
}

#[cfg(test)]
#[path = "docked_tests.rs"]
mod tests;
