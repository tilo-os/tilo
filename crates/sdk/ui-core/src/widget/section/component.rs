//! [`Section`] as a described component.

use std::rc::Rc;

use vello::kurbo::Point;

use crate::component::{Component, Described, Event, SlotViews, Value};
use crate::widget::{Element, Interaction};

use super::Section;

/// A group of related content under an optional title (a list's rows, fields, a figure); a
/// collapsible one opens and closes under its title.
#[derive(Component, Default)]
#[component(name = "Section", role = "Group", label = "title")]
pub struct SectionProps {
    /// What the group is about.
    #[component(positional)]
    pub title: Option<String>,
    /// Whether its title opens and closes it.
    pub collapsible: bool,
    /// The content, in order.
    #[component(slot(default))]
    pub content: Vec<Element>,
    /// Whether a collapsible section shows what it holds.
    #[component(state)]
    pub expanded: Option<bool>,
    /// The pace the content arrives and leaves at, and its title's reveal: the theme's unless
    /// said.
    pub motion: Option<crate::motion::Feel>,
    /// A collapsible section opened or closed: whether it is open now is the value.
    #[component(event(value = bool))]
    pub toggle: Event,
}

impl Described for Section {
    type Props = SectionProps;

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "content").then_some(self as &mut dyn SlotViews)
    }

    fn from_props(_key: &str, props: SectionProps) -> Self {
        let SectionProps {
            title,
            collapsible,
            content,
            expanded,
            motion,
            toggle,
        } = props;
        let mut section = match title {
            Some(title) if !title.is_empty() => Self::titled(title, content),
            _ => Self::new(content),
        };
        section.set_feel(motion.unwrap_or(crate::motion::Feel::Standard));
        if !collapsible {
            return section;
        }
        let on_toggle = reports(toggle);
        section.collapsible_with(expanded.unwrap_or(true), on_toggle)
    }

    /// What the group is called, whether its title opens and closes it, and how fast what it
    /// holds arrives change where the section stands: a section the person closed stays closed.
    fn set_props(&mut self, props: SectionProps) -> bool {
        let SectionProps {
            title,
            collapsible,
            content: _,
            expanded,
            motion,
            toggle,
        } = props;
        self.set_feel(motion.unwrap_or(crate::motion::Feel::Standard));
        let title = title.filter(|title| !title.is_empty());
        if title != self.header {
            self.header.clone_from(&title);
            match &title {
                Some(title) => {
                    self.set_header_row(super::SectionHeader::heading(title.as_str()).into_widget(
                        self.expanded.as_ref().map(|state| super::Disclosure {
                            id: format!("disclosure:{title}"),
                            expanded: Rc::clone(state),
                            on_toggle: None,
                            interaction: Interaction::default(),
                            press: crate::pointer_replay::HeldPress::default(),
                            cursor: Point::ZERO,
                            toggled: false,
                        }),
                    ))
                }
                None => {
                    if self.has_header {
                        self.parts.children.remove(0);
                        self.has_header = false;
                    }
                }
            }
        }
        let on_toggle = reports(toggle);
        if collapsible {
            if self.expanded.is_none() {
                self.make_collapsible(expanded.unwrap_or(true), on_toggle);
            } else {
                if let Some(disclosure) = self.header_disclosure() {
                    disclosure.on_toggle = on_toggle;
                }
                if let Some(expanded) = expanded {
                    self.set_expanded(expanded);
                }
            }
        } else if self.expanded.is_some() {
            self.expanded = None;
            if let Some(row) = self.header_row() {
                row.disclosure = None;
                if row.has_chevron {
                    row.parts.children.pop();
                    row.has_chevron = false;
                }
            }
        }
        true
    }
}

fn reports(toggle: Event) -> Option<Box<dyn FnMut(bool)>> {
    if toggle.is_none() {
        None
    } else {
        Some(Box::new(move |open| toggle.fire(Some(Value::Bool(open)))))
    }
}

impl Section {
    fn header_disclosure(&mut self) -> Option<&mut super::Disclosure> {
        self.header_row()?.disclosure.as_mut()
    }

    fn header_row(&mut self) -> Option<&mut super::HeaderRow> {
        self.parts
            .children
            .first_mut()?
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<super::HeaderRow>())
    }
}

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
    fn described(&mut self) {
        self.structure = self
            .structure
            .join(crate::widget::StructureChange::Described);
    }
}
