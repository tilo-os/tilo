//! A workspace's tilolang resources as a session service: every root and item a loaded package
//! holds is a session resource, read through [`Service::read`] and changed through its actions.
//!
//! Nothing here is per app. The catalogue is the workspace's published trees, kept by the
//! incremental republisher ([`Publications`]), with each package's own actions beside them,
//! exactly as an app's instance publishes them on its connection. A resource's reading is its
//! fields as the runtime reads them, each by its field id, encoded as the runtime encodes a
//! value ([`tilolang_runtime::value::to_json`]); a reference in it names the resource it points
//! at by its published id (`tl:<app>:<id>`) and its type as `<app>#<Type>`. That is what a
//! runtime that loads the package as another app's
//! ([`tilolang_runtime::Runtime::load_foreign_package`]) reads back
//! ([`tilolang_runtime::Runtime::set_reading`]): the shell composes a note's own view over the
//! note's reading, and the reading is the resource the notes app shows.
//!
//! Every resource lives in the device's one resource store, which the host's runtime reads and
//! writes ([`crate::resources::ResourceStore`]). An invocation is the published action's own:
//! the API builds it through the action's spec, which runs it in the runtime as the invoker's
//! step, and what it wrote is sent to the store when the run ends. What the workspace wrote
//! elsewhere reaches the store, which tells this service through its watch.
//!
//! Each published root carries the history verbs every app's resource answers: its versions are
//! listed from the store, and an invocation is taken back by the store's undo, here as in an
//! app's instance, so a resource no app's process holds still offers its History and its undo.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use collaboration::history_source::verbs::{self, EDIT_TYPE, HistoryEdit, HistoryView, VIEW_TYPE};
use serde_json::{Map, Value as Json};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_service::{Service, Started};
use tilolang_runtime::InvokeOutcome;
use tilolang_runtime::store::{ChangeNote, Mutation, Scalar, Store as _, Version};
use tilolang_runtime::value::{ResourceRef, Value};

use crate::action::{ACTION_TYPE, DynAction, EDIT_FLAG_ACTION, EDIT_TEXT_ACTION};
use crate::host::Host;
use crate::publish::{Planner, Publications, publish_package, resource_id};

/// The history verbs a published root answers here: undo and redo through the store's undo, and
/// its versions as the store keeps them.
const HISTORY_VERBS: [&str; 3] = ["undo", "redo", "history"];

/// How many versions the history verb lists when it is not told.
const HISTORY_LAST: usize = 50;

/// What fills an app's resources in a workspace that holds none of them yet.
type Seed = Box<dyn Fn(&Host) -> Result<(), String>>;

/// The workspace's tilolang resources, published to the session.
pub struct ResourceService {
    /// The runtime the resources are read through, over the device's resource store.
    host: Host,
    planner: Arc<dyn Planner>,
    publications: Publications,
    /// What moved in the store since the last poll: a reading moves with any field of its
    /// resource, also one no published tree shows (a note's body).
    watch: crate::resources::Watch,
    /// Who the changes made here are steps of.
    person: String,
    /// What `now` reads: the system time, or what a test fixes.
    clock: Box<dyn Fn() -> String>,
    /// The resources whose readings moved since [`Service::moved`] was last asked, by their
    /// published ids, and whether something moved that names no one resource (the workspace
    /// moved on elsewhere).
    moved: BTreeSet<String>,
    moved_unsaid: bool,
    /// The packages the session runs itself (the shell), whose resources it keeps beside the
    /// apps': each its compiled module, its description embedded, so a connection reads the
    /// package's resources by its app as it reads any app's.
    shared: BTreeMap<String, Arc<[u8]>>,
    /// The outside services its packages' types with an origin are kept in step with, by
    /// origin (`imap`, `youtube`): each native code this service runs beside the resources,
    /// reached through the connection the session hands it ([`Service::connect`]).
    origins: crate::origin::Origins,
    /// What wakes the session's loop when an origin has an answer and work waits.
    origin_waker: Option<crate::background::Waker>,
    /// The notices this device shows its person, and what it told already
    /// ([`crate::notices::Notices`]).
    notices: crate::notices::Notices,
    /// Where they are shown: the session's notifications. Until it is set, nothing is shown.
    tell: Option<Box<dyn tilo_service::notice::Notices>>,
    /// The places a resource's `open` asked the session for since [`Service::take_open`] last
    /// took them, each with the actor that asked: opening is the session's arranging step, never
    /// a change of the resource.
    opens: Vec<(tilo_actions::ActionInvoker, tilo_service::Open)>,
    /// The place the invocation now running is asked in, by the resource that place shows — the
    /// conversation of the turn an agent asks in — or empty for the place in front: what a
    /// resource it opens stands beside ([`Service::note_beside`]).
    beside: String,
    /// The one app this service publishes ([`Self::of_app`]); `None` for every package it loads.
    only: Option<String>,
    /// The words its host reads in, kept for a host opened again over another store
    /// ([`Self::share`]).
    words: Option<std::rc::Rc<dyn crate::runtime::Words>>,
    /// Whether this service is the session's own ([`Self::session_document`]): the one that
    /// publishes the packages the session runs itself (the shell), started whether or not the
    /// session seeds any app, so a program names the shell the session shows.
    session_document: bool,
    /// What fills the app's resources in a workspace that holds none of them yet
    /// ([`Self::set_seed`]).
    seed: Option<Seed>,
    /// Whether the service reads and writes the workspace's store
    /// ([`Service::keep_in_workspace`]).
    in_workspace: bool,
    /// Where the app's resources open on the web, where this host mints their links
    /// ([`crate::share`]): set by the session that knows the workspace's entry.
    link_home: Option<crate::share::LinkHome>,
}

impl ResourceService {
    /// The resources `host` reads, its packages loaded, published for `person`.
    #[must_use]
    pub fn new(host: Host, person: impl Into<String>) -> Self {
        let person: String = person.into();
        let planner: Arc<dyn Planner> = Arc::new(host.handle());
        let publications = Publications::new(&host);
        let watch = host.store().watch();
        let mut service = Self {
            host,
            planner,
            publications,
            watch,
            notices: crate::notices::Notices::new(person.clone()),
            tell: None,
            person,
            clock: Box::new(crate::host::system_time),
            moved: BTreeSet::new(),
            moved_unsaid: false,
            shared: BTreeMap::new(),
            origins: crate::origin::Origins::default(),
            origin_waker: None,
            opens: Vec::new(),
            beside: String::new(),
            only: None,
            words: None,
            session_document: false,
            seed: None,
            in_workspace: false,
            link_home: None,
        };
        service.publications.update(&service.host, &service.planner);
        let _ = service.watch.take();
        service
    }

