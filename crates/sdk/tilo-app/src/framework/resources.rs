//! Resources a view edits besides the one its app shows: the person's own
//! record of a resource ([`PersonResource`](super::PersonResource)) and a typed
//! settings section ([`Setting`](super::Setting)).
//!
//! Such a record is a resource like the app's own. A view binds its fields by
//! path and its verbs by reference in a scope of its own
//! ([`ViewCx::record`](super::ViewCx::record)); a surface binds its controls
//! and live rows with the record's actions
//! ([`SurfaceCx::resource_actions`](super::SurfaceCx::resource_actions)).
//! It is published beside the app's resource with its field edits and verbs,
//! so the palette, agents and scripts change it through the same API. Only
//! the person whose record it is changes it (or software acting for them),
//! whatever their role in the workspace the app shows, and every change is
//! recorded in the record's own history: a person record's document history,
//! the settings document's log of who changed which setting.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use settings_model::SettingsSchema;
use tilo_actions::{ActionError, ActionInvoker, EncodedAction, OperationKind};
use tilo_resource::{Fields, Resource};

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
pub(crate) const RESOURCE_ACTION_TYPE: &str = "tilo.app.record-action";

/// One action of a record, as its published operation carries it.
#[derive(Serialize, Deserialize)]
pub(crate) struct ResourceAction {
    pub(crate) record: String,
    pub(crate) action: EncodedAction,
    /// Whether the action invoked declares no answer: what it carries out
    /// (a generic edit carrying a verb that does answer) answers its caller
    /// nothing then.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) quiet: bool,
    /// The published action that declares an answer, when what it carries
    /// out names no verb of its own (a note body's `replace-range`, carried
    /// as an edit of the body): it answers as that action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) answers: Option<String>,
}

type Apply<P> = Box<dyn Fn(&<P as Resource>::Action, &ActionInvoker) -> Result<(), ActionError>>;
type Sync<P> = Box<dyn Fn(&Live<P>)>;
type Read = Box<dyn Fn(&[u8]) -> Result<serde_json::Value, String>>;

/// A record held for the views that edit it: its state as they read it and
/// how an action applies to it.
pub struct Held<P: Resource> {
    id: String,
    label: String,
    editable: bool,
    /// Whether the workspace's grants decide its changes, whoever asks, as they decide the
    /// app's own: a document the app keeps for its whole workspace. Any other is one
    /// person's own, which only they change.
    granted: bool,
    /// The workspace holding the record when it is not the window's ([`Self::held_in`]).
    workspace: Option<String>,
    live: Live<P>,
    sync: Sync<P>,
    apply: Apply<P>,
    /// How a `read` of the record is answered, where its document is held
    /// here ([`Self::reading`]): it is offered then, when `P` reads itself.
    read: Option<Read>,
    /// The last publication, at the record's revision and loads and the
    /// locale's revision it was built at: published again only when one
    /// moved.
    published: RefCell<Option<(Published, tilo_actions::ResourceSpec)>>,
    /// [`Self::publish_head`] as last built, as `published` is.
    published_head: RefCell<Option<(Published, tilo_actions::ResourceSpec)>>,
    /// What the record publishes of what its type publishes, where that is
    /// not all of it ([`Self::publishing`]).
    publishes: Option<Box<dyn Fn(tilo_actions::ResourceSpec) -> tilo_actions::ResourceSpec>>,
}

