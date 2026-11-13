//! [`ControlRow`] as a described component.

use crate::component::{Component, Described, Event};
use crate::widget::{ControlRow, Element, Row};

/// A selectable row of a short list whose items carry their own controls.
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

    fn set_props(&mut self, props: ControlRowProps) -> bool {
        if props.choose.is_none() == self.has_choose() {
            return false;
        }
        self.set_selected(props.selected);
        self.set_name(props.label);
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn crate::component::SlotViews> {
        if slot != "children" {
            return None;
        }
        Some(self.content_mut())
    }
}
