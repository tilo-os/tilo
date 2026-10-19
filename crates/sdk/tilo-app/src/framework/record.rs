//! Records a view edits besides the one its app shows: the person's own
//! record of a resource ([`PersonRecord`](super::PersonRecord)) and a typed
//! settings section ([`Setting`](super::Setting)).
//!
//! Such a record is a resource like the app's own. A view binds its fields by
//! path and its verbs by reference in a scope of its own
//! ([`ViewCx::record`](super::ViewCx::record)); a surface binds its controls
//! and live rows with the record's actions
//! ([`SurfaceCx::record_actions`](super::SurfaceCx::record_actions)).
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
    ActionError, ActionInvoker, ActionKind, ActionNeed, EncodedAction, OperationKind,
};
use tilo_resource::{Fields, Resource};
use ui_core::storage::SettingsSchema;

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
pub(crate) struct RecordAction {
    pub(crate) record: String,
    pub(crate) action: EncodedAction,
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
    /// The last publication, at the record's and the locale's revision it
    /// was built at: published again only when either moved.
    published: RefCell<Option<((u64, u64), tilo_actions::Resource)>>,
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
            published: RefCell::new(None),
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
        let revision = (self.live.revision(), ui_core::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let state = self.live.get();
        let published = route(
            tilo_resource::publish::<P>(&self.id, &self.label, &state),
            &self.id,
            self.editable,
        );
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }
}

/// `resource`'s document actions, and those of its children, carried out on
/// the record `record`; nothing else it publishes is answered here.
pub(crate) fn route(
    mut resource: tilo_actions::Resource,
    record: &str,
    editable: bool,
) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter(|spec| editable && spec.kind == OperationKind::Document)
        .map(|spec| route_spec(spec, record))
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| route(child, record, editable))
        .collect();
    resource
}

/// The document action `spec` carried out on the record `record`.
pub(crate) fn route_spec(spec: tilo_actions::ActionSpec, record: &str) -> tilo_actions::ActionSpec {
    route_operation(spec, RECORD_ACTION_TYPE, record)
        // The room's roles do not reach a record of the person's own: the
        // host lets only its person change it.
        .of_kind(ActionKind::Read)
        .with_need_of(|_| ActionNeed::of(ActionKind::Read))
}

/// The document action `spec` carried out as the operation `operation` on
/// the record `record`, needing what it declares.
pub(crate) fn route_operation(
    spec: tilo_actions::ActionSpec,
    operation: &'static str,
    record: &str,
) -> tilo_actions::ActionSpec {
    let record = record.to_owned();
    spec.routed(operation, move |action| {
        let routed = RecordAction {
            record: record.clone(),
            action,
        };
        EncodedAction::encode_operation(operation, &routed).unwrap_or_else(|error| EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
            payload: error.to_string().into_bytes(),
        })
    })
}

/// A held record with its type erased, as the host publishes and answers
/// it.
pub(crate) trait RecordDoor {
    fn id(&self) -> &str;
    /// Whether this app changes the record; it is read here otherwise.
    fn editable(&self) -> bool;
    /// Increases whenever what the record holds changed, read now.
    fn revision(&self) -> u64;
    fn publish(&self) -> tilo_actions::Resource;
    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String>;
    /// [`Self::apply_encoded`], answering its invoker with what it made.
    fn apply_answering(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.apply_encoded(action, invoker)
            .map(|()| tilo_actions::ActionOutput::default())
    }
}

impl<P: Resource> RecordDoor for Held<P> {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        self.editable
    }

    fn revision(&self) -> u64 {
        self.sync();
        self.live.revision()
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
            tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
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
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
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
        /// The record as a view part over it edits it through its type.
        fn held(&self, storage: Storage<'_>) -> Option<Rc<super::Held<P>>>;
        /// The record as the door publishes and changes it.
        fn door(&self, storage: Storage<'_>) -> Option<Door>;
    }

    /// A held record as the door publishes and changes it.
    pub struct Door(pub(crate) Rc<dyn super::RecordDoor>);

    /// The window's storage, as a record is held in it.
    pub struct Storage<'a>(pub(crate) &'a super::ViewStorage);
}

/// The records an app keeps besides the one it shows, declared in
/// [`ResourceApp::records`](super::ResourceApp::records): each is opened
/// with the window and published beside the app's resource, so the palette
/// and agents reach it whether or not a view has shown it, and the app's
/// committed surface lists it.
pub struct Records<'a> {
    /// The window's storage, where they are opened; `None` while the app's
    /// records are only declared ([`Self::declaring`]).
    storage: Option<&'a ViewStorage>,
    /// The app's own record, as `Live` of its resource: what the actions of
    /// a chain kept beside it read where they are invoked.
    base: Option<Rc<dyn std::any::Any>>,
    /// The resource types of the chains declared, while declaring.
    chains: Vec<tilo_resource::ResourceKind>,
}

impl<'a> Records<'a> {
    pub(crate) fn new(storage: &'a ViewStorage) -> Self {
        Self {
            storage: Some(storage),
            base: None,
            chains: Vec::new(),
        }
    }

    /// The records an app declares, opening none: what a host without a
    /// window learns of them (the resource types of its chains' parts, so
    /// it reads a part as the window does).
    pub(crate) const fn declaring() -> Self {
        Self {
            storage: None,
            base: None,
            chains: Vec::new(),
        }
    }

    /// The resource types of the chains declared.
    pub(crate) fn into_chains(self) -> Vec<tilo_resource::ResourceKind> {
        self.chains
    }