/// The record's revision and loads ([`Live::loads`]), the locale's
/// revision, and why the record did not read.
type Published = (u64, u64, u64, Option<String>);

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
            granted: false,
            workspace: None,
            live,
            sync: Box::new(sync),
            apply: Box::new(apply),
            read: None,
            published: RefCell::new(None),
            published_head: RefCell::new(None),
            publishes: None,
        }
    }

    /// The record publishing `publishes` of what its type publishes: a
    /// settings section, set field by field by its setters and by no
    /// generic edit.
    pub(crate) fn publishing(
        mut self,
        publishes: impl Fn(tilo_actions::ResourceSpec) -> tilo_actions::ResourceSpec + 'static,
    ) -> Self {
        self.publishes = Some(Box::new(publishes));
        self
    }

    /// The record, whose committed document `document` lends, read as every
    /// record an app publishes is: the record that holds it offers `read`
    /// when `P` reads itself.
    pub(crate) fn reading(
        mut self,
        document: impl Fn(&mut dyn FnMut(&ui_core::automerge::AutoCommit)) + 'static,
    ) -> Self {
        self.read = Some(Box::new(move |payload| {
            let mut answered = Err("the record cannot be read".to_owned());
            let kind = tilo_resource::ResourceKind::of::<P>();
            document(&mut |doc| answered = read_resource(&kind, doc, payload));
            answered
        }));
        self
    }

    /// The same document, its changes decided by the workspace's grants on the app that
    /// keeps it instead of being left to one person.
    pub(crate) fn decided_by_grants(mut self) -> Self {
        self.granted = true;
        self
    }

    /// The record is held in `workspace`, which decides its changes, rather than in the
    /// window's: published as such ([`tilo_actions::ResourceSpec::workspace`]).
    pub(crate) fn held_in(mut self, workspace: Option<String>) -> Self {
        self.workspace = workspace;
        self
    }

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
    /// through [`RESOURCE_ACTION_TYPE`], open to the person whatever their
    /// role in the workspace; nothing to change where it is read only.
    pub(crate) fn publish(&self) -> tilo_actions::ResourceSpec {
        let revision = self.published_at();
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published = self.finish(self.build());
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    /// [`Self::publish`] without its children, built without routing or keeping them: what a
    /// table of the resource's own actions reads, which a resource of many items (an issue list)
    /// answers without carrying each item's actions.
    pub(crate) fn publish_head(&self) -> tilo_actions::ResourceSpec {
        let revision = self.published_at();
        if let Some((at, head)) = self.published_head.borrow().as_ref()
            && *at == revision
        {
            return head.clone();
        }
        let mut head = self.build();
        head.children = Vec::new();
        let head = self.finish(head);
        *self.published_head.borrow_mut() = Some((revision, head.clone()));
        head
    }

    /// What a publication is built at, read now: the same answer is the same publication.
    fn published_at(&self) -> Published {
        self.sync();
        // Reading it again changes no revision — the state stays the last
        // one that read — so whether it reads is part of what the
        // publication is for.
        (
            self.live.revision(),
            self.live.loads(),
            ui_core::i18n::I18nManager::revision(),
            self.live.unreadable().map(|why| why.reason),
        )
    }

    /// The publication as its resource type builds it, and as the record
    /// publishes it, before it is routed here.
    fn build(&self) -> tilo_actions::ResourceSpec {
        let built = self.built();
        match &self.publishes {
            Some(publishes) => publishes(built),
            None => built,
        }
    }

    /// The publication as its resource type builds it.
    fn built(&self) -> tilo_actions::ResourceSpec {
        if let Some(state) = self.live.get_checked() {
            tilo_resource::publish::<P>(&self.id, &self.label, &state)
        } else {
            // Read here, never meant to change here: say why its actions do
            // not apply rather than publish a record that reads as empty.
            let why = self
                .live
                .unreadable()
                .map_or_else(|| "the record does not read".to_owned(), |why| why.reason);
            tilo_resource::resource::unreadable_parts::<P>(&self.id, &self.label, &why)
        }
    }

    /// `published` carried out here: its actions routed to this resource, and the read offered.
    fn finish(&self, published: tilo_actions::ResourceSpec) -> tilo_actions::ResourceSpec {
        let mut published = if self.granted {
            route_granted(published, &self.id, self.editable)
        } else {
            route(published, &self.id, self.editable)
        };
        published.workspace.clone_from(&self.workspace);
        // Read whether it is changed here or not: whoever may act on it
        // reads what it holds.
        if P::READABLE && self.read.is_some() && self.live.unreadable().is_none() {
            published
                .actions
                .push(tilo_resource::read::resource_spec(&self.id));
        }
        published
    }
}

