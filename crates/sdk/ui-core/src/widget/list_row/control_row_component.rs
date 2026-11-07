//! [`ControlRow`] as a described component.

use crate::component::{Component, Described, Event};
use crate::widget::{ControlRow, Element, Row};

/// A selectable row of a short list whose items carry their own controls (a layers panel, each
/// layer's name beside its switches): a press none of its controls took chooses the row, and the
/// chosen row is lifted off the ground it stands on.
#[derive(Component, Default)]
#[component(name = "ControlRow", role = "ListItem", label = "label")]
pub struct ControlRowProps {
    /// What the row holds, in order: its thumbnail, its name, and the controls its item has.
    #[component(slot(default))]
    pub children: Vec<Element>,
    /// Its spoken name.
    pub label: Option<String>,
    /// Whether it is the chosen one of its list.
    pub selected: bool,
    /// It was chosen: a press its controls did not take.
    #[component(event)]
    pub choose: Event,
}

impl Described for ControlRow {
    type Props = ControlRowProps;

    fn from_props(key: &str, props: ControlRowProps) -> Self {
        let ControlRowProps {
            children,
            label,
            selected,
            choose,
        } = props;
        let contents = children
            .into_iter()
            .fold(Row::new(), |row, view| row.push(view));
        let mut row = Self::new(contents).with_id(key).with_selected(selected);
        if let Some(label) = label {
            row = row.with_accessible_name(label);
        }
        if let Some(chosen) = choose.handler() {
            row = row.on_choose(chosen);
        }
        row
    }

    /// What it is called and what a press does change where the row stands; its content changes
    /// by key through [`Self::slot_views`]. A row that gains or loses its press is built again.
    fn set_props(&mut self, props: ControlRowProps) -> bool {
        if props.choose.is_none() == self.has_choose() {
            return false;
        }
        self.set_selected(props.selected);
        self.set_name(props.label);
        true
    }

    /// What stands in the row changes by key in place.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn crate::component::SlotViews> {
        if slot != "children" {
            return None;
        }
        Some(self.content_mut())
    }
}
