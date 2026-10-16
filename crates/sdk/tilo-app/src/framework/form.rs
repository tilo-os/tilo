//! Forms: a verb's parameters filled in before it is invoked.
//!
//! A form is a local instance of a verb's parameter struct: never synced,
//! never undoable, invisible to collaborators. Its fields bind to controls
//! with the same paths a record's do (`cx.input(form.paths().url)`),
//! and its button submits it as one invocation of the verb.

use std::any::Any;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tilo_actions::ActionBinding;
use tilo_resource::{Invoke, Params, Path, Resource, Segment};
use ui_core::LocalState;

use super::controls::Actions;
use super::dispatch::Submit;
use tilo_lock::MutexRecover as _;

/// One field of a form, whatever its type.
trait Slot: Send + Sync {
    fn json(&self) -> Result<serde_json::Value, String>;
    fn clear(&self);
    fn any(&self) -> &dyn Any;
}

struct Typed<V: Send + Sync + 'static>(LocalState<V>);

impl<V> Slot for Typed<V>
where
    V: Serialize + Default + Send + Sync + 'static,
{
    fn json(&self) -> Result<serde_json::Value, String> {
        self.0
            .with(|value| serde_json::to_value(value))
            .map_err(|error| error.to_string())
    }

    fn clear(&self) {
        self.0.set(V::default());
    }

    fn any(&self) -> &dyn Any {
        &self.0
    }
}

type Slots = Arc<Mutex<BTreeMap<String, Box<dyn Slot>>>>;

/// The parameters of the verb `P` as they are being filled in
/// ([`ViewCx::form`](super::ViewCx::form)). Clones share the values.
pub struct Form<P: Params> {
    slots: Slots,
    marker: PhantomData<fn() -> P>,
}

impl<P: Params> Clone for Form<P> {
    fn clone(&self) -> Self {
        Self {
            slots: Arc::clone(&self.slots),
            marker: PhantomData,
        }
    }
}

impl<P: Params> Form<P> {
    pub(crate) fn new() -> Self {
        Self {
            slots: Arc::default(),
            marker: PhantomData,
        }
    }

    /// The value of the field at `path`, as local state a control edits.
    pub(crate) fn field<V>(&self, path: &Path<P, V>) -> LocalState<V>
    where
        V: Serialize + Default + Send + Sync + 'static,
    {
        let key = match path.segments() {
            [Segment::Field(key)] => key.clone(),
            other => other
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("."),
        };
        let mut slots = self.slots.lock_recover();
        if let Some(state) = slots
            .get(&key)
            .and_then(|slot| slot.any().downcast_ref::<LocalState<V>>())
        {
            return state.clone();
        }
        let state = LocalState::new(V::default());
        slots.insert(key, Box::new(Typed(state.clone())));
        state
    }

    /// The paths of the verb's parameters, which controls bind to:
    /// `cx.input(form.paths().url)`.
    #[must_use]
    pub fn paths(&self) -> P::Paths {
        P::paths()
    }

    /// The parameters as filled in now.
    ///
    /// # Errors
    /// Returns why they are not a valid `P` (a required field never set).
    pub fn values(&self) -> Result<P, String> {
        read::<P>(&self.slots)
    }

    /// Empty every field again.
    pub fn clear(&self) {
        clear(&self.slots);
    }

    /// The form's submission, for a button (`cx.button("add", form.submit())`)
    /// or a dispatcher: one invocation of the verb with the parameters as
    /// filled in when it is pressed. The form empties once it is sent.
    #[must_use]
    pub fn submit(&self) -> FormSubmit<P> {
        FormSubmit {
            slots: Arc::clone(&self.slots),
            marker: PhantomData,
        }
    }
}

fn read<P: Params>(slots: &Slots) -> Result<P, String> {
    let fields = slots
        .lock_recover()
        .iter()
        .map(|(key, slot)| Ok((key.clone(), slot.json()?)))
        .collect::<Result<serde_json::Map<_, _>, String>>()?;
    serde_json::from_value(serde_json::Value::Object(fields)).map_err(|error| error.to_string())
}

fn clear(slots: &Slots) {
    for slot in slots.lock_recover().values() {
        slot.clear();
    }
}

/// A form's submission ([`Form::submit`]).
pub struct FormSubmit<P: Params> {
    slots: Slots,
    marker: PhantomData<fn() -> P>,
}

impl<P> Submit<P::Resource> for FormSubmit<P>
where
    P: Params,
    P::Resource: Resource,
{
    fn action_id(&self) -> &'static str {
        P::ACTION.id
    }

    fn into_invoke(self) -> Result<Invoke<P::Resource>, String> {
        let params = read::<P>(&self.slots)?;
        clear(&self.slots);
        Ok(P::ACTION.with(params))
    }

    fn control_binding(self, actions: &Actions<P::Resource>) -> Option<ActionBinding<()>> {
        let slots = self.slots;
        actions.deferred_binding(P::ACTION.id, move || {
            let params = read::<P>(&slots)?;
            let arguments = P::ACTION.arguments(&params)?;
            clear(&slots);
            Ok(arguments)
        })
    }
}

/// Where a bound control's value lives: a field of the record, or a field
/// of a form. See [`ViewCx::input`](super::ViewCx::input).
pub enum Attached<V> {
    /// A field of the record: shown from the state, changed by field edits.
    Record {
        id: String,
        state: LocalState<V>,
        binding: Option<Box<ActionBinding<V>>>,
    },
    /// A field of a form: the control edits the local value itself.
    Local { id: String, state: LocalState<V> },
}

/// Marks the [`Bindable`](super::Bindable) implementation for a record's field.
#[doc(hidden)]
pub enum RecordField {}

/// Marks the [`Bindable`](super::Bindable) implementation for a form's field.
#[doc(hidden)]
pub enum FormField {}

/// The forms of one view scope, by parameter type.
#[derive(Clone, Default)]
pub(crate) struct Forms(
    std::rc::Rc<std::cell::RefCell<std::collections::HashMap<std::any::TypeId, Box<dyn Any>>>>,
);

impl Forms {
    pub(crate) fn get<P: Params>(&self) -> Form<P> {
        let key = std::any::TypeId::of::<P>();
        let mut forms = self.0.borrow_mut();
        if let Some(form) = forms
            .get(&key)
            .and_then(|form| form.downcast_ref::<Form<P>>())
        {
            return form.clone();
        }
        let form = Form::<P>::new();
        forms.insert(key, Box::new(form.clone()));
        form
    }
}
