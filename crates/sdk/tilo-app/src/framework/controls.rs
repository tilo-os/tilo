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

/// A view opened a record of `P` beside the app's own, in the person's
/// room: a surface report lists its surface too, marked as the person's.
pub(crate) fn opened<P: Resource>() {
    OPENED.with(|opened| {
        if let Some(opened) = opened.borrow_mut().as_mut()
            && !opened.iter().any(|surface| surface.kind == P::KIND)
        {
            opened.push(tilo_resource::Surface {
                person: true,
                ..P::surface()
            });
        }
    });
}

/// A control could not be bound to `what` because of `why`.
///
/// # Panics
/// Panics where bindings are strict (`testing::strict_bindings`).
pub(crate) fn unbound<R: Resource>(what: &str, why: &str) {
    unbound_of(R::KIND, what, why);
}

/// [`unbound`] for a resource of kind `kind`.
///
/// # Panics
/// Panics where bindings are strict (`testing::strict_bindings`).
pub(crate) fn unbound_of(kind: &str, what: &str, why: &str) {
    let collected = UNBOUND.with(|unbound| {
        unbound.borrow_mut().as_mut().map(|unbound| {
            unbound.push(format!("a control of `{kind}` is bound to {what}: {why}"));
        })
    });
    if collected.is_some() {
        return;
    }
    assert!(
        !STRICT.with(Cell::get),
        "a control of `{kind}` is bound to {what}, which it cannot be: {why}"
    );
    tracing::warn!(kind, binding = what, reason = why, "control not bound");
}

/// What a control's committed value `I` invokes its action with.
type Arguments<I> =
    Box<dyn Fn(I) -> Result<serde_json::Map<String, serde_json::Value>, String> + Send + Sync>;

/// The published actions of one resource, whatever its type: every binding
/// is made here, so a resource type adds only the thin typed entry points of
/// [`Actions`].
#[derive(Clone)]
pub(crate) struct ActionTable {
    /// The app whose resource this is, for the keys of its actions.
    app: &'static str,
    /// The resource's kind.
    kind: &'static str,
    resource: Rc<RefCell<Published>>,
    /// Another record than the app's own, published at this id, whose
    /// actions a live row may offer; `None` for the app's own resource.
    record: Option<Rc<str>>,
}

/// What bound controls look up: a publication shared with the root that
/// built it, and the few actions the views bound over it since, which win
/// over the shared ones of the same id.
struct Published {
    base: Rc<tilo_actions::Resource>,
    overlay: Vec<tilo_actions::ActionSpec>,
}

impl Published {
    /// The published spec of action `id`.
    fn find(&self, id: &str) -> Option<&tilo_actions::ActionSpec> {
        self.overlay
            .iter()
            .find(|spec| spec.id == id)
            .or_else(|| self.base.actions.iter().find(|spec| spec.id == id))
    }
}

impl ActionTable {
    fn new(app: &'static str, kind: &'static str, published: Rc<tilo_actions::Resource>) -> Self {
        Self {
            app,
            kind,
            resource: Rc::new(RefCell::new(Published {
                base: published,
                overlay: Vec::new(),
            })),
            record: None,
        }
    }

    /// Route the actions to the record `record`, where it may be changed.
    fn route(&mut self, record: &str, editable: bool) {
        self.record = editable.then(|| Rc::from(record));
    }

    /// Follow the canonical publication of this same hosted resource.
    pub(crate) fn update(&self, published: tilo_actions::Resource) {
        self.update_shared(Rc::new(published), Vec::new());
    }

    /// Follow `base`, a publication shared with whoever built it, with
    /// `overlay`'s actions in place of its own of the same id: nothing of
    /// `base` is copied, and the last one is only let go.
    pub(crate) fn update_shared(
        &self,
        base: Rc<tilo_actions::Resource>,
        overlay: Vec<tilo_actions::ActionSpec>,
    ) {
        let mut current = self.resource.borrow_mut();
        // A handle belongs to one root; a replacement gets its own handle.
        if current.base.id == base.id && current.base.kind == base.kind {
            *current = Published { base, overlay };
        } else {
            tracing::error!("action publication changed resource identity");
        }
    }

    /// The key of action `id`.
    fn key(&self, id: &str) -> ActionKey {
        ActionKey::new(self.app, self.kind, id)
    }

