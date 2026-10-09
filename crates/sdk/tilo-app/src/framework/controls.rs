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

use std::cell::Cell;

use tilo_actions::ActionBinding;
use tilo_resource::{ActionKey, Binds, Edit, Effect, Invoke, Path, Resource};
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

thread_local! {
    /// Whether a control bound to something the resource does not publish
    /// is a failure rather than a disabled control: in tests and checks.
    pub(super) static STRICT: Cell<bool> = const { Cell::new(false) };
    /// Such controls, while a surface report collects them.
    pub(super) static UNBOUND: std::cell::RefCell<Option<Vec<String>>> =
        const { std::cell::RefCell::new(None) };
    /// The surfaces of the other records the views open (the person's own),
    /// while a surface report collects them.
    pub(super) static OPENED: std::cell::RefCell<Option<Vec<tilo_resource::Surface>>> =
        const { std::cell::RefCell::new(None) };
}

/// A view opened a record of `P` beside the app's own: a surface report
/// lists its surface too.
pub(crate) fn opened<P: Resource>() {
    OPENED.with(|opened| {
        if let Some(opened) = opened.borrow_mut().as_mut()
            && !opened.iter().any(|surface| surface.kind == P::KIND)
        {
            opened.push(P::surface());
        }
    });
}

/// A control could not be bound to `what` because of `why`.
///
/// # Panics
/// Panics where bindings are strict (`testing::strict_bindings`).
pub(crate) fn unbound<R: Resource>(what: &str, why: &str) {
    let collected = UNBOUND.with(|unbound| {
        unbound.borrow_mut().as_mut().map(|unbound| {
            unbound.push(format!(
                "a control of `{}` is bound to {what}: {why}",
                R::KIND
            ));
        })
    });
    if collected.is_some() {
        return;
    }
    assert!(
        !STRICT.with(Cell::get),
        "a control of `{}` is bound to {what}, which it cannot be: {why}",
        R::KIND
    );
    tracing::warn!(
        kind = R::KIND,
        binding = what,
        reason = why,
        "control not bound"
    );
}

/// The actions a resource publishes, for binding controls that are built
/// outside the view function: rows a table, list or tree creates on demand.
/// Cheap to clone; clones share the published descriptor.
pub struct Actions<R: Resource> {
    /// The app whose resource this is, for the keys of its actions.
    app: &'static str,
    published: Rc<RefCell<tilo_actions::Resource>>,
    marker: PhantomData<fn() -> R>,
}

impl<R: Resource> Clone for Actions<R> {
    fn clone(&self) -> Self {
        Self {
            app: self.app,
            published: Rc::clone(&self.published),
            marker: PhantomData,
        }
    }
}

impl<R: Resource> Actions<R> {
    pub(crate) fn new(app: &'static str, published: Rc<tilo_actions::Resource>) -> Self {
        Self {
            app,
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

    /// The published spec of the action `key` names, or why there is none.
    fn spec(&self, key: &ActionKey) -> Result<tilo_actions::ActionSpec, String> {
        let published = self.published.borrow();
        if published.kind != key.kind {
            return Err(format!("the resource is a `{}`", published.kind));
        }
        published
            .actions
            .iter()
            .find(|spec| spec.id == key.action)
            .cloned()
            .ok_or_else(|| "the resource does not publish it".to_owned())
    }

    /// The key of `R`'s action `id`.
    fn key(&self, id: &str) -> ActionKey {
        ActionKey::new(self.app, R::KIND, id)
    }

    /// The binding of `action` for a control with input `I`, or why there
    /// is none.
    fn try_binding<I: 'static>(
        &self,
        action: impl Binds<R, I>,
    ) -> Result<ActionBinding<I>, (ActionKey, String)> {
        let key = self.key(action.action_id());
        let spec = self.spec(&key).map_err(|why| (key.clone(), why))?;
        let published = self.published.borrow();
        ActionBinding::fallible(&published, &spec, move |input| action.arguments(input))
            .map_err(|error| (key, error.to_string()))
    }

    /// The binding of one fully specified invocation, or why there is none.
    fn try_invocation(&self, invoke: Invoke<R>) -> Result<ActionBinding<()>, (ActionKey, String)> {
        let key = self.key(invoke.id);
        if let Some(refusal) = invoke.refusal() {
            return Err((key, refusal.to_owned()));
        }
        let spec = self.spec(&key).map_err(|why| (key.clone(), why))?;
        let published = self.published.borrow();
        ActionBinding::static_json(&published, &spec, invoke.arguments)
            .map_err(|error| (key, error.to_string()))
    }

    /// The binding of `action` for a control with input `I`, or `None` while
    /// the resource does not publish it (an edit bound to view state that
    /// is not there now).
    pub fn binding<I: 'static>(&self, action: impl Binds<R, I>) -> Option<ActionBinding<I>> {
        self.try_binding(action).ok()
    }