/// `resource`'s document actions, and those of its children, carried out on
/// the record `record`; nothing else it publishes is answered here.
pub(crate) fn route(
    mut resource: tilo_actions::ResourceSpec,
    record: &str,
    editable: bool,
) -> tilo_actions::ResourceSpec {
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

/// [`route`] for what an app keeps for its whole workspace, published as `id`: each action
/// needs what it declares, so the workspace's grants decide it instead of its being left to
/// one person.
fn route_granted(
    mut resource: tilo_actions::ResourceSpec,
    id: &str,
    editable: bool,
) -> tilo_actions::ResourceSpec {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter(|spec| editable && spec.kind == OperationKind::Document)
        .map(|spec| route_operation(spec, RESOURCE_ACTION_TYPE, id))
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| route_granted(child, id, editable))
        .collect();
    resource
}

/// The document action `spec` carried out on the record `record`.
pub(crate) fn route_spec(spec: tilo_actions::ActionSpec, record: &str) -> tilo_actions::ActionSpec {
    let _phase = tilo_resource::profiling::Phase::start("app.route_spec");
    route_operation(spec, RESOURCE_ACTION_TYPE, record)
}

/// The document action `spec` carried out as the operation `operation` on
/// the record `record`, needing what it declares.
pub(crate) fn route_operation(
    spec: tilo_actions::ActionSpec,
    operation: &'static str,
    record: &str,
) -> tilo_actions::ActionSpec {
    let record = record.to_owned();
    let quiet = matches!(
        spec.action_type().contract.result,
        tilo_actions::ResultSpec::None
    );
    let answers = (!quiet).then(|| spec.id.clone());
    spec.routed(operation, move |action| {
        let routed = ResourceAction {
            record: record.clone(),
            action,
            quiet,
            answers: answers.clone(),
        };
        EncodedAction::encode_operation(operation, &routed).unwrap_or_else(|error| EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
            payload: error.to_string().into_bytes(),
        })
    })
}

/// A held record with its type erased, as the host publishes and answers
/// it.
pub(crate) trait ResourceApi {
    fn id(&self) -> &str;
    /// Whether this app changes the record; it is read here otherwise.
    fn editable(&self) -> bool;
    /// Whether it is one person's own, which only they change; what the workspace's grants
    /// decide instead answers whoever they admit.
    fn own(&self) -> bool {
        true
    }
    /// Whether `action` is one of this record's: where two records share one id (two types
    /// of settings kept in one place), each takes its own type's actions.
    fn takes(&self, _action: &EncodedAction) -> bool {
        true
    }
    /// Increases whenever what the record holds changed, read now.
    fn revision(&self) -> u64;
    fn publish(&self) -> tilo_actions::ResourceSpec;
    /// [`Self::publish`] without its children: the record's own kind and actions, what a table
    /// of its actions reads.
    fn publish_head(&self) -> tilo_actions::ResourceSpec {
        let mut head = self.publish();
        head.children = Vec::new();
        head
    }
    /// [`Self::publish`] as a shared tree, which a record whose items change a few at a time
    /// (a chain of parts) builds again only where they changed.
    fn publish_shared(&self) -> ui_core::published::SharedResource {
        ui_core::published::SharedResource::from_resource(self.publish())
    }
    /// What search finds in the record beside the app's own: none unless the
    /// record holds what the window's search lists (a chain's items).
    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        Vec::new()
    }
    /// The items `name` names by its id or label, built alone: what a
    /// resource holding its items itself answers a path naming one with,
    /// however many items it holds (a chain, whose items may stand past its
    /// page).
    fn items_named(&self, _name: &str) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }
    /// The item `id` built alone from what keeps it: what an action
    /// addressed to one resolves, however many items the resource holds and
    /// whether or not its published page carries it.
    fn item(&self, _id: &str) -> Option<tilo_actions::ResourceSpec> {
        None
    }
    /// Answer a read of `item`, which names nothing the resource holds but
    /// something the kind derives over what it keeps beside it (a mail
    /// folder, read as the messages of it every part holds): `None` for a
    /// resource that derives nothing, whose read answers as it would have.
    fn read_over(&self, _item: &str) -> Option<Result<serde_json::Value, String>> {
        None
    }
    /// How many items the resource keeps past the page its publication
    /// carries, across the parts it is kept in: what a listing of it reaches
    /// past that page, counted without building them.
    fn past_page_len(&self) -> usize {
        0
    }
    /// [`Self::past_page_len`] from `offset` on, at most `take`, each built
    /// alone: what a page of a listing past the published page costs.
    fn past_page_window(&self, _offset: usize, _take: usize) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }
    /// Whether a page of the resource's listing reads what is not here yet, `filled` when what
    /// it holds filled the page: a chain's parts not all in, the one a page reaches asked for.
    /// Nothing for a resource held whole.
    fn listing_arriving(&self, _filled: bool) -> bool {
        false
    }
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
    /// [`Self::apply_answering`] in the window's pass over the record it
    /// shows, lent as `live`: what an API that writes that record too (a
    /// container listing a new child) writes it through. Its refusal is the
    /// app's own, so one a caller decides on keeps its code
    /// ([`tilo_resource::Refusal::coded`]): an answer saying the thing is
    /// there already reaches whoever asked as that code.
    fn apply_in_refused(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
        live: &mut ui_core::widget::Edits<'_>,
    ) -> Result<tilo_actions::ActionOutput, tilo_resource::Refusal> {
        let _ = live;
        self.apply_answering_refused(action, invoker)
    }

    /// [`Self::apply_answering`], keeping the refusal the app gave.
    fn apply_answering_refused(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, tilo_resource::Refusal> {
        self.apply_answering(action, invoker)
            .map_err(tilo_resource::Refusal::new)
    }

    /// What a read of this API's record answers, asked as `payload`: a
    /// record reads what it holds ([`tilo_resource::read::TYPE`]), a chain
    /// of parts what its parts hold ([`super::chain::CHAIN_READ_TYPE`]). A
    /// record that answers no reads says so.
    fn read(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        let _ = payload;
        Err(format!("`{}` answers no reads of its parts", self.id()))
    }
}