    /// The published spec of the action `key` names, or why there is none.
    fn spec(&self, key: &ActionKey) -> Result<tilo_actions::ActionSpec, String> {
        let published = self.resource.borrow();
        if published.base.kind != key.kind {
            return Err(format!("the resource is a `{}`", published.base.kind));
        }
        published
            .find(&key.action)
            .cloned()
            .ok_or_else(|| "the resource does not publish it".to_owned())
    }

    /// The binding of action `id` for a control with input `I`, or why
    /// there is none.
    fn binding<I: 'static>(
        &self,
        id: &str,
        arguments: Arguments<I>,
    ) -> Result<ActionBinding<I>, (ActionKey, String)> {
        let key = self.key(id);
        let spec = self.spec(&key).map_err(|why| (key.clone(), why))?;
        let published = self.exporting(spec.clone());
        ActionBinding::fallible(&published, &spec, arguments)
            .map_err(|error| (key, error.to_string()))
    }

    /// The binding of one fully specified invocation of action `id`, or why
    /// there is none.
    fn invocation(
        &self,
        id: &str,
        refusal: Option<&str>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionBinding<()>, (ActionKey, String)> {
        let key = self.key(id);
        if let Some(refusal) = refusal {
            return Err((key, refusal.to_owned()));
        }
        let spec = self.spec(&key).map_err(|why| (key.clone(), why))?;
        let published = self.exporting(spec.clone());
        ActionBinding::static_json(&published, &spec, arguments)
            .map_err(|error| (key, error.to_string()))
    }

    /// [`Self::binding`] for a control being built: an action the resource
    /// does not publish fails tests and `tilo-app check`.
    fn control_binding<I: 'static>(
        &self,
        id: &str,
        arguments: Arguments<I>,
    ) -> Option<ActionBinding<I>> {
        self.binding(id, arguments)
            .map_err(|(key, why)| unbound_of(self.kind, &format!("`{key}`"), &why))
            .ok()
    }

    /// The binding of the field edit described as `what` for a control with
    /// input `I`, each committed input becoming the edit `edit` makes;
    /// `None`, and a failure in tests, when the resource has no field edits
    /// (`edits`) or the field is read-only.
    fn edit_binding<I: 'static>(
        &self,
        what: &str,
        edits: bool,
        readonly: bool,
        edit: Box<dyn Fn(I) -> Result<serde_json::Value, String> + Send + Sync>,
    ) -> Option<ActionBinding<I>> {
        if !edits {
            unbound_of(self.kind, what, "it has no field edits");
            return None;
        }
        if readonly {
            unbound_of(
                self.kind,
                what,
                "the field is read-only; only verbs change it",
            );
            return None;
        }
        let spec = match self.spec(&self.key(tilo_resource::FIELD_EDIT)) {
            Ok(spec) => spec,
            Err(why) => {
                unbound_of(self.kind, what, &why);
                return None;
            }
        };
        let published = self.exporting(spec.clone());
        let arguments: Arguments<I> = Box::new(move |input| {
            let edit = edit(input)?;
            Ok(serde_json::Map::from_iter([(
                tilo_resource::EDIT_PARAM.to_owned(),
                edit,
            )]))
        });
        ActionBinding::fallible(&published, &spec, arguments)
            .map_err(|error| unbound_of(self.kind, what, &error.to_string()))
            .ok()
    }

    /// The binding of setting the field at `segments` (described as `what`)
    /// for a control with input `I`, each committed input becoming the value
    /// `value` makes of it.
    fn set_binding<I: 'static>(
        &self,
        what: &str,
        edits: bool,
        readonly: bool,
        segments: &[tilo_resource::Segment],
        value: Box<dyn Fn(I) -> Result<serde_json::Value, String> + Send + Sync>,
    ) -> Option<ActionBinding<I>> {
        let path = serde_json::to_value(segments).unwrap_or_default();
        self.edit_binding(
            what,
            edits,
            readonly,
            Box::new(move |input| {
                let value = value(input)?;
                let mut edit = serde_json::to_value(tilo_resource::EditOp::Set { value })
                    .map_err(|error| error.to_string())?;
                if let serde_json::Value::Object(edit) = &mut edit {
                    edit.insert("path".to_owned(), path.clone());
                }
                Ok(edit)
            }),
        )
    }

    /// The resource as a binding names it, exporting `spec` alone: one of
    /// its published actions, or an effect, which the handler a view or
    /// surface registers publishes beside them.
    fn exporting(&self, spec: tilo_actions::ActionSpec) -> tilo_actions::Resource {
        let published = &self.resource.borrow().base;
        tilo_actions::Resource {
            id: published.id.clone(),
            semantic_identity: published.semantic_identity.clone(),
            kind: published.kind.clone(),
            label: published.label.clone(),
            presentation: None,
            children: Vec::new(),
            actions: vec![spec],
            views: Vec::new(),
        }
    }

    /// The binding of the effect `spec` (effect `id`) for a control with
    /// input `I`.
    fn effect_binding<I: 'static>(
        &self,
        id: &str,
        spec: Option<tilo_actions::ActionSpec>,
        arguments: Arguments<I>,
    ) -> Option<ActionBinding<I>> {
        let resource = self.exporting(spec?);
        let spec = resource.actions.first()?.clone();
        ActionBinding::fallible(&resource, &spec, arguments)
            .map_err(|error| tracing::warn!(%error, effect = id, "control not bound"))
            .ok()
    }

    /// The binding of one fully specified effect `id`, for a button.
    fn effect_invocation(
        &self,
        id: &str,
        spec: Option<tilo_actions::ActionSpec>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ActionBinding<()>> {
        let resource = self.exporting(spec?);
        let spec = resource.actions.first()?.clone();
        ActionBinding::static_json(&resource, &spec, arguments)
            .map_err(|error| tracing::warn!(%error, effect = id, "control not bound"))
            .ok()
    }
}