    /// The session's own service, which publishes the packages the session runs itself (the
    /// shell, [`Service::share`]): started whether or not the session seeds any app, so the
    /// package the shell actually shows is one a REPL's or an agent's program names, and its
    /// resources are read by its app as Notes' are.
    #[must_use]
    pub fn session_document(host: Host, person: impl Into<String>) -> Self {
        Self {
            session_document: true,
            ..Self::new(host, person)
        }
    }

    /// The resources of the app `app` that `host` reads, its package loaded (with the packages it
    /// is made with), published for `person`: the packages it is made with are its own parts,
    /// which another app's service offers again where it is made with them too.
    #[must_use]
    pub fn of_app(app: impl Into<String>, host: Host, person: impl Into<String>) -> Self {
        Self {
            only: Some(app.into()),
            ..Self::new(host, person)
        }
    }

    /// Mint the links this service answers for its resources under `home`, where the workspace's
    /// host announces a page ([`crate::share`]); without one `share-link` is refused, as an
    /// app's root refuses it.
    pub fn set_link_home(&mut self, home: Option<crate::share::LinkHome>) {
        self.link_home = home;
    }

    /// Fill the app's resources with `seed` where they are kept in a workspace that holds none of
    /// them yet ([`Service::keep_in_workspace`]): a development session's seed data, written once.
    pub fn set_seed(&mut self, seed: impl Fn(&Host) -> Result<(), String> + 'static) {
        self.seed = Some(Box::new(seed));
    }

    /// Read and write `store` from now on, with every package the host runs loaded again over it.
    fn rehost(&mut self, store: crate::resources::ResourceStore) -> Result<(), String> {
        let host = self.host_over(store)?;
        self.install(host);
        Ok(())
    }

    /// A host over `store` with every package this service's host runs loaded again over it.
    fn host_over(&self, store: crate::resources::ResourceStore) -> Result<Host, String> {
        let host = Host::open(store, self.host.viewer().clone(), None, || None)
            .map_err(|error| error.to_string())?;
        if let Some(words) = &self.words {
            host.set_words(std::rc::Rc::clone(words));
        }
        for package in self.host.packages() {
            host.load_module(&package.wasm, package.meta.clone(), package.ids.clone())
                .map_err(|error| format!("{}: {error}", package.meta.app))?;
        }
        Ok(host)
    }

    /// Read and write through `host` from now on.
    fn install(&mut self, host: Host) {
        self.planner = Arc::new(host.handle());
        self.publications = Publications::new(&host);
        self.watch = host.store().watch();
        self.host = host;
    }

    /// Read the resources in `words`.
    pub fn set_words(&mut self, words: std::rc::Rc<dyn crate::runtime::Words>) {
        self.host.set_words(std::rc::Rc::clone(&words));
        self.words = Some(words);
    }

    /// Whether this service publishes the resources and actions of `app`.
    fn offers(&self, app: &str) -> bool {
        self.only.as_deref().is_none_or(|only| only == app) && !self.profile_only(app)
    }

    /// Whether this service keeps `app`'s resources: the app's own service, or the session's for
    /// the packages it runs itself (the shell); the session's loads every package to read it.
    fn answers_for(&self, app: &str) -> bool {
        match &self.only {
            Some(only) => only == app && self.offers(app),
            None if self.session_document => self.shared.contains_key(app),
            None => {
                self.shared.contains_key(app)
                    || self.host.package(app).is_some() && self.offers(app)
            }
        }
    }

    /// Whether `app` keeps its records in each person's profile: the person's own app instance
    /// publishes them, never the workspace's service. The shell's are the session's.
    fn profile_only(&self, app: &str) -> bool {
        self.host
            .package(app)
            .is_some_and(|package| package.meta.uses_personal_profile())
            && !(self.session_document && app == "so.tilo.shell")
    }

    /// Publish the package `share` names, which the session runs itself, with the apps' this
    /// service publishes: its roots are made in the store, and what its parts write there is
    /// published from then on, as the store's watch tells it. A REPL or an agent's task reads and
    /// changes them by the package's app, as it does Notes'.
    ///
    /// Only the session's own service ([`Self::session_document`]) publishes the packages the
    /// session runs itself.
    pub fn share(&mut self, share: &tilo_service::Share<'_>) -> Result<(), String> {
        if !self.session_document || self.only.is_some() {
            return Ok(());
        }
        let Some(meta) = crate::runtime::Meta::from_wasm(share.wasm) else {
            return Err(format!("{} carries no description", share.app));
        };
        // The package's resources are in the store its parts write into: the service reads that
        // one from now on.
        if let Some(store) = share
            .store
            .downcast_ref::<crate::resources::ResourceStore>()
            && !store.same(self.host.store())
        {
            self.rehost(store.clone())?;
        }
        self.host
            .load_module(share.wasm, meta.clone(), String::new())
            .map_err(|error| format!("{}: {error}", meta.app))?;
        for root in &meta.named_roots {
            self.host
                .root_resource(&meta.app, &root.ty)
                .map_err(|error| format!("{}'s root {}: {error}", meta.app, root.name))?;
        }
        self.shared.insert(meta.app, Arc::from(share.wasm));
        self.publications.update(&self.host, &self.planner);
        let _ = self.watch.take();
        Ok(())
    }

    /// The runtime the resources are read through.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Reach the connector of `origin` at the far end of `connection`: the origin that keeps
    /// the packages' types of its kinds in step with the service, native code this service runs
    /// beside the resources, asks the connector over the socket.
    ///
    /// The connector is the process the session started for the app in the sandbox its
    /// manifest declares for the origin: this service asks through it, never holding the
    /// network or the credentials itself.
    #[cfg(unix)]
    pub fn connect_origin(
        &mut self,
        origin: &str,
        connection: std::os::fd::OwnedFd,
    ) -> Result<(), String> {
        let wake = self.origin_waker.clone().unwrap_or_else(|| Arc::new(|| {}));
        let stream = std::os::unix::net::UnixStream::from(connection);
        let shared = crate::origin::remote(origin, stream, wake)?;
        self.origins.borrow_mut().push(shared);
        // What an effect of a package's action hands an origin goes to that origin; anything
        // this service does not carry out itself (the development log's `post`) goes to the
        // log, as a window without a sink does.
        self.host.runtime().set_sink(crate::origin::RoutingSink {
            origins: std::rc::Rc::clone(&self.origins),
            rest: Box::new(crate::sink::LogSink::dev()),
        });
        Ok(())
    }

