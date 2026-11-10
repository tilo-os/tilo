//! A workspace's tilolang records as a session service: every record and item a loaded package
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
//! note's reading, and the reading is the record the notes app shows.
//!
//! An invocation is the published action's own: the door builds it through the action's spec,
//! which plans it in the runtime, and it is committed to the record as a step of the person
//! seated here.

use std::sync::Arc;

use serde_json::{Map, Value as Json};
use tilo_actions::{ActionAddress, ActionContext, ActionOutput, DoorError, ResourceSpec};
use tilo_service::{Service, Started};
use tilolang_runtime::value::ResourceRef;

use crate::action::{ACTION_TYPE, DynAction, EDIT_FLAG_ACTION, EDIT_TEXT_ACTION};
use crate::host::Host;
use crate::publish::{Planner, Publications, edit_action, publish_package, resource_id};

/// The workspace record's tilolang records, published to the session.
pub struct RecordService {
    record: tilo_resource::Doc,
    host: Host,
    planner: Arc<dyn Planner>,
    publications: Publications,
    /// Every field change since the last poll: a reading moves with any field of its resource,
    /// also one no published tree shows (a note's body).
    watch: crate::store::Watch,
    /// Who the changes made here are steps of.
    person: String,
    /// What `now` reads: the system time, or what a test fixes.
    clock: Box<dyn Fn() -> String>,
    /// The resources whose readings moved since [`Service::moved`] was last asked, by their
    /// published ids, and whether something moved that names no one resource (the record was
    /// taken in anew, a document changed as a whole).
    moved: std::collections::BTreeSet<String>,
    moved_unsaid: bool,
    /// The packages the session runs itself (the shell), whose records it keeps in
    /// [`Self::record`] beside the apps': each its compiled module, its description embedded,
    /// so a connection reads the package's records by its app as it reads any app's.
    shared: std::collections::BTreeMap<String, std::sync::Arc<[u8]>>,
    /// The document each of [`Self::shared`]'s packages' parts commit into, by the app: what
    /// this service takes in to publish what they wrote.
    shared_records:
        std::collections::BTreeMap<String, std::rc::Rc<std::cell::RefCell<tilo_resource::Doc>>>,
    /// The outside services its packages' types with an origin are kept in step with, by
    /// origin (`imap`, `youtube`): each native code this service runs beside the records,
    /// reached through the connection the session hands it ([`Service::connect`]).
    origins: crate::origin::Origins,
    /// What wakes the session's loop when an origin has an answer and work waits.
    origin_waker: Option<crate::background::Waker>,
    /// The notices this device shows its person, and what it told already
    /// ([`crate::notices::Notices`]).
    notices: crate::notices::Notices,
    /// Where they are shown: the session's notifications. Until it is set, nothing is shown.
    tell: Option<Box<dyn tilo_session_core::notice::Notices>>,
}

impl RecordService {
    /// The records `host` runs over `record`, its packages loaded, published for `person`.
    #[must_use]
    pub fn new(record: tilo_resource::Doc, host: Host, person: impl Into<String>) -> Self {
        let person: String = person.into();
        host.sync_from(&record);
        let planner: Arc<dyn Planner> = Arc::new(host.handle());
        let mut publications = Publications::new(&host);
        let watch = host.store().watch();
        publications.settle(&host, &planner);
        let _ = watch.take();
        Self {
            record,
            host,
            planner,
            publications,
            watch,
            notices: crate::notices::Notices::new(person.clone()),
            tell: None,
            person,
            clock: Box::new(crate::host::system_time),
            moved: std::collections::BTreeSet::new(),
            moved_unsaid: false,
            shared: std::collections::BTreeMap::new(),
            shared_records: std::collections::BTreeMap::new(),
            origins: crate::origin::Origins::default(),
            origin_waker: None,
        }
    }