    pub(crate) fn with_base<R: Resource>(mut self, live: Live<R>) -> Self {
        self.base = Some(Rc::new(live));
        self
    }

    /// The chain of `P` the app keeps beside its record
    /// ([`Chain`](super::Chain)): its head is held and it is published
    /// with the window.
    pub fn chain<P: super::Chained>(&mut self) -> &mut Self {
        let Some(storage) = self.storage else {
            self.chains.push(tilo_resource::ResourceKind::of::<P>());
            return self;
        };
        let Some(base) = self
            .base
            .clone()
            .and_then(|base| base.downcast::<Live<P::Base>>().ok())
        else {
            tracing::warn!(
                kind = P::KIND,
                "a chain's parts read the app's own resource as their base"
            );
            return self;
        };
        let _ = storage.chain::<P>(move || base.get());
        self
    }

    /// The children of `C` the app's record lists
    /// ([`Children`](super::Children)): each a record of its own, held and
    /// published with the window.
    pub fn children<C: super::ChildRecord>(&mut self) -> &mut Self {
        let Some(storage) = self.storage else {
            return self;
        };
        let Some(parent) = self
            .base
            .clone()
            .and_then(|base| base.downcast::<Live<C::Parent>>().ok())
        else {
            tracing::warn!(
                kind = C::KIND,
                "children read the app's own resource as their parent"
            );
            return self;
        };
        let _ = storage.children::<C>(move || parent.get());
        self
    }

    /// The person's own record of `P` ([`RecordEnv::person`](super::RecordEnv::person)).
    pub fn person<P: Resource>(&mut self) -> &mut Self {
        let Some(storage) = self.storage else {
            return self;
        };
        let _ = storage.person::<P>();
        self
    }

    /// The person's record `P` another app keeps
    /// ([`RecordEnv::shared_person`](super::RecordEnv::shared_person)).
    pub fn shared_person<P: tilo_resource::SharedRecord>(&mut self) -> &mut Self {
        let Some(storage) = self.storage else {
            return self;
        };
        let _ = storage.shared_person::<P>();
        self
    }

    /// The settings section `T`, a resource model ([`RecordEnv::settings`](super::RecordEnv::settings)).
    pub fn settings<T>(&mut self) -> &mut Self
    where
        T: Resource<State = T> + Fields + SettingsSchema,
    {
        let Some(storage) = self.storage else {
            return self;
        };
        let settings = storage.storage().settings::<T>();
        let _ = storage.section(&settings);
        self
    }
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// A part of the view over another record than the app's own — the
    /// person's own record ([`RecordEnv::person`](super::RecordEnv::person)), a settings section
    /// ([`RecordEnv::settings`](super::RecordEnv::settings)) — built like a view of that record: its
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
        let dispatch = record_dispatch(&self.dispatch, &held);
        let actions = record_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            Rc::clone(&held) as Rc<dyn RecordDoor>,
        );
        let mut cx = ViewCx::<P> {
            env: super::env::RecordEnv {
                live: held.live().clone(),
                alive: self.alive.clone(),
                dispatch,
                saved: self.saved.clone(),
                saves: self.saves.clone(),
                run: self.run.clone(),
                storage: Rc::clone(&self.storage),
                effects: Effects::inert(),
            },
            focus: Rc::new(|state| Some(state)),
            updaters: Updaters::new(),
            gate: None,
            action_bindings: actions,
            item: None,
            path: Some(Vec::new()),
            controls: Rc::clone(&self.controls),
            forms: Forms::default(),
            stages: self.stages.clone(),
            feeds: HistoryFeeds::default(),
            bindings: Bindings::new(),
        };
        let element = part(&mut cx);
        let updaters = cx.updaters;
        let followed = Cell::new(held.live().revision());
        self.storage.follow_with(self.alive.clone(), move || {
            held.sync();
            let revision = held.live().revision();
            if followed.replace(revision) != revision {
                updaters.run(&held.live().get(), &Change::Everything);
            }
        });
        Some(element)
    }
}

/// The published actions of `held`, kept current with the record while
/// `alive` lives: what a record's view part binds, and a surface's own
/// controls and live rows ([`Actions::operation`]).
fn record_actions<P: Resource>(
    app: &'static str,
    storage: &ViewStorage,
    alive: std::rc::Weak<()>,
    door: Rc<dyn RecordDoor>,
) -> Actions<P> {
    let actions = Actions::new(app, Rc::new(door.publish())).routed_to(door.id(), door.editable());
    follow_publication(storage, alive, door, actions.table());
    actions
}

/// Keep `table` current with `door`'s publication while `alive` lives.
fn follow_publication(
    storage: &ViewStorage,
    alive: std::rc::Weak<()>,
    door: Rc<dyn RecordDoor>,
    table: super::controls::ActionTable,
) {
    let followed = Cell::new(door.revision());
    storage.follow_with(alive, move || {
        let revision = door.revision();
        if followed.replace(revision) != revision {
            table.update(door.publish());
        }
    });
}

impl<R: Resource> SurfaceCx<R> {
    /// The published actions of another record than the app's own — the
    /// person's own record, a settings section: a hand surface binds its
    /// controls to the record's verbs and field edits with them, as to its
    /// own, submits through [`Actions`]' bindings, and offers them on its
    /// live rows ([`Actions::operation`]). `None` where the record cannot be
    /// held.
    pub fn record_actions<P: Resource>(&self, record: &impl OtherRecord<P>) -> Option<Actions<P>> {
        let door = record.door(sealed::Storage(&self.storage))?.0;
        Some(record_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            door,
        ))
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
