//! Binding any toolkit control to a resource action.
//!
//! [`ViewCx`](super::ViewCx) has shorthands for the common controls
//! (`input`, `checkbox`, `button`). Every other action-bound toolkit control —
//! a slider, a number field, a dropdown, a toggle, a button a virtual list
//! builds for a row long after the view was built — goes through the same two
//! pieces: an [`ActionControl`] (the control accepts an action binding) and
//! [`Actions`] (the resource's published actions, kept by the view).

use std::marker::PhantomData;
use std::rc::Rc;

use tilo_actions::ActionBinding;
use tilo_resource::{Binds, Effect, Invoke, Resource};
use ui_core::widget::{
    Button, Checkbox, Dropdown, EditableLabel, NumberInput, Slider, TextInput, Toggle,
};

/// A toolkit control whose committed value `I` can invoke an action.
pub trait ActionControl<I>: Sized {
    /// Invoke `binding` with the control's committed value.
    #[must_use]
    fn bind_action(self, binding: ActionBinding<I>) -> Self;

    /// Present the control disabled: its action is not available.
    #[must_use]
    fn unavailable(self) -> Self;
}

macro_rules! action_control {
    ($($control:ty => $input:ty, |$disabled:ident| $disable:expr;)*) => {$(
        impl ActionControl<$input> for $control {
            fn bind_action(self, binding: ActionBinding<$input>) -> Self {
                self.on_action(binding)
            }

            fn unavailable(self) -> Self {
                let $disabled = self;
                $disable
            }
        }
    )*};
}

action_control! {
    TextInput => String, |control| control.disabled(true);
    Checkbox => bool, |control| control.with_enabled(false);
    Toggle => bool, |control| control.with_enabled(false);
    Slider => f64, |control| control.with_enabled(false);
    NumberInput => f64, |control| control.with_enabled(false);
    Dropdown => String, |control| control.with_enabled(false);
    Button => (), |control| control.with_enabled(false);
    // Unavailable, an inline rename stays a label that is never committed.
    EditableLabel => String, |control| control;
}

/// The actions a resource publishes, for binding controls that are built
/// outside the view function: rows a table, list or tree creates on demand.
/// Cheap to clone; clones share the published descriptor.
pub struct Actions<R: Resource> {
    published: Rc<tilo_actions::Resource>,
    marker: PhantomData<fn() -> R>,
}

impl<R: Resource> Clone for Actions<R> {
    fn clone(&self) -> Self {
        Self {
            published: Rc::clone(&self.published),
            marker: PhantomData,
        }
    }
}

impl<R: Resource> Actions<R> {
    pub(crate) fn new(published: Rc<tilo_actions::Resource>) -> Self {
        Self {
            published,
            marker: PhantomData,
        }
    }

    /// The binding of `action` for a control with input `I`, or `None` when
    /// the resource does not publish it.
    pub fn binding<I: 'static>(&self, action: impl Binds<R, I>) -> Option<ActionBinding<I>> {
        let id = action.action_id();
        let spec = self
            .published
            .actions
            .iter()
            .find(|spec| spec.id == id)?
            .clone();
        ActionBinding::new(&self.published, &spec, move |input| action.arguments(input))
            .map_err(|error| tracing::warn!(%error, action = id, "control not bound"))
            .ok()
    }

    /// The binding of one fully specified invocation, for a button.
    pub fn invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        let spec = self
            .published
            .actions
            .iter()
            .find(|spec| spec.id == invoke.id)?
            .clone();
        ActionBinding::static_json(&self.published, &spec, invoke.arguments)
            .map_err(|error| tracing::warn!(%error, action = invoke.id, "control not bound"))
            .ok()
    }

    /// `control`, bound to `action`, or disabled when it is not published.
    pub fn bind<I: 'static, C: ActionControl<I>>(&self, control: C, action: impl Binds<R, I>) -> C {
        match self.binding(action) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control` (a button), bound to one fully specified invocation.
    pub fn bind_invocation<C: ActionControl<()>>(&self, control: C, invoke: Invoke<R>) -> C {
        match self.invocation(invoke) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// The resource as a binding names it, exporting effect `id` of `E`: an
    /// effect is published beside the resource's actions by the handler a
    /// view or surface registers, not by the resource type.
    fn exporting<E: Effect>(&self, id: &str) -> Option<tilo_actions::Resource> {
        let spec = E::specs().into_iter().find(|spec| spec.id == id)?;
        Some(tilo_actions::Resource {
            id: self.published.id.clone(),
            kind: self.published.kind.clone(),
            label: self.published.label.clone(),
            presentation: None,
            children: Vec::new(),
            actions: vec![spec],
            views: Vec::new(),
        })
    }

    /// The binding of effect `effect` for a control with input `I`
    /// (`Backups::RESTORE` for a dropdown of snapshots).
    pub fn effect_binding<E: Effect, I: 'static>(
        &self,
        effect: impl Binds<E, I>,
    ) -> Option<ActionBinding<I>> {
        let id = effect.action_id();
        let resource = self.exporting::<E>(id)?;
        let spec = resource.actions.first()?.clone();
        ActionBinding::new(&resource, &spec, move |input| effect.arguments(input))
            .map_err(|error| tracing::warn!(%error, effect = id, "control not bound"))
            .ok()
    }

    /// The binding of one fully specified effect, for a button.
    pub fn effect<E: Effect>(&self, effect: &E) -> Option<ActionBinding<()>> {
        let resource = self.exporting::<E>(effect.id())?;
        let spec = resource.actions.first()?.clone();
        ActionBinding::static_json(&resource, &spec, effect.arguments())
            .map_err(|error| tracing::warn!(%error, effect = effect.id(), "control not bound"))
            .ok()
    }

    /// `control`, bound to effect `effect`.
    pub fn bind_effect<E: Effect, I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        effect: impl Binds<E, I>,
    ) -> C {
        match self.effect_binding(effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control` (a button), bound to one fully specified effect.
    pub fn bind_effect_invocation<E: Effect, C: ActionControl<()>>(
        &self,
        control: C,
        effect: &E,
    ) -> C {
        match self.effect(effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }
}