/// The spec `E` declares for effect `id`.
fn effect_spec<E: Effect>(id: &str) -> Option<tilo_actions::ActionSpec> {
    E::specs().into_iter().find(|spec| spec.id == id)
}

/// The actions a resource publishes, for binding controls that are built
/// outside the view function: rows a table, list or tree creates on demand.
/// Cheap to clone; clones share the published descriptor.
pub struct Actions<R: Resource> {
    table: ActionTable,
    marker: PhantomData<fn() -> R>,
}

impl<R: Resource> Clone for Actions<R> {
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
            marker: PhantomData,
        }
    }
}

impl<R: Resource> Actions<R> {
    pub(crate) fn new(app: &'static str, published: Rc<tilo_actions::Resource>) -> Self {
        Self {
            table: ActionTable::new(app, R::KIND, published),
            marker: PhantomData,
        }
    }

    /// These actions as another record's, published at `record`: a live row
    /// may offer them ([`Self::operation`]) where it may change it.
    pub(crate) fn routed_to(mut self, record: &str, editable: bool) -> Self {
        self.table.route(record, editable);
        self
    }

    /// `decl` — a bound field edit ([`tilo_resource::ActionDecl::edit`],
    /// `edit_asking`) or verb of another record — as an operation a live
    /// row offers ([`LiveRow::operation`](super::LiveRow::operation)),
    /// carried out on that record as its person (*this* app's notification
    /// rule on the app's row). `None` for the app's own resource, whose row
    /// edits are [`ResourceSurface::row_declarations`](super::ResourceSurface::row_declarations),
    /// and where the record is read only here.
    #[must_use]
    pub fn operation(
        &self,
        decl: &tilo_resource::ActionDecl<R>,
    ) -> Option<tilo_actions::ActionSpec> {
        let record = self.table.record.as_deref()?;
        Some(super::record::route_spec(
            tilo_resource::declaration_spec(decl),
            record,
        ))
    }

    /// The table these actions bind from, which follows the publication
    /// whatever the resource's type.
    pub(crate) fn table(&self) -> ActionTable {
        self.table.clone()
    }

