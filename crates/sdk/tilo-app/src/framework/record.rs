//! Records a view edits besides the one its app shows: the person's own
//! record of a resource ([`PersonRecord`](super::PersonRecord)) and a typed
//! settings section ([`Setting`](super::Setting)).
//!
//! Such a record is a resource like the app's own. A view binds its fields by
//! path and its verbs by reference in a scope of its own
//! ([`ViewCx::record`](super::ViewCx::record)); a surface submits through its
//! dispatcher ([`SurfaceCx::record_dispatcher`](super::SurfaceCx::record_dispatcher)).
//! It is published beside the app's resource with its field edits and verbs,
//! so the palette, agents and scripts change it through the same door. Only
//! the person whose record it is changes it (or software acting for them),
//! whatever their role in the room the app shows, and every change is
//! recorded in the record's own history: a person record's document history,
//! the settings document's log of who changed which setting.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action as _, ActionError, ActionInvoker, EncodedAction, OperationKind, PermissionRequirement,
};
use tilo_resource::{Doc, Fields, Reader, Resource};
use ui_core::storage::{ChangeOrigin, Settings, SettingsSchema};

use super::controls::Actions;
use super::dispatch::Dispatcher;
use super::effect::Effects;
use super::form::Forms;
use super::history::HistoryFeeds;
use super::live::Live;
use super::reactive::{Change, Updaters};
use super::settings::ViewStorage;
use super::surface::SurfaceCx;
use super::view::{Bindings, ViewCx};
use ui_core::widget::Element;

/// The operation type a record's published actions are carried out under:
/// the host applies the action to the record it names.
pub(crate) const RECORD_ACTION_TYPE: &str = "tilo.app.record-action";

/// One action of a record, as its published operation carries it.
#[derive(Serialize, Deserialize)]
struct RecordAction {
    record: String,
    action: EncodedAction,
}

type Apply<P> = Box<dyn Fn(&<P as Resource>::Action, &ActionInvoker) -> Result<(), ActionError>>;
type Sync<P> = Box<dyn Fn(&Live<P>)>;

/// A record held for the views that edit it: its state as they read it and
/// how an action applies to it.
pub struct Held<P: Resource> {
    id: String,
    label: String,
    editable: bool,
    live: Live<P>,
    sync: Sync<P>,
    apply: Apply<P>,
}

impl<P: Resource> Held<P> {
    pub(crate) fn new(
        id: String,
        label: String,
        editable: bool,
        live: Live<P>,
        sync: impl Fn(&Live<P>) + 'static,
        apply: impl Fn(&P::Action, &ActionInvoker) -> Result<(), ActionError> + 'static,
    ) -> Self {
        Self {
            id,
            label,
            editable,
            live,
            sync: Box::new(sync),
            apply: Box::new(apply),
        }
    }

    /// The id the record is published under.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn editable(&self) -> bool {
        self.editable
    }

    /// Its state, as views read it.
    pub(crate) fn live(&self) -> &Live<P> {
        &self.live
    }

    /// Follow what changed the record: this window, another, another of
    /// the person's devices.
    pub(crate) fn sync(&self) {
        (self.sync)(&self.live);
        // The record's views follow it whole: it is small.
        let _ = self.live.take_change();
    }

    /// Apply `action` for `invoker`.
    ///
    /// # Errors
    /// The record is read here only, or the action refused.
    pub(crate) fn apply(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        if !self.editable {
            return Err(ActionError::Refused(format!(
                "{} is changed in another app",
                self.label
            )));
        }
        (self.apply)(action, invoker)?;
        self.sync();
        Ok(())
    }

    /// The record as it is published: its field edits and verbs carried out
    /// through [`RECORD_ACTION_TYPE`], open to the person whatever their
    /// role in the room; nothing to change where it is read only.
    pub(crate) fn publish(&self) -> tilo_actions::Resource {
        self.sync();
        let state = self.live.get();
        let published = tilo_resource::publish::<P>(&self.id, &self.label, &state);
        route(published, &self.id, self.editable)
    }
}

