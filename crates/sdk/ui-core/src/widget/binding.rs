//! Where a control's value lives.
//!
//! A value control (checkbox, toggle, slider, dropdown) presents one value and
//! reads its value through a [`ValueBinding`]. The toolkit ships the
//! process-local binding ([`LocalState`](crate::LocalState)); a model layer
//! supplies bindings into its own model, such as a key in the app document
//! ([`crate::doc_source`]). The control never reads the model itself.

use super::{AppModel, ModelPath};

/// What a binding reports for the value to present.
#[derive(Clone, Debug, PartialEq)]
pub enum Pulled<T> {
    /// The model holds this value.
    Value(T),
    /// Nothing changed since the last pull; keep the presented value.
    Unchanged,
    /// The model holds no value; the control shows its empty state.
    Missing,
}

/// A read source and optional process-local storage for a control's value.
pub trait ValueBinding<T> {
    /// The value to present, read in the sync pass before layout.
    fn pull(&mut self, model: &AppModel, path: &ModelPath) -> Pulled<T>;

    /// Publish a value the app set programmatically, with no model at hand.
    /// Bindings into a shared model ignore it: the app edits the model and
    /// the next pull presents the result.
    fn publish(&mut self, _value: &T) {}

    /// The document key this binding reads, reported to the action
    /// directory as the control's collaborative binding.
    fn document_key(&self) -> Option<&str> {
        None
    }
}

impl<T: Clone + PartialEq> ValueBinding<T> for crate::local_state::LocalBinding<T> {
    fn pull(&mut self, _model: &AppModel, _path: &ModelPath) -> Pulled<T> {
        self.poll().map_or(Pulled::Unchanged, Pulled::Value)
    }

    fn publish(&mut self, value: &T) {
        Self::publish(self, value);
    }
}

/// A boxed binding, as controls store them.
pub type BoxedBinding<T> = Box<dyn ValueBinding<T>>;

/// The process-local binding for `state`, with the value it holds now.
pub(crate) fn local<T: Clone + PartialEq + 'static>(
    state: &crate::LocalState<T>,
) -> (T, BoxedBinding<T>) {
    let (value, binding) = crate::local_state::LocalBinding::new(state);
    (value, Box::new(binding))
}

#[cfg(test)]
mod tests {
    use crate::a11y::Action;
    use crate::widget::{Checkbox, Dropdown, Slider, Toggle, Widget};
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT};

    #[test]
    fn value_controls_read_document_and_leave_persistence_to_actions() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "checked", false).unwrap();
        doc.put(ROOT, "number", 0.25).unwrap();
        doc.put(ROOT, "choice", "first").unwrap();
        let mut controls: Vec<(Box<dyn Widget>, Action)> = vec![
            (
                Box::new(Checkbox::new("check", "checked")),
                Action::Activate,
            ),
            (Box::new(Toggle::new("toggle", "checked")), Action::Activate),
            (
                Box::new(Slider::new("slider", "number")),
                Action::SetValue("0.75".into()),
            ),
            (
                Box::new(Dropdown::new(
                    vec!["first".into(), "second".into()],
                    "choice",
                )),
                Action::SetValue("second".into()),
            ),
        ];
        let before = super::super::settle_mark(&doc);
        for (control, action) in &mut controls {
            control.sync(&doc, &ROOT);
            assert!(control.perform_action(
                action,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert_eq!(
                super::super::settle_mark(&doc),
                before,
                "a value control cannot write its read source"
            );
        }
    }
}
