//! Where a control's value lives.
//!
//! A value control (checkbox, toggle, slider, dropdown) presents one value and
//! stores the user's edits through a [`ValueBinding`]. The toolkit ships the
//! process-local binding ([`LocalState`](crate::LocalState)); a model layer
//! supplies bindings into its own model, such as a key in the app document
//! ([`crate::doc_binding`]). The control never reads the model itself.

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

/// Two-way storage for a control's value.
pub trait ValueBinding<T> {
    /// The value to present, read in the sync pass before layout.
    fn pull(&mut self, model: &mut AppModel, path: &ModelPath) -> Pulled<T>;

    /// Store a value the user committed.
    fn push(&mut self, model: &mut AppModel, path: &ModelPath, value: &T);

    /// Publish a value the app set programmatically, with no model at hand.
    /// Bindings into a shared model ignore it: the app edits the model and
    /// the next pull presents the result.
    fn publish(&mut self, _value: &T) {}

    /// The document key this binding writes, reported to the action
    /// directory as the control's collaborative binding.
    fn document_key(&self) -> Option<&str> {
        None
    }
}

impl<T: Clone + PartialEq> ValueBinding<T> for crate::local_state::LocalBinding<T> {
    fn pull(&mut self, _model: &mut AppModel, _path: &ModelPath) -> Pulled<T> {
        self.poll().map_or(Pulled::Unchanged, Pulled::Value)
    }

    fn push(&mut self, _model: &mut AppModel, _path: &ModelPath, value: &T) {
        Self::publish(self, value);
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