    /// Have `wake` called when an origin has something for the next [`Service::poll`]: an
    /// answer from the service it asked.
    pub fn set_origin_waker(&mut self, wake: crate::background::Waker) {
        self.origin_waker = Some(wake);
    }

    /// Poll each origin, and carry out the outside effects the packages queued that wait for
    /// it: what it wrote goes to the store as the origin's change and is published. Whether
    /// anything was written.
    fn poll_origins(&mut self) -> bool {
        if self.origins.borrow().is_empty() {
            return false;
        }
        let now = crate::host::system_time();
        let waker = self.origin_waker.clone().unwrap_or_else(|| Arc::new(|| {}));
        let origins: Vec<crate::origin::SharedOrigin> = self.origins.borrow().clone();
        let mut wrote = false;
        // The outside effects a package's action queued, handed to the origin that carries
        // them out: an effect queued since the last pass goes now, whether or not any origin is
        // already due (an origin is due once something is handed to it).
        match self.host.settle_outbox() {
            Ok(settled) if settled.written > 0 => wrote = true,
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "outside effects could not be settled"),
        }
        for origin in &origins {
            if !origin.borrow().due() {
                continue;
            }
            // The app whose types hold this origin's kinds: the package that declares them, so
            // an origin writes the resources of the app that holds them.
            let name = origin.borrow().name().to_owned();
            let prefix = format!("{name}.");
            let app = self
                .host
                .packages()
                .iter()
                .rev()
                .find(|package| {
                    package.meta.types.iter().any(|ty| {
                        ty.origin
                            .as_deref()
                            .is_some_and(|kind| kind.starts_with(&prefix))
                    })
                })
                .map(|package| package.meta.app.clone())
                .unwrap_or_default();
            let cx = crate::origin::OriginCx {
                host: &self.host,
                app: &app,
                waker: &waker,
                now: now.clone(),
            };
            if origin.borrow_mut().poll(&cx).wrote {
                wrote = true;
            }
        }
        if wrote {
            self.publications.update(&self.host, &self.planner);
        }
        wrote
    }

    /// Whether anything moved in the store since the last call. What moved is kept for
    /// [`Service::moved`]: every published resource of a document written in, by its published
    /// id, or that everything did when the workspace moved on elsewhere.
    fn watched(&mut self) -> bool {
        let moved = self.watch.take();
        if moved.is_empty() {
            return false;
        }
        if moved.everything {
            self.moved_unsaid = true;
            return true;
        }
        let mut named = BTreeSet::new();
        for tree in self.publications.trees().values() {
            each_published(tree, &mut |spec| {
                let Some((_, path)) = split_published(spec.id.as_str()) else {
                    return;
                };
                let doc = path.split_once('/').map_or(path, |(doc, _)| doc);
                if moved.docs.contains(doc) {
                    named.insert(spec.id.as_str().to_owned());
                }
            });
        }
        // A resource no published tree shows yet (one just made) names nothing to say: what
        // lists it moved, and is said as everything.
        if named.is_empty() {
            self.moved_unsaid = true;
        }
        self.moved.extend(named);
        true
    }

    /// The direct edit (a bound control's) of the field `arguments` name in the resource
    /// `resource`, as `by`'s change: what a view composed over its reading elsewhere sends when
    /// the person types into it. `None` for a resource that is not one of these packages'.
    /// A flag (`flag`) may come as its text, as an API that knows no parameter type for it
    /// hands it on.
    fn direct_edit(
        &self,
        resource: &str,
        flag: bool,
        arguments: &Map<String, Json>,
        by: &crate::runtime::Actor,
    ) -> Option<Result<(), String>> {
        let (app, path) = split_published(resource)?;
        let package = self.host.package(app)?;
        let at = resource_at(&self.host, app, path)?;
        let declared = package.meta.types.iter().find(|ty| ty.id == at.ty)?;
        let field = arguments
            .get("field")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let mut value = arguments.get("value").cloned().unwrap_or(Json::Null);
        if flag && let Some(said) = value.as_str() {
            match said.parse::<bool>() {
                Ok(set) => value = Json::Bool(set),
                Err(_) => return Some(Err(format!("`{said}` is not a flag"))),
            }
        }
        Some(self.edit_field(&package.meta, declared, &at, field, &value, by))
    }

    /// Set the field `field` (by its name, the spelling the edit verbs offer, or by its id) of
    /// `at`, a resource of the type `declared` of `meta`, to `value`, as `by`'s change.
    fn edit_field(
        &self,
        meta: &crate::runtime::Meta,
        declared: &tilolang::meta::TypeMeta,
        at: &ResourceRef,
        field: &str,
        value: &Json,
        by: &crate::runtime::Actor,
    ) -> Result<(), String> {
        use tilolang::types::MergeKind;
        let declared_field = declared
            .fields
            .iter()
            .find(|known| known.name == field)
            .or_else(|| declared.fields.iter().find(|known| known.id == field))
            .ok_or_else(|| {
                let names: Vec<&str> = declared
                    .fields
                    .iter()
                    .map(|known| known.name.as_str())
                    .collect();
                format!(
                    "the type `{}` has no field `{field}`; its fields are: {}",
                    declared.name,
                    names.join(", ")
                )
            })?;
        let id = declared_field.id.clone();
        let rich = matches!(declared_field.ty.trim(), "RichText" | "std.RichText");
        let mutation = match (declared_field.merge, value) {
            // A rich text comes as its words, a paragraph a line, or as the runtime's encoding
            // of it where it carries more than words.
            (MergeKind::Text, value) if rich => {
                let value = match value {
                    Json::String(text) => tilolang_runtime::rich::RichValue::from_text(text),
                    encoded => match tilolang_runtime::value::from_json(encoded)
                        .map_err(|why| format!("the field `{field}` takes a rich text: {why}"))?
                    {
                        Value::Rich(rich) => rich,
                        _ => return Err(format!("the field `{field}` takes a rich text")),
                    },
                };
                Mutation::SetRich { field: id, value }
            }
            (MergeKind::Text, Json::String(text)) => Mutation::SetText {
                field: id,
                text: text.clone(),
            },
            (MergeKind::Register, value) => Mutation::SetRegister {
                field: id,
                value: scalar_of(&self.host.argument(meta, value, &declared_field.ty)?)?,
            },
            _ => return Err(format!("the field `{field}` cannot be set to that value")),
        };
        let verb = if value.is_boolean() {
            EDIT_FLAG_ACTION
        } else {
            EDIT_TEXT_ACTION
        };
        let mut store = self.host.store().clone();
        let txn = store.begin().map_err(|error| error.to_string())?;
        if let Err(error) = store.apply(txn, at, &mutation) {
            store.rollback(txn);
            return Err(error.to_string());
        }
        store
            .commit(
                txn,
                &ChangeNote {
                    action: verb.to_owned(),
                    args: serde_json::json!([declared_field.id, value]),
                    invoker: Some(by.clone()),
                    touched: vec![at.id_string()],
                    ..ChangeNote::default()
                },
            )
            .map_err(|error| error.to_string())
    }

    /// The link into an app that opens links, asked at `address`: this service runs the app's own
    /// `open_link` with it, as the host of an app's window does, and what it names is opened where
    /// the window shows it, `beside` the place the asking turn is in.
    fn open_link_asked(
        &mut self,
        payload: &[u8],
        address: &ActionAddress,
        invoker: &tilo_actions::ActionInvoker,
        beside: String,
    ) -> Result<Started, ApiError> {
        let arguments: Json = tilo_actions::decode_json(payload)
            .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
        let link = arguments
            .get(tilo_actions::remote::OPEN_LINK_PARAM)
            .and_then(Json::as_str)
            .ok_or_else(|| {
                ApiError::InvalidArguments("open-link needs the link to open".to_owned())
            })?
            .to_owned();
        let app = crate::publish::app_of(&address.resource)
            .ok_or_else(|| ApiError::Unknown(address.clone()))?
            .to_owned();
        let made = self
            .run_open_link(&app, &link, &invoker.acting.by)
            .map_err(ApiError::Failed)?;
        if let Some(published) = &made {
            let open = tilo_service::Open {
                resource: published.clone(),
                view: String::new(),
                beside,
            };
            self.opens.push((invoker.clone(), open));
        }
        Ok(Started::Done(ActionOutput {
            value: None,
            resources: made.into_iter().collect(),
        }))
    }

    /// Run the published `action` of `resource` with `arguments` as `invoker`'s step: the one
    /// path [`Service::invoke`] and [`Service::invoke_as`] share.
    ///
    /// A resource's `open` is not a change of the resource: it is published so a caller — an
    /// agent reading the catalogue, a view offering it — finds it at the resource, and the
    /// session carries it out as its own arranging step, placing the resource with its view
    /// beside the place the asking turn is in. What was asked waits in [`Self::opens`] for the
    /// session's next pass, which places it with the same `open` the layout's own action runs.
    fn run(
        &mut self,
        action: &str,
        resource: &str,
        arguments: &Map<String, Json>,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<Started, ApiError> {
        let address = ActionAddress::new(resource, action);
        if crate::publish::app_of(resource).is_some_and(|app| self.profile_only(app)) {
            return Err(ApiError::Unknown(address));
        }
        // The place this invocation is asked in, taken as this one's: a note from the step that
        // brought it stands for it alone, never for the next invocation the service answers.
        let beside = std::mem::take(&mut self.beside);
        let by = &invoker.acting.by;
        let mut made = Vec::new();
        // What the action writes is told apart from what was written before it.
        let _ = self.host.store().take_wrote("");
        if action == EDIT_TEXT_ACTION || action == EDIT_FLAG_ACTION {
            self.direct_edit(resource, action == EDIT_FLAG_ACTION, arguments, by)
                .ok_or_else(|| ApiError::Unknown(address.clone()))?
                .map_err(ApiError::InvalidArguments)?;
        } else {
            // The action is planned to be carried out at once: it runs once, as the invoker's
            // step, and what it wrote goes to the store when its run ends.
            let resources = self.resources();
            let encoded = self.host.carrying_out(by, || {
                tilo_actions::api::encode_json(&resources, &address, arguments)
            })?;
            if encoded.type_id == crate::effects::OPEN_OPERATION {
                let mut open =
                    open_asked(&encoded.payload, resource).map_err(ApiError::InvalidArguments)?;
                open.beside = beside;
                self.opens.push((invoker.clone(), open));
                return Ok(Started::Done(ActionOutput::default()));
            }
            if tilo_resource::share::handles(&encoded.type_id) {
                // A link is the workspace's to mint, under the page its host announces: this
                // service answers for the resources it publishes, as an app's root does for its
                // own.
                let app = crate::publish::app_of(resource)
                    .ok_or_else(|| ApiError::Unknown(address.clone()))?;
                let output = crate::share::answer(&encoded.payload, app, self.link_home.as_ref())
                    .map_err(ApiError::Failed)?;
                return Ok(Started::Done(output));
            }
            if encoded.type_id == tilo_actions::remote::APPLICATION_OPEN_LINK_TYPE {
                return self.open_link_asked(&encoded.payload, &address, invoker, beside);
            }
            if encoded.type_id == EDIT_TYPE {
                let edit: HistoryEdit = serde_json::from_slice(&encoded.payload)
                    .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
                let value = self
                    .history_edit(&edit, resource, by)
                    .map_err(ApiError::Failed)?;
                self.publications.update(&self.host, &self.planner);
                self.told();
                return Ok(Started::Done(ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                }));
            }
            if encoded.type_id == VIEW_TYPE {
                let view: HistoryView = tilo_actions::decode_json(&encoded.payload)
                    .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
                return self
                    .history_view(&view, resource)
                    .map(|value| {
                        Started::Done(ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        })
                    })
                    .map_err(ApiError::Failed);
            }
            if encoded.type_id != ACTION_TYPE {
                return Err(ApiError::Unknown(address));
            }
            let planned: DynAction = serde_json::from_slice(&encoded.payload)
                .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
            made.extend(
                self.host
                    .take_returned(&planned.action_id)
                    .map(|made| resource_id(&planned.app, &made.id_string())),
            );
        }
        if let Some(refused) = self.host.store().take_refusal() {
            return Err(ApiError::Failed(refused));
        }
        // What it made is published before it is answered, so the caller's next invocation
        // reaches it.
        // The answer names the characters it wrote: the window draws them in the writer's
        // colour, ending at the writer's caret.
        let wrote =
            crate::publish::app_of(resource).and_then(|app| self.host.store().take_wrote(app));
        self.publications.update(&self.host, &self.planner);
        self.told();
        Ok(Started::Done(ActionOutput {
            value: wrote.and_then(|wrote| ActionOutput::noting_wrote(None, &wrote)),
            resources: made,
        }))
    }

    /// Run the app's own `open_link` with `link`, on the root it acts on: what an opened link,
    /// a shell's delivery and an agent all make it do. What it made, by its published id.
    fn run_open_link(
        &self,
        app: &str,
        link: &str,
        by: &tilolang_runtime::ids::Actor,
    ) -> Result<Option<String>, String> {
        let package = self
            .host
            .package(app)
            .ok_or_else(|| format!("`{app}` is not loaded here"))?;
        let target = crate::meta::open_link_root(&package.meta)
            .ok_or_else(|| format!("`{app}` does not open links"))?;
        let action = package
            .meta
            .actions
            .iter()
            .find(|action| !action.scenario && action.name == crate::meta::OPEN_LINK_ACTION)
            .ok_or_else(|| format!("`{app}` does not open links"))?
            .clone();
        let root = self
            .host
            .root_resource(app, &target.id)
            .map_err(|error| error.to_string())?;
        match self.host.runtime().invoke_action(
            by,
            (
                action.id.as_str(),
                vec![Value::resource(&root), Value::text(link)],
            ),
        ) {
            Ok(InvokeOutcome::Done(Value::Ref(made))) => {
                let made = std::rc::Rc::unwrap_or_clone(made.target);
                Ok(Some(resource_id(app, &made.id_string())))
            }
            Ok(InvokeOutcome::Done(_)) => Ok(None),
            Ok(other) => Err(format!("`{link}` was not opened: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Apply the history verb `edit`, invoked on the published root `resource` by `by`: an
    /// invocation taken back by the store's undo, or the last steps written here taken back or
    /// brought back. What the verb did, as its caller is answered.
    fn history_edit(
        &self,
        edit: &HistoryEdit,
        resource: &str,
        by: &crate::runtime::Actor,
    ) -> Result<Json, String> {
        let store = self.host.store();
        let failed = |error: tilolang_runtime::store::StoreError| error.to_string();
        match edit {
            HistoryEdit::Undo {
                invocation: Some(invocation),
                by: None,
                to: None,
                step: None,
                ..
            } => {
                store.undo(invocation, Some(by)).map_err(failed)?;
                Ok(serde_json::json!({ "undone": [invocation] }))
            }
            HistoryEdit::Undo {
                step: Some(step),
                invocation: None,
                by: None,
                to: None,
                ..
            } => {
                let invocation = self
                    .versions_of(resource)?
                    .into_iter()
                    .find(|version| !step.is_empty() && version.change.starts_with(step.as_str()))
                    .and_then(|version| version.gesture)
                    .ok_or_else(|| format!("{resource} has no step {step}"))?;
                store.undo(&invocation, Some(by)).map_err(failed)?;
                Ok(serde_json::json!({ "undone": [invocation] }))
            }
            HistoryEdit::Undo {
                steps,
                invocation: None,
                by: None,
                to: None,
                step: None,
            } => {
                let mut undone = 0_u32;
                while undone < steps.unwrap_or(1) && store.undo_last(Some(by)).map_err(failed)? {
                    undone = undone.saturating_add(1);
                }
                Ok(serde_json::json!({ "undone": undone }))
            }
            HistoryEdit::Redo { steps } => {
                let mut redone = 0_u32;
                while redone < steps.unwrap_or(1) && store.redo_last(Some(by)).map_err(failed)? {
                    redone = redone.saturating_add(1);
                }
                Ok(serde_json::json!({ "redone": redone }))
            }
            HistoryEdit::Undo { .. } => Err(
                "undo takes an invocation or a step from history, or how many of your last \
                 steps"
                    .to_owned(),
            ),
            other => Err(format!("{} is not offered here", other.verb())),
        }
    }

    /// Answer the history view `view` of the published root `resource`: its versions and those
    /// of every resource published in it, oldest first, the last ones it asks for.
    fn history_view(&self, view: &HistoryView, resource: &str) -> Result<Json, String> {
        history(&self.host, &[self.tree(resource)?], view)
    }

    /// The versions of the published root `resource` and of every resource published in it,
    /// oldest first.
    fn versions_of(&self, resource: &str) -> Result<Vec<Version>, String> {
        versions(&self.host, self.tree(resource)?)
    }

    /// The published root `resource`.
    fn tree(&self, resource: &str) -> Result<&ResourceSpec, String> {
        self.publications
            .trees()
            .values()
            .find(|tree| tree.id.as_str() == resource)
            .map(|tree| &**tree)
            .ok_or_else(|| format!("{resource} keeps no history here"))
    }
}

/// Answer the history view `view` of the published resources `trees`, which `host` reads: their
/// versions and those of every resource published in them, oldest first, the last ones it asks
/// for. What an app's window and the session's service both answer the `history` verb with.
///
/// # Errors
/// The view is not a resource's history, or the store did not read a resource's versions.
pub fn history(host: &Host, trees: &[&ResourceSpec], view: &HistoryView) -> Result<Json, String> {
    let HistoryView::History { last } = view else {
        return Err("only the history of a resource is offered here".to_owned());
    };
    let mut versions = Vec::new();
    for tree in trees {
        let app = crate::publish::app_of(tree.id.as_str()).unwrap_or_default();
        versions.extend(
            self::versions(host, tree)?
                .into_iter()
                .map(|version| (app.to_owned(), version)),
        );
    }
    versions.sort_by(|(_, a), (_, b)| a.order().cmp(&b.order()));
    let total = versions.len();
    let last = last.map_or(HISTORY_LAST, |last| usize::try_from(last).unwrap_or(1));
    let listed: Vec<Json> = versions
        .into_iter()
        .skip(total.saturating_sub(last))
        .map(|(app, version)| {
            let published = resource_id(&app, &version.resource);
            let label = history_label(host, &version.resource);
            let mut entry = serde_json::to_value(&version).unwrap_or_default();
            if let Some(entry) = entry.as_object_mut() {
                entry.insert("resource".to_owned(), Json::String(published));
                entry.insert("label".to_owned(), Json::String(label));
            }
            entry
        })
        .collect();
    Ok(serde_json::json!({ "versions": listed, "total": total }))
}

/// What the app's history calls the resource `id` (as [`ResourceRef::id_string`] writes it): an
/// item by its label as published, a package's root by its type ("Notebook"), as the app's name
/// it is published under says nothing in the app's own history.
fn history_label(host: &Host, id: &str) -> String {
    let Some(at) = host
        .store()
        .type_of_id(id)
        .and_then(|ty| ResourceRef::parse_id(id, ty))
    else {
        return String::new();
    };
    match (&at.item, crate::schema::lookup(&at.ty)) {
        (None, Some(schema)) => schema.spoken_name(),
        _ => host.label_at(&at),
    }
}

/// The versions of the published resource `tree`, which `host` reads, and of every resource
/// published in it, oldest first.
///
/// # Errors
/// The store did not read a resource's versions.
pub fn versions(host: &Host, tree: &ResourceSpec) -> Result<Vec<Version>, String> {
    let store = host.store();
    let mut versions = Vec::new();
    let mut failed = None;
    each_published(tree, &mut |spec| {
        let Some((_, path)) = split_published(spec.id.as_str()) else {
            return;
        };
        let Some(ty) = store
            .kinds()
            .of_kind(&spec.kind)
            .map(|entry| entry.id.clone())
        else {
            return;
        };
        let Some(at) = ResourceRef::parse_id(path, ty) else {
            return;
        };
        match store.versions(&at) {
            Ok(held) => versions.extend(held),
            Err(error) => failed = Some(error.to_string()),
        }
    });
    if let Some(error) = failed {
        return Err(error);
    }
    versions.sort_by(|a, b| a.order().cmp(&b.order()));
    Ok(versions)
}

/// The scalar a register holds for `value`.
fn scalar_of(value: &Value) -> Result<Scalar, String> {
    Ok(match value {
        Value::Bool(flag) => Scalar::Bool(*flag),
        Value::Int(number) => Scalar::Int(*number),
        Value::Float(number) => Scalar::Float(*number),
        Value::Text(text) => Scalar::Text(text.clone()),
        other => Scalar::Json(tilolang_runtime::value::to_json(other)?),
    })
}

/// The app and the path in it of the published id `resource` (`tl:<app>:<path>`).
fn split_published(resource: &str) -> Option<(&str, &str)> {
    resource.strip_prefix("tl:")?.split_once(':')
}

/// Call `visit` with `spec` and every resource published in it, depth first.
fn each_published(spec: &ResourceSpec, visit: &mut dyn FnMut(&ResourceSpec)) {
    visit(spec);
    for child in &spec.children {
        each_published(child, visit);
    }
}

/// The resource `path` of the app `app` that `host`'s store holds, typed as the type of `app`
/// it is of: a package's root not made yet counts as held, as it reads as it would start.
/// `None` for a path that names none of the app's resources.
fn resource_at(host: &Host, app: &str, path: &str) -> Option<ResourceRef> {
    let kinds = host.store().kinds();
    let mut types = kinds
        .types()
        .filter(|entry| entry.app == app)
        .map(|entry| entry.id.clone());
    types.find_map(|ty| {
        let at = ResourceRef::parse_id(path, ty)?;
        let unmade_root = at.is_unmade_root() && at.doc == format!("root-{}", at.ty);
        (unmade_root || host.store().contains(&at)).then_some(at)
    })
}

/// The place the `open` effect `payload` names: the resource the invocation addresses — the
/// published id of the root or item whose `open` ran — and the view it shows, empty for the
/// type's primary one.
fn open_asked(payload: &[u8], resource: &str) -> Result<tilo_service::Open, String> {
    use tilo_resource::Effect as _;
    let open =
        crate::effects::OpenEffect::from_payload(payload).map_err(|error| error.to_string())?;
    Ok(tilo_service::Open {
        resource: resource.to_owned(),
        view: open.view().unwrap_or_default().to_owned(),
        beside: String::new(),
    })
}

/// The reading of the root or item `resource` (its published id, `tl:<app>:<id>`) that `host`
/// reads: its fields as the runtime reads them, each by its field id, and what links to it. What a
/// runtime that loads its package as another app's reads back
/// ([`tilolang_runtime::Runtime::set_reading`]). `None` for a resource `host` holds none of.
#[must_use]
pub fn reading_of(host: &Host, resource: &str) -> Option<Json> {
    let (app, path) = split_published(resource)?;
    host.package(app)?;
    let at = resource_at(host, app, path)?;
    let schema = crate::schema::lookup(&at.ty)?;
    let runtime = host.runtime();
    let mut fields = Map::new();
    for field in &schema.fields {
        let Ok(value) = runtime.read(&at, &field.id) else {
            continue;
        };
        let Ok(mut json) = tilolang_runtime::value::to_json(&value) else {
            continue;
        };
        published_refs(&mut json);
        fields.insert(field.id.clone(), json);
    }
    // What the person it is read for has not seen of its items: a view composed elsewhere
    // counts it as this app's own views do ([`tilolang_runtime::store::UNSEEN`]).
    if let Some(unseen) = runtime.unseen_reading(host.viewer(), &at) {
        fields.insert(tilolang_runtime::store::UNSEEN.to_owned(), unseen);
    }
    // What links to it, each with how: a view composed elsewhere draws them as this app's
    // own views do ([`tilolang_runtime::store::LINKS`]), and runs again when they move.
    if let Ok(links) = host.store().linked(&at) {
        let links: Vec<Json> = links
            .into_iter()
            .filter_map(|link| {
                let schema = crate::schema::lookup(&link.of.ty)?;
                Some(Json::Object(Map::from_iter([
                    (
                        "ref".to_owned(),
                        Json::String(resource_id(&schema.app, &link.of.id_string())),
                    ),
                    (
                        "ty".to_owned(),
                        Json::String(format!("{}#{}", schema.app, schema.name)),
                    ),
                    ("how".to_owned(), Json::String(link.how.word().to_owned())),
                ])))
            })
            .collect();
        if !links.is_empty() {
            fields.insert(
                tilolang_runtime::store::LINKS.to_owned(),
                Json::Array(links),
            );
        }
    }
    Some(Json::Object(fields))
}

/// Each reference in `json` named as the session publishes it: by its published id, and its
/// type as `<app>#<Type>`.
fn published_refs(json: &mut Json) {
    match json {
        Json::Object(fields) => {
            let named = fields
                .get("ref")
                .and_then(Json::as_str)
                .zip(fields.get("ty").and_then(Json::as_str))
                .and_then(|(id, ty)| {
                    let schema = crate::schema::lookup(ty)?;
                    Some((
                        resource_id(&schema.app, id),
                        format!("{}#{}", schema.app, schema.name),
                    ))
                });
            if let Some((id, ty)) = named {
                fields.insert("ref".into(), Json::String(id));
                fields.insert("ty".into(), Json::String(ty));
            }
            for nested in fields.values_mut() {
                published_refs(nested);
            }
        }
        Json::Array(items) => items.iter_mut().for_each(published_refs),
        _ => {}
    }
}

impl ResourceService {
    /// Run what the packages have due at this device's clock: a trigger runs here whether a
    /// window of its app is open or not, once per instant across the workspace's devices (the
    /// store keeps what the trigger last did for the resource). Whether anything ran.
    fn trigger(&self) -> bool {
        let Some(now) = self.now() else {
            return false;
        };
        self.host.run_triggers(now) > 0
    }

    /// How long until what the packages have due next: the service's next poll runs it then, so
    /// nothing polls for it.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        let now = self.now()?;
        let at = self.host.next_trigger(now)?;
        Some(std::time::Duration::from_secs(
            u64::try_from(at.saturating_sub(now)).unwrap_or(0),
        ))
    }

    /// Where the notices the workspace holds for this device's person are shown: the session's
    /// notifications, the ones every app's notification goes through.
    pub fn set_tell(&mut self, tell: impl tilo_service::notice::Notices + 'static) {
        self.tell = Some(Box::new(tell));
    }

    /// What this device's person has told, to keep across restarts: the notices it shows no
    /// more ([`crate::notices::Notices::kept`]).
    #[must_use]
    pub fn told_kept(&self) -> Vec<String> {
        self.notices.kept()
    }

    /// Take up what this device told before it restarted: none of those notices is told again.
    pub fn take_up_told(&mut self, kept: impl IntoIterator<Item = String>) {
        self.notices.remember(kept);
    }

    /// The person at this device saw the notice `id` — they opened or dismissed what it showed —
    /// so it is marked seen for every device of theirs, and its notification here is taken back.
    pub fn notice_seen(&mut self, id: &str) {
        if let Err(error) = self.notices.seen(&self.host, id) {
            tracing::warn!(%error, notice = id, "a notice was not marked seen");
            return;
        }
        self.told();
    }

    /// Mark seen the notices the person dismissed on this device since the last look: their
    /// notification closed because they dismissed it ([`tilo_service::notice::Closed::Dismissed`]),
    /// not because it ran out or because another device of theirs saw it.
    fn dismissed(&mut self) {
        let Some(tell) = &self.tell else {
            return;
        };
        let ids: Vec<String> = tell
            .take_closed()
            .into_iter()
            .filter(|(_, reason)| *reason == tilo_service::notice::Closed::Dismissed)
            .filter_map(|(tag, _)| self.notices.shown_as(&tag).map(str::to_owned))
            .collect();
        for id in ids {
            self.notice_seen(&id);
        }
    }

    /// Show the notices this device's person has not seen, and take back the ones they have
    /// seen since the last look, here or on another device of theirs.
    fn told(&mut self) {
        let Some(tell) = &self.tell else {
            return;
        };
        let told = self.notices.step(&self.host);
        for notice in &told.show {
            tell.show(notice);
        }
        for tag in &told.withdraw {
            tell.withdraw(tag);
        }
    }

    /// What `now` reads: this device's clock, or what a test fixes.
    pub fn set_clock(&mut self, clock: impl Fn() -> String + 'static) {
        self.clock = Box::new(clock);
    }

    /// This device's clock now, in seconds since the epoch.
    fn now(&self) -> Option<i64> {
        tilolang::calendar::instant(&(self.clock)())
    }
}

impl Service for ResourceService {
    /// An app's resources ([`Self::of_app`]) are the ones its windows open in the workspace: the
    /// service reads and writes them there from now on, and fills them with its seed where the
    /// workspace holds none of them yet.
    fn keep_in_workspace(
        &mut self,
        store: &std::rc::Rc<dyn tilo_service::WorkspaceStore>,
    ) -> Result<bool, String> {
        let Some(app) = self.only.clone() else {
            return Ok(false);
        };
        if self.in_workspace {
            return Ok(false);
        }
        let port: std::rc::Rc<dyn crate::resources::Port> = std::rc::Rc::new(WorkspacePort {
            store: std::rc::Rc::clone(store),
            app: app.clone(),
        });
        // The service moves only once the workspace took the seed: one it refused leaves the
        // service reading the resources it held, and the move is asked again.
        let host = self.host_over(crate::resources::ResourceStore::new(port))?;
        // Seeded once something besides the app's root is kept: a root alone is what a seed the
        // workspace refused partway left (its keys were not here yet), and it is seeded again.
        let held = match host.package(&app) {
            Some(package) => {
                let mut held = false;
                for ty in &package.meta.types {
                    if package.meta.roots.contains(&ty.id) {
                        continue;
                    }
                    if !host
                        .store()
                        .resources_of(&ty.id)
                        .map_err(|error| error.to_string())?
                        .is_empty()
                    {
                        held = true;
                        break;
                    }
                }
                held
            }
            None => true,
        };
        if !held && let Some(seed) = &self.seed {
            seed(&host)?;
            if let Some(refused) = host.store().take_refusal() {
                return Err(refused);
            }
        }
        self.install(host);
        self.in_workspace = true;
        self.publications.update(&self.host, &self.planner);
        let _ = self.watch.take();
        self.moved_unsaid = true;
        Ok(true)
    }

    fn own_person(&mut self, person: &str, _earlier: &[String]) {
        person.clone_into(&mut self.person);
    }

    /// Where this service's resources open on the web, as the session read it from the workspace's
    /// directory: what its `share-link` mints under ([`Self::set_link_home`]).
    fn lend_link_home(&mut self, home: Option<&str>) {
        let home = home.and_then(|home| match serde_json::from_str(home) {
            Ok(home) => Some(home),
            Err(error) => {
                // The session made this JSON; one that does not read leaves this service's
                // links refused, which the session's log says why.
                tracing::warn!(
                    %error,
                    "a workspace's page does not read: its resources have no link"
                );
                None
            }
        });
        self.set_link_home(home);
    }

    fn keeps_session_document(&self) -> bool {
        self.session_document
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        // Each resource is its package's, as an app's connection says of what it publishes: what
        // places it on the canvas shows it through that app.
        let mut out: Vec<ResourceSpec> = self
            .publications
            .trees()
            .values()
            .filter(|tree| {
                crate::publish::app_of(tree.id.as_str()).is_none_or(|app| !self.profile_only(app))
            })
            .map(|tree| {
                let mut tree = (**tree).clone();
                kept_by_its_app(&mut tree);
                // The root offers its history, as an app's own resource does in its instance.
                with_history(tree)
            })
            .collect();
        let words = self.host.runtime().words();
        for package in self.host.packages() {
            if !self.offers(&package.meta.app) {
                continue;
            }
            let resource = publish_package(&package.meta, &self.planner, &*words);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }

    fn read(&self, resource: &str) -> Option<Json> {
        let app = crate::publish::app_of(resource)?;
        if self.profile_only(app) {
            return None;
        }
        reading_of(&self.host, resource)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, ApiError> {
        let invoker = tilo_actions::ActionInvoker::person(self.person.clone());
        self.run(action, resource, arguments, &invoker)
    }

    /// [`Service::invoke`] as `invoker` (an agent acting for its person): the change is
    /// attributed to them, and the resources are the same ones.
    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, ApiError> {
        self.run(action, resource, arguments, invoker)
    }

    /// The places a resource's `open` asked this service for since the last pass: the session
    /// places each through its layout, as its own `open` does.
    fn take_open(&mut self) -> Vec<(tilo_actions::ActionInvoker, tilo_service::Open)> {
        std::mem::take(&mut self.opens)
    }

    /// The place the invocation that follows is asked in, by the resource it shows: the
    /// conversation of the turn an agent asks in, or empty for the place in front. A resource the
    /// invocation opens stands beside it.
    fn note_beside(&mut self, beside: &str) {
        beside.clone_into(&mut self.beside);
    }

    fn poll(&mut self) -> bool {
        let touched = self.publications.update(&self.host, &self.planner);
        let ran = self.trigger();
        // What the person dismissed here is their answer to a notice, as their change.
        self.dismissed();
        // What the workspace holds for this device's person, as the store reached it since the
        // last look.
        self.told();
        // The origins keep their resources in step with their services, and hand the outside
        // effects the packages queued to them: what they wrote is published.
        let wrote = self.poll_origins();
        self.watched() || !touched.is_empty() || ran || wrote
    }

    fn wakes_in(&self) -> Option<std::time::Duration> {
        ResourceService::wakes_in(self)
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        self.watched();
        let moved = std::mem::take(&mut self.moved);
        if std::mem::take(&mut self.moved_unsaid) {
            return None;
        }
        Some(moved.into_iter().collect())
    }

    fn module(&self, app: &str) -> Option<Arc<[u8]>> {
        self.shared
            .get(app)
            .cloned()
            .or_else(|| self.host.package(app).map(|package| package.wasm))
    }

    fn apps(&self) -> Vec<String> {
        let loaded = self
            .host
            .packages()
            .into_iter()
            .map(|package| package.meta.app.clone());
        let apps: BTreeSet<String> = loaded.chain(self.shared.keys().cloned()).collect();
        apps.into_iter()
            .filter(|app| self.answers_for(app))
            .collect()
    }

    fn share(&mut self, share: &tilo_service::Share<'_>) -> Result<(), String> {
        Self::share(self, share)
    }

    fn connectors(&self) -> Vec<tilo_service::Connector> {
        // A package's types that hold an outside service's resources: the origins the session
        // runs a connector process for, for the app whose manifest the package is
        // (`crate::origin::origin_types`). Only an origin a crate registered a connector's
        // client for gets one.
        let mut connectors: Vec<tilo_service::Connector> = Vec::new();
        for package in self.host.packages() {
            for ty in &package.meta.types {
                let Some(origin) = ty.origin.as_deref().and_then(|kind| kind.split('.').next())
                else {
                    continue;
                };
                if !crate::origin::served(origin) {
                    continue;
                }
                let connector = tilo_service::Connector {
                    app: package.meta.app.clone(),
                    origin: origin.to_owned(),
                };
                if !connectors.contains(&connector) {
                    connectors.push(connector);
                }
            }
        }
        connectors
    }

    #[cfg(unix)]
    fn connect(&mut self, origin: &str, connection: std::os::fd::OwnedFd) -> Result<(), String> {
        self.connect_origin(origin, connection)
    }

    fn wake_origins(&mut self, wake: tilo_service::Wake) {
        self.set_origin_waker(wake);
    }

    fn settle(&mut self) {
        self.publications.update(&self.host, &self.planner);
    }
}

/// The workspace's store as one app's resources reach it ([`Service::keep_in_workspace`]).
struct WorkspacePort {
    store: std::rc::Rc<dyn tilo_service::WorkspaceStore>,
    app: String,
}

impl crate::resources::Port for WorkspacePort {
    fn command(
        &self,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        let bytes = serde_json::to_vec(&command).map_err(|error| error.to_string())?;
        let reply = self.store.command(&self.app, &bytes)?;
        serde_json::from_slice(&reply).map_err(|error| error.to_string())
    }
}

/// `resource` (a published root) with the history verbs answered here, those it does not offer
/// itself.
pub fn with_history(mut resource: ResourceSpec) -> ResourceSpec {
    for spec in verbs::specs() {
        if HISTORY_VERBS.contains(&spec.id.as_str())
            && !resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
        {
            resource = resource.with_action(spec);
        }
    }
    resource
}

/// Name the app that keeps `resource` and each of its children, by its published id
/// (`tl:<app>:<id>`), where it names none yet.
fn kept_by_its_app(resource: &mut ResourceSpec) {
    if resource.app_id.is_none()
        && let Some((app, _)) = split_published(resource.id.as_str())
    {
        resource.app_id = Some(app.to_owned());
    }
    for child in &mut resource.children {
        kept_by_its_app(child);
    }
}

/// The type a resource of the kind `kind` (`tl.<app>.<Type>`, as [`ResourceService`] publishes
/// it) is, as a runtime that loads its package as another app's names it: `<app>#<Type>`.
/// `None` for a kind no tilolang resource has.
#[must_use]
pub fn reading_type(kind: &str) -> Option<String> {
    let (app, ty) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    Some(format!("{app}#{ty}"))
}

/// The app of a resource whose type the shell's runtime names `<app>#<Type>`.
#[must_use]
pub fn app_of(ty: &str) -> Option<&str> {
    ty.split_once('#').map(|(app, _)| app)
}

/// Each resource `records` (the ids a shell's places name) that `resources`, the session's
/// catalogue, holds as a tilolang resource: its id, and its type as the shell's runtime names
/// another app's (`<app>#<Type>`). A place naming nothing the catalogue holds keeps its well.
#[must_use]
pub fn placed_documents<'a>(
    records: impl IntoIterator<Item = &'a str>,
    resources: &[ResourceSpec],
) -> BTreeMap<String, String> {
    records
        .into_iter()
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let ty =
                tilo_actions::api::find_resource(resources, record).and_then(reading_type_of)?;
            Some((record.to_owned(), ty))
        })
        .collect()
}

/// The type the shell's runtime names a resource of `spec` by (`<app>#<Type>`): a tilolang
/// resource's (`tl.<app>.<Type>`), or a Rust app's resource, whose kind is the app's own
/// (`tube.Channel`) and whose app the catalogue names. `None` for a resource no app draws.
#[must_use]
pub fn reading_type_of(spec: &ResourceSpec) -> Option<String> {
    if let Some(ty) = reading_type(&spec.kind) {
        return Some(ty);
    }
    let app = spec.app()?;
    let ty = spec.kind.rsplit('.').next()?;
    Some(format!("{app}#{ty}"))
}
