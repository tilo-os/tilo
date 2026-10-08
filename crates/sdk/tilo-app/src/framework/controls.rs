//! Binding any toolkit control to a resource action.
//!
//! [`ViewCx`](super::ViewCx) has shorthands for the common controls
//! (`input`, `checkbox`, `button`). Every other action-bound toolkit control —
//! a slider, a number field, a dropdown, a toggle, a button a virtual list
//! builds for a row long after the view was built — goes through the same two
//! pieces: an [`ActionControl`] (the control accepts an action binding) and
//! [`Actions`] (the resource's published actions, kept by the view).

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use tilo_actions::ActionBinding;
use tilo_resource::{Binds, Effect, Invoke, Resource};
use ui_core::widget::{
    Button, Checkbox, Dropdown, EditableLabel, EmptyState, NumberInput, Slider, TextInput, Toggle,
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
    ui_core::KeyboardMapSelection => String, |control| control.with_enabled(false);
    Button => (), |control| control.with_enabled(false);
    EmptyState => (), |control| control.with_enabled(false);
    // Unavailable, an inline rename stays a label that is never committed.
    EditableLabel => String, |control| control;
}

/// The actions a resource publishes, for binding controls that are built
/// outside the view function: rows a table, list or tree creates on demand.
/// Cheap to clone; clones share the published descriptor.
pub struct Actions<R: Resource> {
    published: Rc<RefCell<tilo_actions::Resource>>,
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
            published: Rc::new(RefCell::new(Rc::unwrap_or_clone(published))),
            marker: PhantomData,
        }
    }

    /// Follow the canonical publication of this same hosted resource.
    pub(crate) fn update(&self, published: tilo_actions::Resource) {
        let mut current = self.published.borrow_mut();
        // A handle belongs to one root; a replacement gets its own handle.
        if current.id == published.id && current.kind == published.kind {
            *current = published;
        } else {
            tracing::error!("action publication changed resource identity");
        }
    }

    /// The binding of `action` for a control with input `I`, or `None` when
    /// the resource does not publish it.
    pub fn binding<I: 'static>(&self, action: impl Binds<R, I>) -> Option<ActionBinding<I>> {
        let id = action.action_id();
        let published = self.published.borrow();
        let spec = published.actions.iter().find(|spec| spec.id == id)?.clone();
        ActionBinding::new(&published, &spec, move |input| action.arguments(input))
            .map_err(|error| tracing::warn!(%error, action = id, "control not bound"))
            .ok()
    }

    /// The binding of one fully specified invocation, for a button.
    pub fn invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        let published = self.published.borrow();
        let spec = published
            .actions
            .iter()
            .find(|spec| spec.id == invoke.id)?
            .clone();
        ActionBinding::static_json(&published, &spec, invoke.arguments)
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
        let spec = E::spec(id)?;
        let published = self.published.borrow();
        Some(tilo_actions::Resource {
            id: published.id.clone(),
            semantic_identity: published.semantic_identity.clone(),
            kind: published.kind.clone(),
            label: published.label.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::{Doc, EffectDecl, ModelError, NoAction};

    struct Document;
    #[cfg(test)]
    impl Resource for Document {
        const KIND: &'static str = "test.effect-binding";
        const LABEL: &'static str = "Effect binding";
        type State = ();
        type Action = NoAction;
        fn read(_: &Doc) -> Result<(), ModelError> {
            Ok(())
        }
    }

    #[derive(tilo_resource::Effect)]
    #[effect(operation = "test.effect-binding")]
    enum Vocabulary {
        /// Read status.
        #[effect(permission = "read", returns)]
        Status,
        /// Delete one item.
        #[effect(row, destructive)]
        Delete { item: String },
    }

    struct Selective;
    impl Effect for Selective {
        const OPERATION: &'static str = Vocabulary::OPERATION;
        fn declarations() -> Vec<EffectDecl> {
            panic!("binding one control must not enumerate unrelated effects")
        }
        fn declaration(id: &str) -> Option<EffectDecl> {
            Vocabulary::declaration(id)
        }
        fn spec(id: &str) -> Option<tilo_actions::ActionSpec> {
            Vocabulary::spec(id)
        }
        fn id(&self) -> &'static str {
            "status"
        }
        fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
            serde_json::Map::new()
        }
        fn decode(_: &str, _: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
            Err("not invoked in binding test".into())
        }
    }

    #[test]
    fn binding_one_effect_uses_its_fresh_declaration_and_refuses_row_effects() {
        let actions = Actions::<Document>::new(Rc::new(tilo_actions::Resource::new(
            "original-resource",
            Document::KIND,
            Document::LABEL,
        )));
        let selected = actions.exporting::<Selective>("status").unwrap();
        let expected = actions.exporting::<Vocabulary>("status").unwrap();
        assert_eq!(
            tilo_actions::door::survey(&[selected]),
            tilo_actions::door::survey(&[expected]),
        );
        assert!(actions.effect(&Selective).is_some());
        assert!(actions.exporting::<Selective>("delete").is_none());
        assert!(actions.exporting::<Selective>("unknown").is_none());
    }
}