/// `resource`'s document actions, and those of its children, carried out on
/// the record `record`; nothing else it publishes is answered here.
fn route(
    mut resource: tilo_actions::Resource,
    record: &str,
    editable: bool,
) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter(|spec| editable && spec.kind == OperationKind::Document)
        .map(|spec| {
            let record = record.to_owned();
            spec.routed(RECORD_ACTION_TYPE, move |action| {
                let routed = RecordAction {
                    record: record.clone(),
                    action,
                };
                EncodedAction::encode_operation(RECORD_ACTION_TYPE, &routed).unwrap_or_else(
                    |error| EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
                        payload: error.to_string().into_bytes(),
                    },
                )
            })
            // The room's roles do not reach a record of the person's own:
            // the host lets only its person change it.
            .requiring(PermissionRequirement::Read)
            .with_permission_of(|_| PermissionRequirement::Read)
        })
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| route(child, record, editable))
        .collect();
    resource
}

/// A held record with its type erased, as the host publishes and answers
/// it.
pub(crate) trait RecordDoor {
    fn id(&self) -> &str;
    fn publish(&self) -> tilo_actions::Resource;
    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String>;
}

impl<P: Resource> RecordDoor for Held<P> {
    fn id(&self) -> &str {
        &self.id
    }

    fn publish(&self) -> tilo_actions::Resource {
        Held::publish(self)
    }

    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String> {
        if action.type_id != P::ACTION_TYPE {
            return Err(format!(
                "{} takes no `{}` action",
                self.label, action.type_id
            ));
        }
        let action: P::Action =
            serde_json::from_slice(&action.payload).map_err(|error| error.to_string())?;
        self.apply(&action, invoker)
            .map_err(|error| error.to_string())
    }
}

/// Answer one published action of a record: apply it to the record it
/// names, for the person whose record it is.
///
/// # Errors
/// The payload names no record of this window, someone else invoked it, or
/// the action refused.
pub(crate) fn answer(
    doors: &[Rc<dyn RecordDoor>],
    me: Option<&str>,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    let request: RecordAction =
        serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    let door = doors
        .iter()
        .find(|door| door.id() == request.record)
        .ok_or_else(|| format!("no record `{}` is open here", request.record))?;
    let me = me.ok_or("this window changes no record of anyone's")?;
    let invoker =
        ui_core::invocation_scope::current_invoker().unwrap_or_else(|| ActionInvoker::actor(me));
    let theirs = invoker.actor == me || invoker.on_behalf_of.as_deref() == Some(me);
    if !theirs {
        return Err(format!(
            "`{}` is {me}'s own; only they change it",
            request.record
        ));
    }
    door.apply_encoded(&request.action, &invoker)?;
    Ok(tilo_actions::ActionOutput::default())
}

/// A record other than the app's own that views and surfaces edit: the
/// person's own record of `P` ([`PersonRecord`](super::PersonRecord)), or
/// a settings section `P` that is a resource model
/// ([`Setting`](super::Setting)).
pub trait OtherRecord<P: Resource>: sealed::Hold<P> {}

impl<P: Resource, H: sealed::Hold<P>> OtherRecord<P> for H {}

pub(crate) mod sealed {
    use std::rc::Rc;

    use tilo_resource::Resource;

    /// How a record is held for the window's views.
    pub trait Hold<P: Resource> {
        fn held(&self, storage: Storage<'_>) -> Option<Rc<super::Held<P>>>;
    }

    /// The window's storage, as a record is held in it.
    pub struct Storage<'a>(pub(crate) &'a super::ViewStorage);
}

/// The records an app keeps besides the one it shows, declared in
/// [`ResourceApp::records`](super::ResourceApp::records): each is opened
/// with the window and published beside the app's resource, so the palette
/// and agents reach it whether or not a view has shown it, and the app's
/// committed surface lists it.
pub struct Records<'a> {
    storage: &'a ViewStorage,
}

impl<'a> Records<'a> {
    pub(crate) fn new(storage: &'a ViewStorage) -> Self {
        Self { storage }
    }

    /// The person's own record of `P` ([`ViewCx::person`]).
    pub fn person<P: Resource>(&mut self) -> &mut Self {
        let _ = self.storage.person::<P>();
        self
    }

    /// The person's record `P` another app keeps
    /// ([`ViewCx::shared_person`]).
    pub fn shared_person<P: tilo_resource::SharedRecord>(&mut self) -> &mut Self {
        let _ = self.storage.shared_person::<P>();
        self
    }