    /// The binding of one fully specified invocation, for a button, or
    /// `None` while the resource does not publish it.
    pub fn invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        self.try_invocation(invoke).ok()
    }

    /// [`Self::binding`] for a control being built: an action the resource
    /// does not publish fails tests and `tilo-app check`.
    pub(crate) fn control_binding<I: 'static>(
        &self,
        action: impl Binds<R, I>,
    ) -> Option<ActionBinding<I>> {
        self.try_binding(action)
            .map_err(|(key, why)| unbound::<R>(&format!("`{key}`"), &why))
            .ok()
    }

    /// [`Self::invocation`] for a control being built.
    pub(crate) fn control_invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        self.try_invocation(invoke)
            .map_err(|(key, why)| unbound::<R>(&format!("`{key}`"), &why))
            .ok()
    }

    /// The binding of a button that invokes `R`'s action `id` with the
    /// arguments `arguments` makes when it is pressed (a form's values).
    pub(crate) fn deferred_binding(
        &self,
        id: &str,
        arguments: impl Fn() -> Result<serde_json::Map<String, serde_json::Value>, String>
        + Send
        + Sync
        + 'static,
    ) -> Option<ActionBinding<()>> {
        let key = self.key(id);
        let spec = match self.spec(&key) {
            Ok(spec) => spec,
            Err(why) => {
                unbound::<R>(&format!("`{key}`"), &why);
                return None;
            }
        };
        let published = self.published.borrow();
        ActionBinding::fallible(&published, &spec, move |()| arguments())
            .map_err(|error| unbound::<R>(&format!("`{key}`"), &error.to_string()))
            .ok()
    }

    /// The binding of the field at `path` for a control with input `V`:
    /// each committed value becomes the field edit `edit(path, value)`.
    /// `None`, and a failure in tests, when the resource has no field edits
    /// or the field is `#[model(readonly)]`.
    pub fn edit_binding<V: 'static>(
        &self,
        path: Path<R, V>,
        edit: impl Fn(&Path<R, V>, V) -> Edit<R> + Send + Sync + 'static,
    ) -> Option<ActionBinding<V>> {
        let what = format!("the field {path:?}");
        if !R::EDITS {
            unbound::<R>(&what, "it has no field edits");
            return None;
        }
        if R::shape().is_some_and(|shape| shape.readonly_at(path.segments())) {
            unbound::<R>(&what, "the field is read-only; only verbs change it");
            return None;
        }
        let spec = match self.spec(&self.key(tilo_resource::FIELD_EDIT)) {
            Ok(spec) => spec,
            Err(why) => {
                unbound::<R>(&what, &why);
                return None;
            }
        };
        let published = self.published.borrow();
        ActionBinding::fallible(&published, &spec, move |value| {
            let edit = edit(&path, value);
            serde_json::to_value(&edit)
                .map(|edit| {
                    serde_json::Map::from_iter([(tilo_resource::EDIT_PARAM.to_owned(), edit)])
                })
                .map_err(|error| error.to_string())
        })
        .map_err(|error| unbound::<R>(&what, &error.to_string()))
        .ok()
    }

    /// `control`, bound to `action`. An action the resource does not publish
    /// leaves it disabled, and fails tests and `tilo-app check`.
    pub fn bind<I: 'static, C: ActionControl<I>>(&self, control: C, action: impl Binds<R, I>) -> C {
        match self.control_binding(action) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control` (a button), bound to one fully specified invocation.
    pub fn bind_invocation<C: ActionControl<()>>(&self, control: C, invoke: Invoke<R>) -> C {
        match self.control_invocation(invoke) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// The resource as a binding names it, exporting effect `id` of `E`: an
    /// effect is published beside the resource's actions by the handler a
    /// view or surface registers, not by the resource type.
    fn exporting<E: Effect>(&self, id: &str) -> Option<tilo_actions::Resource> {
        let spec = E::specs().into_iter().find(|spec| spec.id == id)?;
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
        ActionBinding::fallible(&resource, &spec, move |input| effect.arguments(input))
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
