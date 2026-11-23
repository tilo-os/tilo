//! `EnumField`: a value among an enum's cases, shown as one fact and picked in its menu.
//!
//! The field never holds the authoritative value: what it shows is the case its binding
//! holds (a described component's field follows the value the host keeps in step), and an
//! edit goes back through the binding as the case's name.

use crate::component::{Bound, Value};
use crate::widget::binding::{BoxedBinding, Pulled, derived};
use crate::widget::{AppModel, Button, Decorator, MenuButton, MenuItem, ModelPath, Widget};

#[path = "enum_field_component.rs"]
mod component;
pub use component::{EnumFieldProps, EnumOptionProps};

/// The case the bound value took while the menu was open: shown once it closes, so a case set
/// elsewhere never moves the menu from under the person picking.
enum Waiting {
    Nothing,
    Case(String),
}

/// The chip of an enum's cases, opening them to pick another.
pub struct EnumField {
    inner: MenuButton,
    id: String,
    source: Option<crate::LocalState<String>>,
    cases: Vec<EnumOptionProps>,
    label: Option<String>,
    value: String,
    edits: Bound<String>,
    bound: Option<BoxedBinding<String>>,
    waiting: Waiting,
}

impl EnumField {
    /// A field holding `chosen` (a case's name), offering `cases`, named `label`, and
    /// sending a picked case through `edits`.
    #[must_use]
    pub fn new(
        id: &str,
        chosen: &str,
        cases: Vec<EnumOptionProps>,
        label: Option<String>,
        edits: Bound<String>,
    ) -> Self {
        Self {
            inner: field(id, chosen, &cases, label.as_deref(), &edits),
            id: id.to_owned(),
            source: None,
            cases,
            label,
            value: chosen.to_owned(),
            edits,
            bound: None,
            waiting: Waiting::Nothing,
        }
    }

    /// A field following the case `state` holds as its name, offering `cases` and sending a
    /// picked case through `edits`: a case set there shows in place.
    pub(super) fn from_state(
        id: &str,
        state: &crate::LocalState<String>,
        cases: Vec<EnumOptionProps>,
        label: Option<String>,
        edits: Bound<String>,
    ) -> Self {
        let (chosen, binding) = derived(state, |name: &String| name.clone());
        let mut field = Self::new(id, &chosen, cases, label, edits);
        field.bound = Some(binding);
        field.source = Some(state.clone());
        field
    }

    /// The case the field holds, by its name.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Show `chosen`, the case set elsewhere.
    pub fn set_value(&mut self, chosen: &str) {
        if chosen == self.value || self.inner.is_open() {
            return;
        }
        chosen.clone_into(&mut self.value);
        self.inner = field(
            &self.id,
            &self.value,
            &self.cases,
            self.label.as_deref(),
            &self.edits,
        );
    }

    fn show_waiting(&mut self) -> bool {
        if self.inner.is_open() {
            return false;
        }
        let Waiting::Case(chosen) = std::mem::replace(&mut self.waiting, Waiting::Nothing) else {
            return false;
        };
        if chosen == self.value {
            return false;
        }
        self.set_value(&chosen);
        true
    }
}

fn field(
    id: &str,
    chosen: &str,
    cases: &[EnumOptionProps],
    label: Option<&str>,
    edits: &Bound<String>,
) -> MenuButton {
    let shown = cases
        .iter()
        .find(|case| case.name == chosen)
        .cloned()
        .unwrap_or_default();
    let items = cases
        .iter()
        .map(|case| {
            let name = case.name.clone();
            let edits = edits.clone();
            let item = MenuItem::action(case.label.clone(), move || {
                edits.edited(Value::Text(name.clone()));
            })
            .with_checked(case.name == chosen)
            .with_enabled(!case.name.is_empty());
            match case.icon {
                Some(icon) => item.with_icon(icon),
                None => item,
            }
        })
        .collect();
    let mut chip = Button::new(id, &shown.label)
        .showing_choice()
        .tone(shown.tone);
    if let Some(icon) = shown.icon {
        chip = chip.with_icon(icon);
    }
    if let Some(label) = label.filter(|label| !label.is_empty()) {
        chip = chip.with_accessible_name(&format!("{label}, {}", shown.label));
    }
    chip.with_menu(items)
}

impl Decorator for EnumField {
    type Inner = MenuButton;

    fn inner(&self) -> &MenuButton {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut MenuButton {
        &mut self.inner
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(bound) = &mut self.bound
            && let Pulled::Value(chosen) = bound.pull(state, path)
        {
            if self.inner.is_open() {
                self.waiting = Waiting::Case(chosen);
            } else {
                self.set_value(&chosen);
            }
        }
        Widget::sync(&mut self.inner, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let shown = self.show_waiting();
        let inner = Widget::update(&mut self.inner, state, path);
        shown || inner
    }

    fn name(&self) -> &str {
        "EnumField"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Tone;

    fn cases() -> (Vec<EnumOptionProps>, EnumOptionProps) {
        (
            vec![
                EnumOptionProps {
                    name: "none".to_owned(),
                    label: "None".to_owned(),
                    ..EnumOptionProps::default()
                },
                EnumOptionProps {
                    name: "high".to_owned(),
                    label: "High".to_owned(),
                    tone: Tone::Warning,
                    ..EnumOptionProps::default()
                },
            ],
            EnumOptionProps::default(),
        )
    }

    #[test]
    fn the_case_the_binding_holds_is_the_case_it_shows() {
        let (cases, _) = cases();
        let state = crate::LocalState::new("none".to_owned());
        let mut field = EnumField::from_state(
            "priority",
            &state,
            cases,
            Some("Priority".to_owned()),
            Bound::default(),
        );
        assert_eq!(field.value(), "none", "the case the binding holds is shown");
        state.set("high".to_owned());
        Decorator::sync(&mut field, &(), &());
        assert_eq!(field.value(), "high", "a case set elsewhere shows in place");
        field.inner_mut().open(true);
        state.set("none".to_owned());
        Decorator::sync(&mut field, &(), &());
        assert_eq!(
            field.value(),
            "high",
            "a change while the menu is open is not shown yet"
        );
        field.inner_mut().close();
        assert!(Decorator::update(&mut field, &(), &()));
        assert_eq!(
            field.value(),
            "none",
            "the case it took shows once it closes"
        );
    }
}