    /// The binding of `action` for a control with input `I`, or `None` while
    /// the resource does not publish it (an edit bound to view state that
    /// is not there now).
    pub fn binding<I: 'static>(&self, action: impl Binds<R, I>) -> Option<ActionBinding<I>> {
        let id = action.action_id();
        self.table
            .binding(id, Box::new(move |input| action.arguments(input)))
            .ok()
    }

    /// The binding of one fully specified invocation, for a button, or
    /// `None` while the resource does not publish it.
    pub fn invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        let refusal = invoke.refusal().map(str::to_owned);
        self.table
            .invocation(invoke.id, refusal.as_deref(), invoke.arguments)
            .ok()
    }

    /// [`Self::binding`] for a control being built: an action the resource
    /// does not publish fails tests and `tilo-app check`.
    pub(crate) fn control_binding<I: 'static>(
        &self,
        action: impl Binds<R, I>,
    ) -> Option<ActionBinding<I>> {
        let id = action.action_id();
        self.table
            .control_binding(id, Box::new(move |input| action.arguments(input)))
    }

    /// [`Self::invocation`] for a control being built.
    pub(crate) fn control_invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        let refusal = invoke.refusal().map(str::to_owned);
        self.table
            .invocation(invoke.id, refusal.as_deref(), invoke.arguments)
            .map_err(|(key, why)| unbound_of(R::KIND, &format!("`{key}`"), &why))
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
        self.table
            .control_binding(id, Box::new(move |()| arguments()))
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
        self.edit_binding_with(path, move |path, value| Ok(edit(path, value)))
    }

    /// [`Self::edit_binding`] for a control whose input `I` is not the
    /// field's value: a dropdown of "125%" for a scale of 1.25, a slider's
    /// position on a log scale. `edit` maps each committed input to the
    /// field edit, or refuses it, which refuses the input like a value the
    /// field cannot take.
    pub fn edit_binding_with<V: 'static, I: 'static>(
        &self,
        path: Path<R, V>,
        edit: impl Fn(&Path<R, V>, I) -> Result<Edit<R>, String> + Send + Sync + 'static,
    ) -> Option<ActionBinding<I>> {
        let what = format!("the field {path:?}");
        let readonly = R::shape().is_some_and(|shape| shape.readonly_at(path.segments()));
        self.table.edit_binding(
            &what,
            R::EDITS,
            readonly,
            Box::new(move |input| {
                let edit = edit(&path, input)?;
                serde_json::to_value(&edit).map_err(|error| error.to_string())
            }),
        )
    }

    /// The binding of the field at `path` for a control with input `I`:
    /// each committed input sets the field to what `value` makes of it, or is
    /// refused where `value` refuses it. What a settings row binds; the same
    /// edit as [`Self::edit_binding_with`] and [`Path::set`].
    pub fn set_binding<V, I>(
        &self,
        path: Path<R, V>,
        value: impl Fn(I) -> Result<V, String> + Send + Sync + 'static,
    ) -> Option<ActionBinding<I>>
    where
        V: tilo_resource::Settable + 'static,
        I: 'static,
    {
        let what = format!("the field {path:?}");
        let readonly = R::shape().is_some_and(|shape| shape.readonly_at(path.segments()));
        let segments = path.into_segments();
        self.table
            .set_binding(&what, R::EDITS, readonly, &segments, json_of(value))
    }

    /// [`Self::set_binding`] of a control whose input is the field's value.
    pub fn field_binding<V>(&self, path: Path<R, V>) -> Option<ActionBinding<V>>
    where
        V: tilo_resource::Settable + 'static,
    {
        self.set_binding(path, same::<V>)
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

    /// The binding of effect `effect` for a control with input `I`
    /// (`Backups::RESTORE` for a dropdown of snapshots).
    pub fn effect_binding<E: Effect, I: 'static>(
        &self,
        effect: impl Binds<E, I>,
    ) -> Option<ActionBinding<I>> {
        let id = effect.action_id();
        self.table.effect_binding(
            id,
            effect_spec::<E>(id),
            Box::new(move |input| effect.arguments(input)),
        )
    }

    /// The binding of one fully specified effect, for a button.
    pub fn effect<E: Effect>(&self, effect: &E) -> Option<ActionBinding<()>> {
        self.table.effect_invocation(
            effect.id(),
            effect_spec::<E>(effect.id()),
            effect.arguments(),
        )
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

/// `value` with its result in its JSON form, the same for every resource.
fn json_of<V: tilo_resource::Model, I>(
    value: impl Fn(I) -> Result<V, String> + Send + Sync + 'static,
) -> Box<dyn Fn(I) -> Result<serde_json::Value, String> + Send + Sync> {
    Box::new(move |input| value(input).map(|value| value.to_json()))
}

/// A control's value, set as it is.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every value conversion has"
)]
fn same<V>(value: V) -> Result<V, String> {
    Ok(value)
}