    /// Publish the package `meta`'s records, which the session runs the package itself for,
    /// with the apps' this service keeps: its roots are made in the record, and whatever its
    /// parts commit into `records` — the document they share, the one they draw from — is taken
    /// in and published from then on. A REPL or an agent's task reads and changes them by the
    /// package's app, as it does Notes'.
    ///
    /// The package's own record is where its views' roots read: the shell keeps the person's
    /// arrangement and the shell's settings, and both are the person's, synced with their
    /// devices, as an app's record is.
    pub fn share(&mut self, share: &tilo_service::Share<'_>) -> Result<(), String> {
        use tilo_automerge::ApplyIncoming as _;
        let Some(meta) = crate::runtime::Meta::from_wasm(share.wasm) else {
            return Err(format!("{} carries no description", share.app));
        };
        self.record
            .merge_incoming(&mut share.records.borrow_mut())
            .map_err(|error| error.to_string())?;
        self.record.commit();
        self.host.sync_from(&self.record);
        // The package's own code runs here, over its record as any app's does: its roots are
        // resources of the record, so a program reads and changes them by the app.
        self.host
            .load_module(share.wasm, meta.clone(), String::new())
            .map_err(|error| format!("{}: {error}", meta.app))?;
        for root in &meta.named_roots {
            self.host
                .root_record(&meta.app, &root.ty)
                .map_err(|error| format!("{}'s root {}: {error}", meta.app, root.name))?;
        }
        // The roots are made: what that wrote goes where the package's parts read it, so the
        // record they draw exists for them too, each as the step its authorship names.
        for committed in self.host.take_committed() {
            let invoker = invoker_of(&committed.action);
            self.commit(&committed.action, &invoker)?;
        }
        *share.records.borrow_mut() = self.record.clone();
        self.shared
            .insert(meta.app.clone(), std::sync::Arc::from(share.wasm));
        self.shared_records
            .insert(meta.app, std::rc::Rc::clone(share.records));
        self.publications.settle(&self.host, &self.planner);
        let _ = self.watch.take();
        Ok(())
    }

    /// Take in what the parts of the packages `share`d wrote into the documents they share
    /// since the last call, and publish it: the shell's own parts commit a setting or a place
    /// into the record, and the door offers what it changed. Whether anything was taken in.
    pub fn follow(&mut self) -> Result<bool, String> {
        use tilo_automerge::ApplyIncoming as _;
        let mut took = false;
        for (app, records) in &self.shared_records {
            let heads = self.record.committed_heads();
            let theirs = records.borrow().committed_heads();
            if heads == theirs {
                continue;
            }
            let mut theirs = records.borrow_mut();
            match self.record.merge_incoming(&mut theirs) {
                Ok(merged) if merged.is_empty() => {}
                Ok(_) => {
                    took = true;
                    // What the parts hold is the same record: the change this service makes
                    // (by a connection's action) reaches them too.
                    let _ = theirs.merge_incoming(&mut self.record.clone());
                }
                Err(error) => return Err(format!("{app}'s record: {error}")),
            }
        }
        if !took {
            return Ok(false);
        }
        self.record.commit();
        self.host.sync_from(&self.record);
        self.moved_unsaid = true;
        Ok(true)
    }

    /// The record of the package `app` the session runs itself (`share`d here): its app's
    /// committed document and the packages a reader of it loads (the package's and each it
    /// uses), as a task's host reads it. `None` for any other app.
    #[must_use]
    pub fn service_record(&self, app: &str) -> Option<tilo_service::Record> {
        let wasm = self.shared.get(app)?;
        let meta = crate::runtime::Meta::from_wasm(wasm)?;
        let mut packages = vec![std::sync::Arc::clone(wasm)];
        for used in &meta.uses {
            if let Some(package) = self.host.package(used) {
                packages.push(std::sync::Arc::clone(&package.wasm));
            }
        }
        Some(tilo_service::Record {
            app: app.to_owned(),
            id: crate::own_record(&meta, &self.person).unwrap_or_default(),
            document: self.record.clone().save(),
            packages,
        })
    }