impl<P: Resource> ResourceApi for Held<P> {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        self.editable
    }

    fn own(&self) -> bool {
        !self.granted
    }

    fn revision(&self) -> u64 {
        self.sync();
        self.live.revision()
    }

    fn publish(&self) -> tilo_actions::ResourceSpec {
        Held::publish(self)
    }

    fn publish_head(&self) -> tilo_actions::ResourceSpec {
        Held::publish_head(self)
    }

    fn read(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        let read = self
            .read
            .as_ref()
            .ok_or_else(|| format!("{} answers no reads here", self.label))?;
        read(payload)
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
        not_by_app::<P>(&action)?;
        self.apply(&action, invoker)
            .map_err(|error| error.to_string())
    }
}

/// Refuse `action` when only the app's own code applies it
/// ([`Resource::by_app`]): an API takes what a caller may send, and this is
/// no caller's, however it came to be encoded.
pub(crate) fn not_by_app<P: Resource>(action: &P::Action) -> Result<(), String> {
    if P::by_app(action) {
        return Err(tilo_resource::by_app_refusal(P::LABEL));
    }
    Ok(())
}

/// Who a change made now is by: the invoker the host delivered the
/// invocation being carried out for (an agent, a Shortcut, an app, or a
/// person over the broker), else `me`, the person at this window. Every write
/// path reads it, so an agent's change is the agent's in whichever record
/// it lands, and names the person it acts for.
/// Whether `invoker` is the person `me` themselves, or a device of theirs,
/// rather than software acting on their behalf: whose step the window's Undo
/// takes back.
pub(crate) fn acts_for(invoker: &ActionInvoker, me: &tilo_resource::MemberId) -> bool {
    let by_agent = invoker
        .acting
        .by
        .member_id()
        .is_some_and(tilo_resource::MemberId::is_agent);
    !by_agent
        && (invoker.acting.by.member_id() == Some(me)
            || me
                .person_id()
                .is_some_and(|person| invoker.acting.acted_for_person() == Some(person)))
}

