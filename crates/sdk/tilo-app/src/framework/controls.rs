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
use tilo_resource::{ActionKey, Binds, Edit, Effect, Invoke, Path, Resource, TryMapped};
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
    ui_core::widget::ToggleGroup => usize, |control| control.with_enabled(false);
    ui_core::widget::PictureChoice => usize, |control| control.with_enabled(false);
    // Unavailable, a swatch grid is left unbound, which chooses nothing.
    ui_core::widget::ColorSwatchPicker => usize, |control| control;
    ui_core::KeyboardMapSelection => String, |control| control.with_enabled(false);
    // Unavailable, a recorder records nothing it could commit.
    ui_core::widget::ShortcutRecorder => String, |control| control;
    Button => (), |control| control.with_enabled(false);
    // Unavailable, a chip is left unbound, which makes it inert.
    ui_core::widget::Chip => (), |control| control;
    // A row that does something when chosen (a service to link).
    ui_core::widget::ListRow => (), |control| control.with_enabled(false);
    // Unavailable, a card is left unbound, which makes it inert.
    ui_core::widget::MediaCard => (), |control| control;
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
    record_opened::<P>(true);
}

/// The app opened records of `P` beside its own in the room it lives in
/// (the parts of a chain): a surface report lists their surface too, so
/// every member decides a change to them against it as against the app's
/// own record.
pub(crate) fn opened_in_room<P: Resource>() {
    record_opened::<P>(false);
}