    /// The settings section `T`, a resource model ([`ViewCx::settings`]).
    pub fn settings<T>(&mut self) -> &mut Self
    where
        T: Resource<State = T> + Fields + SettingsSchema,
    {
        let settings = self.storage.storage().settings::<T>();
        let _ = self.storage.section(&settings);
        self
    }
}

/// The settings section `T` held as a record: kept as the resource model in
/// a document of its own, each change applied there, checked, and stored in
/// the settings document with who made it.
pub(crate) fn section<T>(settings: Settings<T>, editable: bool) -> Result<Held<T>, String>
where
    T: Resource<State = T> + SettingsSchema + Fields,
{
    let id = format!("settings:{}", T::KIND);
    let mut doc = Doc::new();
    T::initialize(&mut doc).map_err(|error| error.to_string())?;
    settings
        .get()
        .write_fields(&mut doc, &tilo_resource::__private::ROOT)
        .map_err(|error| error.to_string())?;
    doc.commit();
    let live = Live::new(Reader::<T>::read(&doc).map_err(|error| error.to_string())?);
    let doc = Rc::new(RefCell::new(doc));
    let seen = Rc::new(Cell::new(settings.revision()));
    let following = settings.clone();
    let (followed, read) = (Rc::clone(&doc), Rc::clone(&seen));
    let sync = move |live: &Live<T>| {
        let mut doc = followed.borrow_mut();
        let revision = following.revision();
        if revision != read.get() {
            read.set(revision);
            // Another window, the Settings app, another device: the stored
            // value is the truth.
            if following
                .get()
                .write_fields(&mut doc, &tilo_resource::__private::ROOT)
                .is_ok()
            {
                doc.commit();
            } else {
                doc.rollback();
            }
        }
        if let Err(error) = live.sync(&doc) {
            tracing::warn!(%error, kind = T::KIND, "settings section unreadable");
        }
    };
    let apply = move |action: &T::Action, invoker: &ActionInvoker| {
        let mut doc = doc.borrow_mut();
        let id = action_id(action);
        let changed = action.apply(&mut doc).and_then(|()| {
            let value = T::read_fields(&doc, &tilo_resource::__private::ROOT)
                .map_err(|error| ActionError::Refused(error.to_string()))?;
            let origin =
                ChangeOrigin::action(format!("{}/{id}", T::KIND)).by(invoker.actor.clone());
            settings
                .set_by(&value, Some(&origin))
                .map_err(|error| ActionError::Refused(error.to_string()))
        });
        match changed {
            Ok(()) => {
                doc.commit();
                seen.set(settings.revision());
                Ok(())
            }
            Err(error) => {
                doc.rollback();
                Err(error)
            }
        }
    };
    Ok(Held::new(
        id,
        T::LABEL.to_owned(),
        editable,
        live,
        sync,
        apply,
    ))
}