pub(crate) fn acting(me: &tilo_resource::MemberId) -> ActionInvoker {
    ui_core::invocation_scope::current_invoker()
        .unwrap_or_else(|| ActionInvoker::member(me.clone()))
}

/// Answer one published action of a record: apply it to the record it
/// names, for the person whose record it is.
pub(crate) fn answer(
    apis: &[Rc<dyn ResourceApi>],
    me: Option<&tilo_resource::MemberId>,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    let request: ResourceAction =
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
    let api = apis
        .iter()
        .find(|api| api.id() == request.record && api.takes(&request.action))
        .ok_or_else(|| format!("no record `{}` is open here", request.record))?;
    if !api.own() {
        // Whoever asks: the workspace's grants decide it where the document applies it.
        let invoker = ui_core::invocation_scope::current_invoker()
            .or_else(|| me.map(|me| ActionInvoker::member(me.clone())))
            .ok_or("this window changes nothing for nobody")?;
        api.apply_encoded(&request.action, &invoker)?;
        return Ok(tilo_actions::ActionOutput::default());
    }
    let me = me.ok_or("this window changes no record of anyone's")?;
    let invoker = acting(me);
    let theirs = invoker.acting.by.member_id() == Some(me)
        || me
            .person_id()
            .is_some_and(|person| invoker.acting.acted_for_person() == Some(person));
    if !theirs {
        return Err(format!(
            "`{}` is {me}'s own; only they change it",
            request.record
        ));
    }
    api.apply_encoded(&request.action, &invoker)?;
    Ok(tilo_actions::ActionOutput::default())
}

/// A `read` of the record of `kind` whose committed document is `doc`, asked as
/// `payload`: what it holds through its own read, or from its model tables.
fn read_resource(
    kind: &tilo_resource::ResourceKind,
    doc: &ui_core::automerge::AutoCommit,
    payload: &[u8],
) -> Result<serde_json::Value, String> {
    let content = |doc: &ui_core::automerge::AutoCommit, item: Option<&str>| {
        kind.content(doc, item).map_err(|error| error.to_string())
    };
    super::root::answer_read(doc, payload, &|item| content(doc, item), &content)
}

/// A record other than the app's own that views and surfaces edit: the
/// person's own record of `P` ([`PersonResource`](super::PersonResource)), or
/// a settings section `P` that is a resource model
/// ([`Setting`](super::Setting)).
pub trait OtherResource<P: Resource>: sealed::Hold<P> {}

impl<P: Resource, H: sealed::Hold<P>> OtherResource<P> for H {}

pub(crate) mod sealed {
    use std::rc::Rc;

    use tilo_resource::Resource;