    /// The workspace record as it is here.
    #[must_use]
    pub fn record(&self) -> &tilo_resource::Doc {
        &self.record
    }

    /// The runtime the records are read through.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Take in what another replica of the workspace record holds.
    pub fn take_in(&mut self, other: &mut tilo_resource::Doc) -> Result<(), String> {
        use tilo_automerge::ApplyIncoming as _;
        self.record
            .merge_incoming(other)
            .map_err(|error| error.to_string())?;
        self.host.sync_from(&self.record);
        // What reached it of the person's notices, told as it arrives.
        self.told();
        Ok(())
    }

    /// Have `wake` called (from another thread) when records built off the frame are ready for
    /// the next [`Service::poll`] to publish.
    pub fn set_waker(&mut self, wake: crate::background::Waker) {
        self.publications.set_waker(wake);
    }

    /// Reach the connector of `origin` at the far end of `connection`: the origin that keeps
    /// the packages' types of its kinds in step with the service, native code this service runs
    /// beside the records, asks the connector over the socket.
    ///
    /// The connector is the process the session started for the app in the sandbox its
    /// manifest declares for the origin: this service asks through it, never holding the
    /// network or the credentials itself.
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

    /// Poll each origin, and carry out the outside effects the record queued that wait for it:
    /// what it wrote is committed to the record as the origin's change and published. Whether
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
        // already due (an origin is due once something is handed to it). What cannot be carried
        // out yet waits in the record, as a window's does.
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
            // an origin writes into the records of the app that holds its resources.
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
            let polled = origin.borrow_mut().poll(&cx);
            if polled.wrote {
                wrote = true;
            }
        }
        if wrote {
            // What the origins wrote is committed to the record this service publishes, as the
            // origins' own changes.
            self.flush();
            self.publications.settle(&self.host, &self.planner);
        }
        wrote
    }

    /// Whether a field of any record changed since the last call. What changed is kept
    /// for [`Service::moved`]: each resource a changed field is of, by its published id.
    fn watched(&mut self) -> bool {
        use tilolang_runtime::store::Store as _;
        let changes = self.watch.take();
        let any = changes.everything || !changes.whole.is_empty() || !changes.docs.is_empty();
        self.moved_unsaid |= changes.everything || !changes.whole.is_empty();
        // A resource that is gone has no app to name it by: what listed it moved too, and
        // names it no more.
        for key in changes.docs.values().flatten() {
            if let Some(app) = ResourceRef::parse_id(&key.resource, String::new())
                .and_then(|probe| self.host.store().type_of(&probe).ok())
                .and_then(|ty| crate::schema::lookup(&ty))
                .map(|schema| schema.app.clone())
            {
                self.moved.insert(resource_id(&app, &key.resource));
            }
        }
        any
    }

    /// The reading of the record or item `path` of `app`.
    fn reading(&self, app: &str, path: &str) -> Option<Json> {
        use tilolang_runtime::store::Store as _;
        let probe = ResourceRef::parse_id(path, String::new())?;
        let ty = self.host.store().type_of(&probe).ok()?;
        let schema = crate::schema::lookup(&ty)?;
        if schema.app != app {
            return None;
        }
        let at = ResourceRef::parse_id(path, ty)?;
        let runtime = self.host.runtime();
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
        // What links to it, each with how: a view composed elsewhere draws them as this app's
        // own views do ([`tilolang_runtime::store::LINKS`]), and runs again when they move.
        if let Ok(links) = self.host.store().linked(&at) {
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

    /// The direct edit (a bound control's) of the field `arguments` name in the record or item
    /// `resource`: what a view composed over its reading elsewhere sends when the person types
    /// into it. Both publish these edits, and the same ones: an item is edited in its record by
    /// its id there ([`edit_action`]).
    /// `None` for a resource that is not one of these records'.
    /// A flag (`flag`) may come as its text, as a door that knows no parameter type for it
    /// hands it on.
    fn direct_edit(
        &self,
        resource: &str,
        flag: bool,
        arguments: &Map<String, Json>,
    ) -> Option<Result<DynAction, String>> {
        use tilolang_runtime::store::Store as _;
        let (app, path) = resource.strip_prefix("tl:")?.split_once(':')?;
        let package = self.host.package(app)?;
        let probe = ResourceRef::parse_id(path, String::new())?;
        let ty = self.host.store().type_of(&probe).ok()?;
        let schema = crate::schema::lookup(&ty)?;
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
        Some(
            self.host
                .with_schemas(|| edit_action(&package.meta, &schema.name, path, field, &value)),
        )
    }

    /// Commit `action` to the record as a step of `invoker`: the person seated here, or an
    /// agent acting for them.
    fn commit(
        &mut self,
        action: &DynAction,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<(), String> {
        let encoded = action.encode().map_err(|error| error.to_string())?;
        let context = ActionContext::for_resource(
            // The workspace root's path, as a grant's tree path names it (`room/app:todo/**`),
            // for an action that wrote no resource of its own.
            action.resources().first().copied().unwrap_or("room"),
            &action.kind,
            &action.kind,
        )
        .with_invoker(invoker.clone());
        let actor = invoker.actor.as_bytes();
        let record = &mut self.record;
        self.host
            .with_schemas(|| {
                tilo_actions::apply_local_with_context(
                    record,
                    action,
                    encoded,
                    Some(actor),
                    context,
                )
            })
            .map_err(|error| error.to_string())?;
        self.record.commit();
        self.host.sync_from(&self.record);
        // The change belongs to every replica of a shared record once it is committed: the
        // parts of a package the session runs itself (the shell) draw it on their next turn.
        for records in self.shared_records.values() {
            use tilo_automerge::ApplyIncoming as _;
            let _ = records
                .borrow_mut()
                .merge_incoming(&mut self.record.clone());
        }
        Ok(())
    }

    /// Run the published `action` of `resource` with `arguments`, planned on the record as it
    /// is and committed as `invoker`'s step: the one path [`Service::invoke`] and
    /// [`Service::invoke_as`] share.
    fn run(
        &mut self,
        action: &str,
        resource: &str,
        arguments: &Map<String, Json>,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<Started, DoorError> {
        let address = ActionAddress::new(resource, action);
        let planned = if action == EDIT_TEXT_ACTION || action == EDIT_FLAG_ACTION {
            self.direct_edit(resource, action == EDIT_FLAG_ACTION, arguments)
                .ok_or_else(|| DoorError::Unknown(address.clone()))?
                .map_err(DoorError::InvalidArguments)?
        } else {
            let encoded = tilo_actions::door::encode_json(&self.resources(), &address, arguments)?;
            if encoded.type_id != ACTION_TYPE {
                return Err(DoorError::Unknown(address));
            }
            serde_json::from_slice(&encoded.payload)
                .map_err(|error| DoorError::InvalidArguments(error.to_string()))?
        };
        self.commit(&planned, invoker).map_err(DoorError::Failed)?;
        // What it made is published before it is answered, so the caller's next invocation
        // reaches it: a record changed as a whole is built now, not off the frame.
        self.publications.settle(&self.host, &self.planner);
        self.told();
        let made = self
            .host
            .take_returned(&planned.action_id)
            .map(|made| resource_id(&planned.app, &made.id_string()));
        Ok(Started::Done(ActionOutput {
            value: None,
            resources: made.into_iter().collect(),
        }))
    }
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

/// Who commits an action the packages committed on their own: the person or agent its
/// authorship names, or the principal that runs a trigger of its app when it names nobody.
fn invoker_of(action: &DynAction) -> tilo_actions::ActionInvoker {
    let actor = if action.authorship.invoker.is_empty() {
        crate::authority::trigger_principal(&action.app).0
    } else {
        action.authorship.invoker.clone()
    };
    tilo_actions::ActionInvoker::actor(actor)
}

impl RecordService {
    /// Run what the packages have due at this device's clock: a trigger runs here whether a
    /// window of its app is open or not, as it does on the device that governs the workspace,
    /// once per instant across the workspace's devices (the change records what the trigger last
    /// did for the record). Whether anything ran.
    fn trigger(&mut self) -> bool {
        let Some(now) = self.now() else {
            return false;
        };
        if self.host.run_triggers(now) == 0 {
            return false;
        }
        // What a trigger did is the app's own change, committed as its invocation says: the same
        // commit as an action from the workspace, as the run's principal.
        self.flush();
        self.moved_unsaid = true;
        true
    }

    /// Commit what the packages committed on their own — a trigger's change, a notice marked
    /// seen — each as the step its authorship names.
    fn flush(&mut self) {
        for committed in self.host.take_committed() {
            if let Err(error) = self.commit(&committed.action, &invoker_of(&committed.action)) {
                tracing::warn!(%error, "a change made on its own did not reach the workspace");
            }
        }
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
    pub fn set_tell(&mut self, tell: impl tilo_session_core::notice::Notices + 'static) {
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
        self.flush();
        self.told();
    }

    /// Mark seen the notices the person dismissed on this device since the last look: their
    /// notification closed because they dismissed it ([`tilo_session_core::notice::Closed::Dismissed`]),
    /// not because it ran out or because another device of theirs saw it.
    fn dismissed(&mut self) {
        let Some(tell) = &self.tell else {
            return;
        };
        let ids: Vec<String> = tell
            .take_closed()
            .into_iter()
            .filter(|(_, reason)| *reason == tilo_session_core::notice::Closed::Dismissed)
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

impl Service for RecordService {
    fn seated(&mut self, person: &str, _earlier: &[String]) {
        person.clone_into(&mut self.person);
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        // Each record is its package's, as an app's connection says of what it publishes: what
        // places it on the canvas shows it through that app.
        let mut out: Vec<ResourceSpec> = self
            .publications
            .trees()
            .values()
            .map(|tree| {
                let mut tree = (**tree).clone();
                kept_by_its_app(&mut tree);
                tree
            })
            .collect();
        let words = self.host.runtime().words();
        for package in self.host.packages() {
            let resource = publish_package(&package.meta, &self.planner, &*words);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }

    /// The address this host publishes `app`'s record at: this service keeps every loaded
    /// package's records, so a launch it makes for the app names one and the app's process opens
    /// no record of its own (`docs/architecture/apps.md` §8 step 2, `one-app-model.md` §4).
    ///
    /// It names the app's root: the resource every view of the package reads its state from
    /// (`root YouTube`, `root Tasklist`), by its published id. An app whose package this host
    /// loaded but which declares no root names none, so a launch of it opens its own record.
    fn hosted(&self, app: &str) -> Option<String> {
        let package = self.host.package(app)?;
        let root = package.meta.roots.first()?;
        Some(crate::publish::resource_id(app, &format!("root-{root}")))
    }

    /// Every package this host runs, by its app, with the resource its record is published at:
    /// what [`Self::hosted`] answers, for a host that asks about them all at once.
    fn served(&self) -> Vec<(String, String)> {
        self.host
            .packages()
            .into_iter()
            .filter_map(|package| {
                let root = package.meta.roots.first()?;
                let app = package.meta.app;
                Some((
                    app.clone(),
                    crate::publish::resource_id(&app, &format!("root-{root}")),
                ))
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Json> {
        let (app, path) = resource.strip_prefix("tl:")?.split_once(':')?;
        self.host.package(app)?;
        self.reading(app, path)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, DoorError> {
        let invoker = tilo_actions::ActionInvoker::actor(self.person.clone());
        self.run(action, resource, arguments, &invoker)
    }

    /// [`Service::invoke`] as `invoker` (an agent acting for its person): the change is
    /// attributed to them, as an agent's is at its app's record origin, and the record is the
    /// same one.
    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, DoorError> {
        self.run(action, resource, arguments, invoker)
    }

    fn poll(&mut self) -> bool {
        let touched = self.publications.update(&self.host, &self.planner);
        let ran = self.trigger();
        // What the person dismissed here is their answer to a notice, as their change.
        self.dismissed();
        // What the workspace holds for this device's person, as the record reached it since the
        // last look.
        self.told();
        // The origins keep their records in step with their services, and hand the outside
        // effects the record queued to them: what they wrote is committed and published.
        let wrote = self.poll_origins();
        self.watched() || !touched.is_empty() || ran || wrote
    }

    fn wakes_in(&self) -> Option<std::time::Duration> {
        RecordService::wakes_in(self)
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        self.watched();
        let moved = std::mem::take(&mut self.moved);
        if std::mem::take(&mut self.moved_unsaid) {
            return None;
        }
        Some(moved.into_iter().collect())
    }

    fn record(&self, app: &str) -> Option<tilo_service::Record> {
        self.service_record(app)
    }

    fn module(&self, app: &str) -> Option<std::sync::Arc<[u8]>> {
        self.shared
            .get(app)
            .cloned()
            .or_else(|| self.host.package(app).map(|package| package.wasm))
    }

    fn records(&self) -> Vec<tilo_service::Record> {
        self.shared
            .keys()
            .filter_map(|app| self.service_record(app))
            .collect()
    }

    fn share(&mut self, share: &tilo_service::Share<'_>) -> Result<(), String> {
        Self::share(self, share)
    }

    fn follow(&mut self) -> Result<bool, String> {
        Self::follow(self)
    }

    fn connectors(&self) -> Vec<tilo_service::Connector> {
        // A package's types that hold an outside service's resources: the origins the session
        // runs a connector process for, for the app whose manifest the package is
        // (`crate::origin::origin_types`). The session's own kinds (`session.desk`, the types
        // the device derives) are written by the session's process, and no connector serves
        // them: only an origin a crate registered a connector's client for gets one.
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

    fn connect(&mut self, origin: &str, connection: std::os::fd::OwnedFd) -> Result<(), String> {
        self.connect_origin(origin, connection)
    }

    fn wake_origins(&mut self, wake: tilo_service::Wake) {
        self.set_origin_waker(wake);
    }

    fn settle(&mut self) {
        self.publications.settle(&self.host, &self.planner);
    }
}

/// Name the app that keeps `resource` and each of its children, by its published id
/// (`tl:<app>:<id>`), where it names none yet.
fn kept_by_its_app(resource: &mut ResourceSpec) {
    if resource.app_id.is_none()
        && let Some((app, _)) = resource
            .id
            .as_str()
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
    {
        resource.app_id = Some(app.to_owned());
    }
    for child in &mut resource.children {
        kept_by_its_app(child);
    }
}

/// The type a resource of the kind `kind` (`tl.<app>.<Type>`, as [`RecordService`] publishes
/// it) is, as a runtime that loads its package as another app's names it: `<app>#<Type>`.
/// `None` for a kind no tilolang record has.
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
/// catalogue, holds as a tilolang record: its id, and its type as the shell's runtime names
/// another app's (`<app>#<Type>`). A place naming nothing the catalogue holds keeps its well.
#[must_use]
pub fn placed_records<'a>(
    records: impl IntoIterator<Item = &'a str>,
    resources: &[ResourceSpec],
) -> std::collections::BTreeMap<String, String> {
    records
        .into_iter()
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let ty =
                tilo_actions::door::find_resource(resources, record).and_then(reading_type_of)?;
            Some((record.to_owned(), ty))
        })
        .collect()
}

/// The type the shell's runtime names a resource of `spec` by (`<app>#<Type>`): a tilolang
/// record's (`tl.<app>.<Type>`), or a Rust app's resource, whose kind is the app's own
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