/// The public id of the action `action` is, as its encoded form names it.
pub(crate) fn action_id<A: Serialize>(action: &A) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| {
            let operation = value.get("op").unwrap_or(&value);
            operation
                .get("action")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "action".to_owned())
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// A part of the view over another record than the app's own — the
    /// person's own record ([`ViewCx::person`]), a settings section
    /// ([`ViewCx::settings`]) — built like a view of that record: its
    /// controls bind the record's paths
    /// (`cx.checkbox("high-contrast", Appearance::paths().high_contrast)`)
    /// and verbs, its edits go to that record as the person, recorded in its
    /// history, and the part follows every change of it. `None` where the
    /// record cannot be held (a view of a past version).
    pub fn record<P: Resource>(
        &self,
        record: &impl OtherRecord<P>,
        part: impl FnOnce(&mut ViewCx<P>) -> Element,
    ) -> Option<Element> {
        let held = record.held(sealed::Storage(&self.storage))?;
        let dispatch = self.record_dispatch(&held);
        let mut cx = ViewCx::<P> {
            live: held.live().clone(),
            focus: Rc::new(|state| Some(state)),
            updaters: Updaters::new(),
            alive: self.alive.clone(),
            dispatch,
            action_bindings: Actions::new(self.dispatch.app_id(), Rc::new(held.publish())),
            item: None,
            path: Some(Vec::new()),
            controls: Rc::clone(&self.controls),
            forms: Forms::default(),
            saved: self.saved.clone(),
            stages: self.stages.clone(),
            feeds: HistoryFeeds::default(),
            run: self.run.clone(),
            storage: Rc::clone(&self.storage),
            bindings: Bindings::new(),
            effects: Effects::inert(),
        };
        let element = part(&mut cx);
        let (updaters, actions) = (cx.updaters, cx.action_bindings);
        let followed = Cell::new(held.live().revision());
        self.storage.follow_with(self.alive.clone(), move || {
            held.sync();
            let revision = held.live().revision();
            if followed.replace(revision) != revision {
                actions.update(held.publish());
                updaters.run(&held.live().get(), &Change::Everything);
            }
        });
        Some(element)
    }

    /// The dispatcher of another record than the app's own, for code that
    /// submits to it outside a [`Self::record`] part. `None` where the
    /// record cannot be held.
    pub fn record_dispatcher<P: Resource>(
        &self,
        record: &impl OtherRecord<P>,
    ) -> Option<Dispatcher<P>> {
        record
            .held(sealed::Storage(&self.storage))
            .map(|held| self.record_dispatch(&held))
    }

    fn record_dispatch<P: Resource>(&self, held: &Rc<Held<P>>) -> Dispatcher<P> {
        record_dispatch(&self.dispatch, held)
    }
}

/// The published actions of `held`, for controls a surface or a view binds
/// itself, kept current with the record while `alive` lives.
fn record_actions<P: Resource>(
    app: &'static str,
    storage: &ViewStorage,
    alive: std::rc::Weak<()>,
    held: Rc<Held<P>>,
) -> Actions<P> {
    let actions = Actions::new(app, Rc::new(held.publish()));
    let following = actions.clone();
    let followed = Cell::new(held.live().revision());
    storage.follow_with(alive, move || {
        held.sync();
        let revision = held.live().revision();
        if followed.replace(revision) != revision {
            following.update(held.publish());
        }
    });
    actions
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// The published actions of another record than the app's own, for
    /// controls built outside a [`Self::record`] part: `bind` its verbs,
    /// `edit_binding` its paths. `None` where the record cannot be held.
    pub fn record_actions<P: Resource>(&self, record: &impl OtherRecord<P>) -> Option<Actions<P>> {
        let held = record.held(sealed::Storage(&self.storage))?;
        Some(record_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            held,
        ))
    }
}

impl<R: Resource> SurfaceCx<R> {
    /// The published actions of another record than the app's own, as
    /// [`ViewCx::record_actions`]: a hand surface binds its controls to the
    /// record's verbs and field edits with them, as to its own.
    pub fn record_actions<P: Resource>(&self, record: &impl OtherRecord<P>) -> Option<Actions<P>> {
        let held = record.held(sealed::Storage(&self.storage))?;
        Some(record_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            held,
        ))
    }

    /// The dispatcher of another record than the app's own, as
    /// [`ViewCx::record_dispatcher`]: a surface reads the record itself
    /// ([`PersonRecord::with`](super::PersonRecord::with),
    /// [`Setting::get`](super::Setting::get)) and submits its field edits
    /// and verbs here.
    pub fn record_dispatcher<P: Resource>(
        &self,
        record: &impl OtherRecord<P>,
    ) -> Option<Dispatcher<P>> {
        record
            .held(sealed::Storage(&self.storage))
            .map(|held| record_dispatch(&self.dispatch, &held))
    }
}

/// `parent`'s door, submitting to the held record `held`: what applies now
/// applies to it as the person.
fn record_dispatch<R: Resource, P: Resource>(
    parent: &Dispatcher<R>,
    held: &Rc<Held<P>>,
) -> Dispatcher<P> {
    let applying = Rc::downgrade(held);
    let me = parent.me().map(str::to_owned);
    parent.for_record(held.id(), !held.editable(), move |action| {
        let held = applying
            .upgrade()
            .ok_or_else(|| ActionError::Refused("the record is closed".into()))?;
        let me = me
            .as_deref()
            .ok_or_else(|| ActionError::Refused("nobody edits a past version".into()))?;
        held.apply(action, &ActionInvoker::actor(me))
    })
}