    /// How a record is held for the window's views.
    pub trait Hold<P: Resource> {
        /// The record as a view part over it edits it through its type.
        fn held(&self, storage: Storage<'_>) -> Option<Rc<super::Held<P>>>;
        /// The record as the API publishes and changes it.
        fn api(&self, storage: Storage<'_>) -> Option<Api>;
    }

    /// A held record as the API publishes and changes it.
    pub struct Api(pub(crate) Rc<dyn super::ResourceApi>);

    /// The window's storage, as a record is held in it.
    pub struct Storage<'a>(pub(crate) &'a super::ViewStorage);
}

/// The records an app keeps besides the one it shows, declared in
/// [`ResourceApp::records`](super::ResourceApp::records): each is opened
/// with the window and published beside the app's resource, so the palette
/// and agents reach it whether or not a view has shown it, and the app's
/// committed surface lists it. The person's own shared records are declared
/// with the app instead
/// ([`ResourceApp::PERSON_RESOURCES`](super::ResourceApp::PERSON_RESOURCES)).
pub struct Resources<'a> {
    /// The window's storage, where they are opened; `None` while the app's
    /// records are only declared ([`Self::declaring`]).
    storage: Option<&'a ViewStorage>,
    /// The app's own record, as `Live` of its resource: what the actions of
    /// a chain kept beside it read where they are invoked.
    base: Option<Rc<dyn std::any::Any>>,
    /// The API of the app's own record, as the `Dispatcher` of its
    /// resource: what a container of children it lists writes it through.
    dispatch: Option<Rc<dyn std::any::Any>>,
    /// The record the window shows: the app's own record of the workspace,
    /// or the one per member where the app keeps those, which a chain hangs
    /// off ([`Chained::chain_beside`](super::Chained::chain_beside)).
    record: Option<ui_core::resource::DocumentId>,
    /// The resource types of the chains declared, while declaring.
    chains: Vec<tilo_resource::ResourceKind>,
    /// Where a host without a window keeps the settings sections declared,
    /// with the app they are the settings of ([`super::SettingsResources`]).
    sections: Option<Sections>,
    /// The provider keeping the records being declared, inside
    /// [`Self::kept_by`].
    provider: Option<&'static str>,
    /// What [`Self::kept_by`] declared, with the provider keeping it.
    kept: Vec<(&'static str, Declare)>,
}

/// A declaration of records ([`ResourceApp::records`](super::ResourceApp::records)).
pub type Declare = fn(&mut Resources<'_>);

/// Where a host without a window holds the settings sections declared.
pub(crate) struct Sections {
    /// The app whose settings they are.
    pub(crate) app_id: &'static str,
    /// The settings document they are stored in.
    pub(crate) document: ui_core::storage::SettingsDocument,
    /// The person they are changed by: each invoker's steps on them are
    /// published to undo and redo beside them.
    pub(crate) person: Option<Rc<super::person::PersonState>>,
    pub(crate) apis: Vec<Rc<super::section::SectionApi>>,
}

impl<'a> Resources<'a> {
    pub(crate) fn new(storage: &'a ViewStorage) -> Self {
        Self {
            storage: Some(storage),
            base: None,
            dispatch: None,
            record: None,
            chains: Vec::new(),
            sections: None,
            provider: None,
            kept: Vec::new(),
        }
    }

    /// The records an app declares, opening none: what a host without a
    /// window learns of them (the resource types of its chains' parts, so
    /// it reads a part as the window does).
    pub(crate) const fn declaring() -> Self {
        Self {
            storage: None,
            base: None,
            dispatch: None,
            record: None,
            chains: Vec::new(),
            sections: None,
            provider: None,
            kept: Vec::new(),
        }
    }

    /// The settings sections an app declares, held where `sections` says;
    /// nothing else it declares is opened.
    pub(crate) const fn sections(sections: Sections) -> Self {
        Self {
            storage: None,
            base: None,
            dispatch: None,
            record: None,
            chains: Vec::new(),
            sections: Some(sections),
            provider: None,
            kept: Vec::new(),
        }
    }

    /// The settings sections held, after [`Self::sections`].
    pub(crate) fn into_sections(self) -> Vec<Rc<super::section::SectionApi>> {
        self.sections
            .map(|sections| sections.apis)
            .unwrap_or_default()
    }

    /// The resource types of the chains declared.
    pub(crate) fn into_chains(self) -> Vec<tilo_resource::ResourceKind> {
        self.chains
    }

    /// What was declared kept by another provider ([`Self::kept_by`]),
    /// with the provider keeping each: what a host in place of those
    /// providers (the native test hosts do) serves.
    #[must_use]
    pub fn into_kept(self) -> Vec<(&'static str, Declare)> {
        self.kept
    }

    /// The records `declare` declares, kept by the host as `provider`
    /// (the session keeps the person's settings sections): the window holds
    /// and shows them and its views bind their controls to them as usual,
    /// but each change is `provider`'s action, invoked there through the
    /// workspace's broker and admitted, run and recorded there as any
    /// caller's. The window publishes none of them; `provider` does.
    pub fn kept_by(&mut self, provider: &'static str, declare: Declare) -> &mut Self {
        self.kept.push((provider, declare));
        let outer = self.provider.replace(provider);
        declare(self);
        self.provider = outer;
        self
    }

    pub(crate) fn with_base<R: Resource>(mut self, live: Live<R>, dispatch: Dispatcher<R>) -> Self {
        self.base = Some(Rc::new(live));
        self.dispatch = Some(Rc::new(dispatch));
        self
    }

    /// The record the window shows, which a chain hangs off
    /// ([`Chained::chain_beside`](super::Chained::chain_beside)): the app's
    /// own record of the workspace, or the one per member where the app
    /// keeps those.
    pub(crate) fn at(mut self, record: Option<&ui_core::resource::DocumentId>) -> Self {
        self.record = record.cloned();
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
        let _ = storage.chain::<P>(self.record.as_ref(), move || base.get());
        self
    }

    /// The children of `C` the app's record lists
    /// ([`Children`](super::Children)): each a record of its own, held and
    /// published with the window.
    pub fn children<C: super::ChildResource>(&mut self) -> &mut Self {
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
        let children = storage.children::<C>(move || parent.get());
        if let (Some(children), Some(api)) = (
            children,
            self.dispatch
                .clone()
                .and_then(|api| api.downcast::<Dispatcher<C::Parent>>().ok()),
        ) {
            children.write_parent_through((*api).clone());
        }
        self
    }

    /// Declare the app's record of `P` in the person's profile
    /// ([`ResourceEnv::profile_resource`](super::ResourceEnv::profile_resource)).
    pub fn profile_resource<P: Resource>(&mut self) -> &mut Self {
        let Some(storage) = self.storage else {
            // Declared, not opened: a host that reads the record without the
            // window ([`super::resource_index`]) reads it by its kind.
            self.chains.push(tilo_resource::ResourceKind::of::<P>());
            return self;
        };
        let _ = storage.profile_resource::<P>();
        self
    }

    /// The record `P` that every app of the workspace opens and this app keeps
    /// (`collab_types::WORKSPACE_DOCUMENTS`; the workspace's emoji are
    /// Chat's): opened with the window, so the app's surface lists it and
    /// every member's change to it is decided against that surface.
    pub fn shared_workspace<P: Resource>(&mut self) -> &mut Self {
        let Some(storage) = self.storage else {
            // Declared, not opened: a host that reads the record without the
            // window ([`super::resource_index`]) reads it by its kind.
            self.chains.push(tilo_resource::ResourceKind::of::<P>());
            return self;
        };
        storage.workspace_resource::<P>();
        self
    }

    /// An additional document `P` that this app keeps in the workspace.
    pub fn workspace_record<P: Resource>(&mut self, id: &str) -> &mut Self {
        if let Some(storage) = self.storage {
            storage.workspace_record::<P>(id);
        } else {
            self.chains.push(tilo_resource::ResourceKind::of::<P>());
        }
        self
    }

    /// Records of `P` another program of the device keeps for this app in the workspace under
    /// ids of their own (Terminal's environments, which the shell runtime writes): part of the
    /// app's surface, so the workspace takes them as the app's, and never opened with the
    /// window.
    pub fn kept_records<P: Resource>(&mut self) -> &mut Self {
        super::controls::opened_resource::<P>();
        if self.storage.is_none() {
            self.chains.push(tilo_resource::ResourceKind::of::<P>());
        }
        self
    }

    /// The settings section `T`, a resource model ([`ResourceEnv::settings`](super::ResourceEnv::settings)).
    pub fn settings<T>(&mut self) -> &mut Self
    where
        T: Resource<State = T> + Fields + SettingsSchema,
    {
        // A setting that means nothing on this device is not published.
        if !T::available_on(settings_model::DeviceClass::current()) {
            return self;
        }
        if let Some(sections) = &mut self.sections {
            let settings = ui_core::storage::Settings::<T>::for_app(sections.app_id)
                .in_document(sections.document.clone());
            let kind = super::section::SectionKind::of(&settings);
            sections.apis.push(match &sections.person {
                Some(person) => person.open_section(kind),
                None => Rc::new(super::section::SectionApi::new(kind, None, Vec::new())),
            });
        }
        let Some(storage) = self.storage else {
            return self;
        };
        if self.provider.is_some() {
            super::controls::kept_section(T::KIND);
        }
        let settings = storage.storage().settings::<T>();
        let api = storage.section(&settings);
        if let Some(provider) = self.provider {
            storage.keep_by(api.id(), provider);
        }
        self
    }
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// A part of the view over another record than the app's own — the
    /// person's profile record ([`ResourceEnv::profile_resource`](super::ResourceEnv::profile_resource)), a settings section
    /// ([`ResourceEnv::settings`](super::ResourceEnv::settings)) — built like a view of that record: its
    /// controls bind the record's paths
    /// (`cx.checkbox("high-contrast", Appearance::paths().high_contrast)`)
    /// and verbs, its edits go to that record as the person, recorded in its
    /// history, and the part follows every change of it. `None` where the
    /// record cannot be held (a view of a past version).
    pub fn record<P: Resource>(
        &self,
        record: &impl OtherResource<P>,
        part: impl FnOnce(&mut ViewCx<P>) -> Element,
    ) -> Option<Element> {
        let held = record.held(sealed::Storage(&self.storage))?;
        let dispatch = record_dispatch(&self.dispatch, &held);
        let actions = resource_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            Rc::clone(&held) as Rc<dyn ResourceApi>,
        );
        let mut cx = ViewCx::<P> {
            env: super::env::ResourceEnv {
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
            row_mount: None,
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
fn resource_actions<P: Resource>(
    app: &'static str,
    storage: &ViewStorage,
    alive: std::rc::Weak<()>,
    api: Rc<dyn ResourceApi>,
) -> Actions<P> {
    let actions = Actions::new(app, Rc::new(api.publish_head())).routed_to(
        api.id(),
        api.editable(),
        storage.kept_provider(api.id()),
    );
    follow_publication(storage, alive, api, actions.table());
    actions
}

/// Keep `table` current with `api`'s publication while `alive` lives.
fn follow_publication(
    storage: &ViewStorage,
    alive: std::rc::Weak<()>,
    api: Rc<dyn ResourceApi>,
    table: super::controls::ActionTable,
) {
    let followed = Cell::new(api.revision());
    storage.follow_with(alive, move || {
        let revision = api.revision();
        if followed.replace(revision) != revision {
            table.update(api.publish_head());
        }
    });
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// The published actions of another record than the app's own, for
    /// controls a view builds outside a part over that record
    /// ([`Self::record`]): a hand widget bound to a settings section's field
    /// edits. `None` where the record cannot be held.
    pub fn resource_actions<P: Resource>(
        &self,
        record: &impl OtherResource<P>,
    ) -> Option<Actions<P>> {
        let api = record.api(sealed::Storage(&self.storage))?.0;
        Some(resource_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            api,
        ))
    }
}

impl<R: Resource> SurfaceCx<R> {
    /// The published actions of another record than the app's own — the
    /// person's own record, a settings section: a hand surface binds its
    /// controls to the record's verbs and field edits with them, as to its
    /// own, submits through [`Actions`]' bindings, and offers them on its
    /// live rows ([`Actions::operation`]). `None` where the record cannot be
    /// held.
    pub fn resource_actions<P: Resource>(
        &self,
        record: &impl OtherResource<P>,
    ) -> Option<Actions<P>> {
        let api = record.api(sealed::Storage(&self.storage))?.0;
        Some(resource_actions(
            self.dispatch.app_id(),
            &self.storage,
            self.alive.clone(),
            api,
        ))
    }
}

/// `parent`'s API, submitting to the held record `held`: what applies now
/// applies to it as the person.
fn record_dispatch<R: Resource, P: Resource>(
    parent: &Dispatcher<R>,
    held: &Rc<Held<P>>,
) -> Dispatcher<P> {
    let applying = Rc::downgrade(held);
    let me = parent.me().cloned();
    parent.for_resource(held.id(), !held.editable(), move |action| {
        let held = applying
            .upgrade()
            .ok_or_else(|| ActionError::Refused("the record is closed".into()))?;
        let me = me
            .as_ref()
            .ok_or_else(|| ActionError::Refused("nobody edits a past version".into()))?;
        held.apply(action, &acting(me))
    })
}