fn record_opened<P: Resource>(person: bool) {
    OPENED.with(|opened| {
        if let Some(opened) = opened.borrow_mut().as_mut()
            && !opened.iter().any(|surface| surface.kind == P::KIND)
        {
            opened.push(tilo_resource::Surface {
                person,
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
    kind: &'static str,
    resource: Rc<RefCell<Published>>,
    /// Another record than the app's own, published at this id, whose
    /// actions a live row may offer; `None` for the app's own resource.
    record: Option<Rc<str>>,
    /// The provider keeping that record, which runs its actions
    /// ([`Records::kept_by`](super::Records::kept_by)).
    kept_by: Option<&'static str>,
}

/// What bound controls look up: a publication shared with the root that
/// built it, and the few actions the views bound over it since, which win
/// over the shared ones of the same id.
struct Published {
    base: Rc<tilo_actions::ResourceSpec>,
    overlay: Vec<tilo_actions::ActionSpec>,
}

impl Published {
    /// The published spec of the action `key` names, or why this publication
    /// offers none: one of another kind is another record's, and one of
    /// another app than `key` names is reached through the app publishing it.
    fn find(&self, key: &ActionKey) -> Result<&tilo_actions::ActionSpec, String> {
        if self.base.kind != key.kind {
            return Err(format!("the resource is a `{}`", self.base.kind));
        }
        if let Some(app) = self.base.app().filter(|app| *app != key.app) {
            return Err(format!("it publishes `{app}`'s actions"));
        }
        self.overlay
            .iter()
            .find(|spec| spec.id == key.action)
            .or_else(|| self.base.actions.iter().find(|spec| spec.id == key.action))
            .ok_or_else(|| "the resource does not publish it".to_owned())
    }
}

impl ActionTable {
    fn new(
        app: &'static str,
        kind: &'static str,
        published: Rc<tilo_actions::ResourceSpec>,
    ) -> Self {
        Self {
            app,
            kind,
            resource: Rc::new(RefCell::new(Published {
                base: published,
                overlay: Vec::new(),
            })),
            record: None,
            kept_by: None,
        }
    }

    /// Route the actions to the record `record`, where it may be changed,
    /// kept by the provider `kept_by` when another provider runs them.
    fn route(&mut self, record: &str, editable: bool, kept_by: Option<&'static str>) {
        self.record = editable.then(|| Rc::from(record));
        self.kept_by = kept_by;
    }

    /// Follow the canonical publication of this same hosted resource.
    pub(crate) fn update(&self, published: tilo_actions::ResourceSpec) {
        self.update_shared(Rc::new(published), Vec::new());
    }

    /// Follow `base`, a publication shared with whoever built it, with
    /// `overlay`'s actions in place of its own of the same id: nothing of
    /// `base` is copied, and the last one is only let go.
    pub(crate) fn update_shared(
        &self,
        base: Rc<tilo_actions::ResourceSpec>,
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

    fn key(&self, id: &str) -> ActionKey {
        ActionKey::new(self.app, self.kind, id)
    }

    /// The published spec of the action `key` names, read as a control opened
    /// in the app `opened_in` names it: a window drawing another app's record
    /// binds it under the app id it publishes that record by, which
    /// `fallback` supplies.
    fn spec_of(&self, key: &ActionKey, fallback: &str) -> Result<tilo_actions::ActionSpec, String> {
        let published = self.resource.borrow();
        match published.find(key) {
            Err(why) if key.app == self.app && key.kind == self.kind => published
                .find(&ActionKey::new(
                    fallback,
                    key.kind.as_str(),
                    key.action.as_str(),
                ))
                .cloned()
                .map_err(|_| why),
            found => found.cloned(),
        }
    }

    /// The binding of action `id` for a control with input `I`, or why
    /// there is none.
    fn binding<I: 'static>(
        &self,
        key: ActionKey,
        arguments: Arguments<I>,
    ) -> Result<ActionBinding<I>, (ActionKey, String)> {
        self.binding_in(key, self.app, arguments)
    }

    /// [`Self::binding`], `key`'s app id read in `opened_in`'s place when the
    /// table was opened for another ([`Self::control_binding_in`]).
    fn binding_in<I: 'static>(
        &self,
        key: ActionKey,
        opened_in: &str,
        arguments: Arguments<I>,
    ) -> Result<ActionBinding<I>, (ActionKey, String)> {
        let spec = self
            .spec_of(&key, opened_in)
            .map_err(|why| (key.clone(), why))?;
        let published = self.exporting(spec.clone());
        ActionBinding::fallible(&published, &spec, arguments)
            .map_err(|error| (key, error.to_string()))
    }

    /// The binding of one fully specified invocation of action `id`, or why
    /// there is none: `key`'s app id read in `opened_in`'s place as
    /// [`Self::binding_in`] reads it, so a button and a typed control bind
    /// the same published action.
    fn invocation(
        &self,
        key: ActionKey,
        opened_in: &str,
        refusal: Option<&str>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionBinding<()>, (ActionKey, String)> {
        if let Some(refusal) = refusal {
            return Err((key, refusal.to_owned()));
        }
        let spec = self
            .spec_of(&key, opened_in)
            .map_err(|why| (key.clone(), why))?;
        let published = self.exporting(spec.clone());
        ActionBinding::static_json(&published, &spec, arguments)
            .map_err(|error| (key, error.to_string()))
    }

    /// [`Self::binding`] for a control being built, where an action the
    /// resource does not publish fails tests and `tilo-app check`: `key`'s
    /// app id read in `opened_in`'s place where a control built from a typed
    /// reference names the app by
    /// [`Resource::APP_ID`](tilo_resource::Resource::APP_ID) and this table
    /// was opened for another app: a window drawing another app's record
    /// binds it under the id that record is published by.
    fn control_binding_in<I: 'static>(
        &self,
        key: ActionKey,
        opened_in: &str,
        arguments: Arguments<I>,
    ) -> Option<ActionBinding<I>> {
        self.binding_in(key, opened_in, arguments)
            .map_err(|(key, why)| unbound_of(self.kind, &format!("`{key}`"), &why))
            .ok()
    }

    /// The binding of the field edit described as `what` for a control with
    /// input `I`, each committed input becoming the edit `edit` makes;
    /// `None`, and a failure in tests, when the resource has no field edits
    /// (`edits`) or the field is read-only.
    fn edit_binding<I: 'static, T: Resource>(
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
        // A field edit is the record's own action, under whatever app the
        // control that binds it names it by
        // ([`Self::control_binding_in`]).
        let spec = match self.spec_of(&self.key(tilo_resource::FIELD_EDIT), T::APP_ID) {
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
    fn set_binding<I: 'static, T: Resource>(
        &self,
        what: &str,
        edits: bool,
        readonly: bool,
        segments: &[tilo_resource::Segment],
        value: Box<dyn Fn(I) -> Result<serde_json::Value, String> + Send + Sync>,
    ) -> Option<ActionBinding<I>> {
        let path = serde_json::to_value(segments).unwrap_or_default();
        self.edit_binding::<I, T>(
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
    fn exporting(&self, spec: tilo_actions::ActionSpec) -> tilo_actions::ResourceSpec {
        let published = &self.resource.borrow().base;
        tilo_actions::ResourceSpec {
            id: published.id.clone(),
            semantic_identity: published.semantic_identity.clone(),
            app_id: published.app_id.clone(),
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
        key: &ActionKey,
        spec: Option<tilo_actions::ActionSpec>,
        arguments: Arguments<I>,
    ) -> Option<ActionBinding<I>> {
        let (resource, spec) = self.effect_publication(key, spec)?;
        ActionBinding::fallible(&resource, &spec, arguments)
            .map_err(|error| unbound_of(&key.kind, &format!("`{key}`"), &error.to_string()))
            .ok()
    }

    /// The binding of one fully specified effect, for a button.
    fn effect_invocation(
        &self,
        key: &ActionKey,
        spec: Option<tilo_actions::ActionSpec>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ActionBinding<()>> {
        let (resource, spec) = self.effect_publication(key, spec)?;
        ActionBinding::static_json(&resource, &spec, arguments)
            .map_err(|error| unbound_of(&key.kind, &format!("`{key}`"), &error.to_string()))
            .ok()
    }

    /// The resource and spec an effect control is bound to, or `None` after
    /// reporting why the view declared no such effect.
    fn effect_publication(
        &self,
        key: &ActionKey,
        spec: Option<tilo_actions::ActionSpec>,
    ) -> Option<(tilo_actions::ResourceSpec, tilo_actions::ActionSpec)> {
        let spec = match spec {
            Some(spec) if spec.id == key.action => spec,
            Some(_) | None => {
                unbound_of(
                    &key.kind,
                    &format!("`{key}`"),
                    "the view declares no such effect",
                );
                return None;
            }
        };
        let resource = self.exporting(spec.clone());
        Some((resource, spec))
    }
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
    pub(crate) fn new(app: &'static str, published: Rc<tilo_actions::ResourceSpec>) -> Self {
        Self {
            table: ActionTable::new(app, R::KIND, published),
            marker: PhantomData,
        }
    }

    /// These actions as another record's, published at `record`: a live row
    /// may offer them ([`Self::operation`]) where it may change it.
    pub(crate) fn routed_to(
        mut self,
        record: &str,
        editable: bool,
        kept_by: Option<&'static str>,
    ) -> Self {
        self.table.route(record, editable, kept_by);
        self
    }

    /// `decl` — a bound field edit ([`tilo_resource::ActionDecl::edit`],
    /// `edit_asking`) or verb of another record — as an operation a live
    /// row offers ([`LiveRow::operation`](super::LiveRow::operation)),
    /// carried out on that record as its person (*this* app's notification
    /// rule on the app's row). `None` for the app's own resource, whose row
    /// edits are [`Publishes::row_edits`](super::Publishes::row_edits),
    /// and where the record is read only here.
    #[must_use]
    pub fn operation(
        &self,
        decl: &tilo_resource::ActionDecl<R>,
    ) -> Option<tilo_actions::ActionSpec> {
        let record = self.table.record.as_deref()?;
        let spec = super::record::route_spec(tilo_resource::declaration_spec(decl), record);
        Some(match self.table.kept_by {
            // The provider keeping the record runs the action this one
            // invokes of it.
            Some(provider) => spec.run_as(tilo_actions::RunBy {
                provider: provider.to_owned(),
                resource: Some(record.to_owned()),
                invokes: AsRef::<tilo_resource::Declaration>::as_ref(decl)
                    .invokes
                    .clone(),
            }),
            None => spec,
        })
    }

    /// The resource's own actions as published now: what a query view's
    /// "In this app" group searches, beside the app's effects.
    #[must_use]
    pub fn published(&self) -> Vec<tilo_actions::ActionSpec> {
        let published = self.table.resource.borrow();
        let mut specs = published.overlay.clone();
        specs.extend(
            published
                .base
                .actions
                .iter()
                .filter(|spec| !published.overlay.iter().any(|over| over.id == spec.id))
                .cloned(),
        );
        specs
    }

    /// The binding of a button that runs `spec`, one of the resource's
    /// published actions or one of its effects, with no arguments; `None`
    /// where it asks for some.
    #[must_use]
    pub fn spec_binding(&self, spec: &tilo_actions::ActionSpec) -> Option<ActionBinding<()>> {
        let resource = self.table.exporting(spec.clone());
        let spec = resource.actions.first()?.clone();
        ActionBinding::static_json(&resource, &spec, serde_json::Map::new())
            .map_err(|error| tracing::debug!(%error, action = %spec.id, "control not bound"))
            .ok()
    }

    /// The binding of a switch that runs `spec`, with no arguments, whichever
    /// way it is flipped: the action itself flips what it controls.
    #[must_use]
    pub fn spec_switch_binding(
        &self,
        spec: &tilo_actions::ActionSpec,
    ) -> Option<ActionBinding<bool>> {
        let resource = self.table.exporting(spec.clone());
        let spec = resource.actions.first()?.clone();
        let arguments: Arguments<bool> = Box::new(|_| Ok(serde_json::Map::new()));
        ActionBinding::fallible(&resource, &spec, arguments)
            .map_err(|error| tracing::debug!(%error, action = %spec.id, "control not bound"))
            .ok()
    }

    /// The id the resource is published at.
    pub(crate) fn resource_id(&self) -> String {
        self.table.resource.borrow().base.id.0.clone()
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
        let key = action.action_key();
        self.table
            .binding(key, Box::new(move |input| action.arguments(input)))
            .ok()
    }

    /// The binding of one fully specified invocation, for a button, or
    /// `None` while the resource does not publish it.
    pub fn invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        let refusal = invoke.refusal().map(str::to_owned);
        self.table
            .invocation(
                self.table.key(invoke.id),
                R::APP_ID,
                refusal.as_deref(),
                invoke.arguments,
            )
            .ok()
    }

    /// [`Self::binding`] for a control being built: an action the resource
    /// does not publish fails tests and `tilo-app check`.
    pub(crate) fn control_binding<I: 'static>(
        &self,
        action: impl Binds<R, I>,
    ) -> Option<ActionBinding<I>> {
        let key = action.action_key();
        self.table.control_binding_in(
            key,
            R::APP_ID,
            Box::new(move |input| action.arguments(input)),
        )
    }

    /// [`Self::invocation`] for a control being built.
    pub(crate) fn control_invocation(&self, invoke: Invoke<R>) -> Option<ActionBinding<()>> {
        let refusal = invoke.refusal().map(str::to_owned);
        let key = self.table.key(invoke.id);
        self.table
            .invocation(key, R::APP_ID, refusal.as_deref(), invoke.arguments)
            .map_err(|(key, why)| unbound_of(R::KIND, &format!("`{key}`"), &why))
            .ok()
    }

    /// The binding of a button that invokes `R`'s action `id` with the
    /// arguments `arguments` makes when it is pressed (a form's values).
    pub(crate) fn deferred_binding(
        &self,
        key: ActionKey,
        arguments: impl Fn() -> Result<serde_json::Map<String, serde_json::Value>, String>
        + Send
        + Sync
        + 'static,
    ) -> Option<ActionBinding<()>> {
        self.table
            .control_binding_in(key, R::APP_ID, Box::new(move |()| arguments()))
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
        self.table.edit_binding::<I, R>(
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
            .set_binding::<I, R>(&what, R::EDITS, readonly, &segments, json_of(value))
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

    /// `button`, bound to submit what `draft` holds through `action`, which
    /// parses it ([`ActionRef::try_map`](tilo_resource::ActionRef::try_map)):
    /// disabled while the draft does not parse, and the draft cleared once
    /// submitted. The row that creates an item from a title typed beside
    /// its button; [`Self::submit_draft`] makes Enter in the field do the
    /// same.
    pub fn bind_draft<A, T, F>(
        &self,
        button: Button,
        draft: &ui_core::LocalState<String>,
        action: TryMapped<R, A, F>,
    ) -> Button
    where
        A: serde::Serialize + 'static,
        T: Into<A>,
        F: Fn(String) -> Option<T> + Send + Sync + 'static,
    {
        self.bind_draft_clearing(button, draft, &[], action)
    }

    /// [`Self::bind_draft`] for a draft with further fields `parse` reads
    /// beside it (a person's role beside their name): they are cleared with
    /// it once submitted.
    pub fn bind_draft_clearing<A, T, F>(
        &self,
        button: Button,
        draft: &ui_core::LocalState<String>,
        others: &[&ui_core::LocalState<String>],
        action: TryMapped<R, A, F>,
    ) -> Button
    where
        A: serde::Serialize + 'static,
        T: Into<A>,
        F: Fn(String) -> Option<T> + Send + Sync + 'static,
    {
        let (key, parse) = draft_arguments(action);
        let mut drafts = vec![draft.clone()];
        drafts.extend(others.iter().map(|other| (*other).clone()));
        self.bind_parsed_draft(button, key, parse, drafts)
    }

    /// The type-erased half of [`Self::bind_draft_clearing`]: one copy for
    /// every action a draft is bound to.
    fn bind_parsed_draft(
        &self,
        button: Button,
        key: ActionKey,
        parse: DraftArguments,
        drafts: Vec<ui_core::LocalState<String>>,
    ) -> Button {
        let Some(draft) = drafts.first().cloned() else {
            return button.unavailable();
        };
        let ready = {
            let parse = std::sync::Arc::clone(&parse);
            move |text: &String| parse(text.clone()).is_ok()
        };
        let submitted = draft.clone();
        let submit = move |()| {
            let arguments = parse(submitted.get())?;
            for draft in &drafts {
                draft.set(String::new());
            }
            Ok(arguments)
        };
        match self
            .table
            .control_binding_in(key, R::APP_ID, Box::new(submit))
        {
            Some(binding) => button.bind_action(binding).enabled_while(&draft, ready),
            None => button.unavailable(),
        }
    }

    /// `field` (the text field `draft` follows), with Enter submitting what
    /// it holds through `action`, as [`Self::bind_draft`]'s button does:
    /// Enter on text that does not parse does nothing, and a submission
    /// clears the draft.
    pub fn submit_draft<A, T, F>(
        &self,
        field: TextInput,
        draft: &ui_core::LocalState<String>,
        action: TryMapped<R, A, F>,
    ) -> TextInput
    where
        A: serde::Serialize + 'static,
        T: Into<A>,
        F: Fn(String) -> Option<T> + Send + Sync + 'static,
    {
        let (key, parse) = draft_arguments(action);
        self.submit_parsed_draft(field, key, parse, draft.clone())
    }

    /// The type-erased half of [`Self::submit_draft`].
    fn submit_parsed_draft(
        &self,
        field: TextInput,
        key: ActionKey,
        parse: DraftArguments,
        draft: ui_core::LocalState<String>,
    ) -> TextInput {
        let submit = move |text: String| {
            let arguments = parse(text)?;
            draft.set(String::new());
            Ok(arguments)
        };
        match self
            .table
            .control_binding_in(key, R::APP_ID, Box::new(submit))
        {
            Some(binding) => field.on_submit_action(binding),
            None => field,
        }
    }

    /// The binding of effect `effect` for a control with input `I`
    /// (`Backups::RESTORE` for a dropdown of snapshots).
    pub fn effect_binding<E: Effect, I: 'static>(
        &self,
        effect: impl Binds<E, I>,
    ) -> Option<ActionBinding<I>> {
        let key = effect_key::<E, I>(&effect);
        self.table.effect_binding(
            &key,
            E::spec(&key.action),
            Box::new(move |input| effect.arguments(input)),
        )
    }

    /// The binding of one fully specified effect, for a button.
    pub fn effect<E: Effect>(&self, effect: &E) -> Option<ActionBinding<()>> {
        self.table
            .effect_invocation(&effect.key(), E::spec(effect.id()), effect.arguments())
    }

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

/// What a draft's text makes for the action it is bound to, or why it makes
/// nothing: shared by the control's readiness and its submission.
type DraftArguments = std::sync::Arc<
    dyn Fn(String) -> Result<serde_json::Map<String, serde_json::Value>, String> + Send + Sync,
>;

/// `action`'s key and its parse of a draft's text, erased so the controls
/// bound to drafts share one copy of their code.
fn draft_arguments<R, A, T, F>(action: TryMapped<R, A, F>) -> (ActionKey, DraftArguments)
where
    R: Resource,
    A: serde::Serialize + 'static,
    T: Into<A>,
    F: Fn(String) -> Option<T> + Send + Sync + 'static,
{
    let key = Binds::<R, String>::action_key(&action);
    (key, std::sync::Arc::new(move |text| action.arguments(text)))
}

/// The key the effect a control is bound to is offered under: the one its
/// reference names, and for a vocabulary's [`Effect`] alone, the one
/// [`Effect::key`] reads.
fn effect_key<E: Effect, I>(effect: &impl Binds<E, I>) -> ActionKey {
    ActionKey::new(E::APP_ID, E::OPERATION, effect.action_id())
}

fn json_of<V: tilo_resource::Model, I>(
    value: impl Fn(I) -> Result<V, String> + Send + Sync + 'static,
) -> Box<dyn Fn(I) -> Result<serde_json::Value, String> + Send + Sync> {
    Box::new(move |input| value(input).map(|value| value.to_json()))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every value conversion has"
)]
fn same<V>(value: V) -> Result<V, String> {
    Ok(value)
}
