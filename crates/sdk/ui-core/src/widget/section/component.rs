//! [`Section`] as a described component.

use crate::component::{Component, Described, Event, SlotViews, Value};
use crate::widget::Element;

use super::Section;

/// A group of related content under an optional title: rows of a list, fields, a figure. A
/// collapsible section opens and closes under its title.
#[derive(Component, Default)]
#[component(name = "Section", role = "Group", label = "title")]
pub struct SectionProps {
    /// What the group is about.
    #[component(positional)]
    pub title: Option<String>,
    /// Whether its title opens and closes it.
    pub collapsible: bool,
    /// What it holds, in order.
    #[component(slot(default))]
    pub content: Vec<Element>,
    /// Whether a collapsible section shows what it holds.
    #[component(state)]
    pub expanded: Option<bool>,
    /// A collapsible section opened or closed; carries whether it is open now.
    #[component(event(value = bool))]
    pub toggle: Event,
}

impl Described for Section {
    type Props = SectionProps;

    /// What it holds changes by key, under its header.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "content").then_some(self as &mut dyn SlotViews)
    }

    fn from_props(_key: &str, props: SectionProps) -> Self {
        let SectionProps {
            title,
            collapsible,
            content,
            expanded,
            toggle,
        } = props;
        let section = match title {
            Some(title) if !title.is_empty() => Self::titled(title, content),
            _ => Self::new(content),
        };
        if !collapsible {
            return section;
        }
        let on_toggle: Option<Box<dyn FnMut(bool)>> = if toggle.is_none() {
            None
        } else {
            Some(Box::new(move |open| toggle.fire(Some(Value::Bool(open)))))
        };
        section.collapsible_with(expanded.unwrap_or(true), on_toggle)
    }
}

/// The items under a section's header.
impl SlotViews for Section {
    fn len(&self) -> usize {
        self.parts
            .children
            .len()
            .saturating_sub(usize::from(self.has_header))
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        let at = index.saturating_add(usize::from(self.has_header));
        self.parts.children.get_mut(at)
    }

    fn insert(&mut self, index: usize, view: Element) {
        let at = index
            .saturating_add(usize::from(self.has_header))
            .min(self.parts.children.len());
        self.parts.children.insert(at, view);
        self.described();
    }

    fn remove(&mut self, index: usize) {
        let at = index.saturating_add(usize::from(self.has_header));
        if at < self.parts.children.len() {
            self.parts.children.remove(at);
            self.described();
        }
    }

    fn move_view(&mut self, from: usize, to: usize) {
        let offset = usize::from(self.has_header);
        let (from, to) = (from.saturating_add(offset), to.saturating_add(offset));
        let len = self.parts.children.len();
        if from < len && to < len && from != to {
            let view = self.parts.children.remove(from);
            self.parts.children.insert(to, view);
            self.described();
        }
    }

    fn replace(&mut self, index: usize, view: Element) {
        if let Some(held) = self.view_mut(index) {
            *held = view;
            self.described();
        }
    }
}

impl Section {
    /// Its items changed in place: the window lays it out again and repaints what moved.
    fn described(&mut self) {
        self.structure = self
            .structure
            .join(crate::widget::StructureChange::Described);
    }
}
