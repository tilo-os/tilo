//! A tilolang app's record of a workspace, as a device's own roles read and change it: through
//! the app's own package, as a given member, each write reaching the workspace's resource store
//! as it is made.
//!
//! The workspace service keeps what every record read and write needs — the store
//! ([`crate::workspaces::Workspaces::record_command`]), the workspace's policy, and the route a
//! record is addressed at — and the tilolang runtime, which the daemon's program links and mounts
//! ([`Tilolang`]), answers the rest: the packages built into the build, the host that runs them,
//! and the versions its store lists. This module is the boundary between the two: the traits a
//! binary implements for its language ([`Records`], [`RichText`], [`crate::styles::Reader`]),
//! the types they answer in ([`Raised`], and [`Package`], the app's package as this device has
//! it), the value it hands over ([`Tilolang`]), and the record a store is read through
//! ([`Record`]).
//!
//! - **The record** ([`Record`]): an app's record of a workspace as this device's workspace
//!   service holds it, which a tilolang store reads and writes through
//!   ([`crate::workspaces::Workspaces::record_command`]).
//! - **The runtime** ([`Records`]): the packages a record is read through, compiled once and
//!   kept, and the host that reads and changes it: what a device's own roles use when they act
//!   on tilolang resources with no window of the app open. A host is bound to its thread, so the
//!   work runs on a blocking thread of its own ([`Workspaces::record_rows`] and the rest), and
//!   only what the caller asked for comes back.
//!
//! Only the packages built into the build run here; an app a workspace installed is untrusted
//! code and runs only on a device that installed it: its windows there, and what search reads of
//! it with none open ([`Workspaces::record_entries_of`], given that device's verified package).
//!
//! Who may do what is decided by the host's authority, from the workspace's policy as the
//! acting member holds it: an agent within its delegation, a bot within its grants. The caller
//! names the member it acts as; code on this device that calls here is the device's own.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value as Json};
use tilo_action_policy::{MemberAccess, WorkspacePolicy};
use tilo_actions::remote::{CataloguePage, CatalogueQuery, CatalogueRow};
use tilo_actions::{ActionAddress, ActionContext, ActionInvoker, ActionOutput, ApiError};
#[cfg(any(feature = "apps", feature = "agents"))]
use tilo_resource_store::wire::{Command, Reply};

use crate::workspaces::Workspaces;

/// The store a record is read and written through: [`Record`] for a workspace's, or one in
/// memory for a record of no workspace.
pub use tilo_resource_store::{LocalPort, Port};

/// What a tilolang app's record of a workspace is read and changed through: the packages built
/// into the build, the host that runs them, and its history. Implemented by the tilolang runtime
/// the daemon's program links ([`Tilolang`]).
pub trait Records: Send + Sync {
    /// Whether this build carries a package of `app`.
    fn built_in(&self, app: &str) -> bool;

    /// The `surface.toml` this build's own sources compile to for `app`: the types and verbs a
    /// workspace's store judges the app's records' writes by.
    fn surface(&self, app: &str) -> Option<Vec<u8>>;

    /// Each package's surface file, as `tilolang build` writes it for an installed app.
    fn surfaces(&self) -> Vec<(String, Vec<u8>)>;

    /// The name of each field of the type `ty` the built-in app `app` declares, by the id the
    /// record keeps it under: how a value the record holds whole is read by its names.
    fn field_names(&self, app: &str, ty: &str) -> BTreeMap<String, String>;

    /// What `app`'s record publishes, each action in the row of the resource holding it, as the
    /// record's `person` reads it. `None` for an app this build runs no package of, or a record
    /// that does not open.
    fn rows(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        app: &str,
    ) -> Option<Vec<CatalogueRow>>;

    /// The page of [`Self::rows`] `query` asks for.
    fn page(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        app: &str,
        query: &CatalogueQuery,
    ) -> Option<CataloguePage>;

    /// Every version of the record's resources, oldest first, each as its history lists it.
    fn changes(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        app: &str,
    ) -> Option<Vec<Json>>;

    /// Take back the changes of `undo`'s invocations in `app`'s record, made as `acting` (an agent
    /// for its person, as it acts: handed its person's session or under a key of its own), as one
    /// run of `run` (its id and title), for `undo`'s call, the call its caller signed when one
    /// asked for it: the undo carries that call's invocation and keeps the call, so every reader
    /// checks that the call declares taking back changes of the record. How many were taken back.
    ///
    /// # Errors
    /// The record does not open, or a change could not be taken back.
    fn undo(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        acting: &tilo_actions::Acting,
        app: &str,
        run: (&str, &str),
        undo: (&[String], Option<&tilo_actions::remote::SignedCall>),
    ) -> Result<u32, String>;

    /// Whether `version` is a change `who` made.
    fn wrote(&self, version: &Json, who: &tilo_actions::MemberId) -> bool;

    /// What `app` keeps in the record's workspace publishes to search, read as the record's
    /// person reads it, through its app's built-in package or through `installed`'s.
    ///
    /// # Errors
    /// No package of `app` is installed or built in, the package does not load, or the record
    /// does not open.
    fn entries(
        &self,
        record: Record,
        policy: Arc<WorkspacePolicy>,
        viewer: &tilo_actions::Actor,
        app: &str,
        installed: Option<&runtime_portal::process_link::Module>,
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String>;

    /// What `app` keeps in the store document `document` of the record's workspace publishes to
    /// search, read as the record's person reads it: the entries of the resources that document
    /// holds now, through its app's built-in package or through `installed`'s.
    ///
    /// # Errors
    /// As [`Self::entries`].
    fn entries_of(
        &self,
        record: Record,
        policy: Arc<WorkspacePolicy>,
        viewer: &tilo_actions::Actor,
        app: &str,
        document: &str,
        installed: Option<&runtime_portal::process_link::Module>,
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String>;

    /// Run `app`'s published action `address` with `arguments`, as `viewer` reads it and as
    /// `invoker` invokes it, within the invocation `context` names: what it answered.
    ///
    /// # Errors
    /// As any call of a published action: unknown, unavailable, invalid arguments, refused.
    fn apply(
        &self,
        record: Record,
        policy: Arc<WorkspacePolicy>,
        access: Arc<MemberAccess>,
        viewer: tilo_actions::Actor,
        app: &str,
        action: (ActionAddress, Map<String, Json>),
        call: (ActionInvoker, ActionContext),
    ) -> Result<ActionOutput, ApiError>;

    /// The notices `app` wrote in `record` that `person` has not seen, read through `package`,
    /// the app's package as this device has it: what a device of theirs shows them.
    ///
    /// # Errors
    /// The record does not open, or its notices do not read.
    fn notices(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        app: &str,
        package: Option<&Package>,
    ) -> Result<Vec<Raised>, String>;

    /// Mark the notice `doc` of `app`'s record seen by `person`: no device of theirs raises it
    /// again.
    ///
    /// # Errors
    /// The record does not open, or the write was refused.
    fn notice_seen(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        app: &str,
        package: Option<&Package>,
        doc: &str,
    ) -> Result<(), String>;

    /// Run `action`, one of `app`'s actions on what the notice `doc` opens, as `person`.
    ///
    /// # Errors
    /// The record does not open, the app does not declare the action, or it was refused.
    fn notice_action(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        app: &str,
        package: Option<&Package>,
        doc: &str,
        action: &str,
    ) -> Result<(), String>;

    /// Serve the records of every app built into the build on `workspaces`, from now on.
    fn serve(&self, workspaces: &Workspaces);
}

/// The package a record is read through here: the one this build carries for the app, the one
/// this device installed, or one the language compiled here (a test's app).
#[derive(Clone)]
pub enum Package {
    /// The package this build carries.
    BuiltIn,
    /// The package this device installed, verified as a launch of it verifies it.
    Installed(runtime_portal::process_link::Module),
    /// A package the language compiled here, as a test's app is: what a program that mounts a
    /// language in this process reads a record of an app it compiled itself through.
    Compiled(Compiled),
}

/// A package the language compiled in this build, as a test's app is: core keeps it as the
/// language handed it over and gives it back the same way, and never looks inside.
#[derive(Clone)]
pub struct Compiled(Arc<dyn std::any::Any + Send + Sync>);

impl Compiled {
    /// `packages`, the language's own compiled packages, kept for it to read back.
    #[must_use]
    pub fn new<T: std::any::Any + Send + Sync>(packages: T) -> Self {
        Self(Arc::new(packages))
    }

    /// The packages, as the language compiled them.
    #[must_use]
    pub fn as_language<T: std::any::Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

/// A notice an app wrote in its record, as this device reads it: what it says, what a person may
/// choose on it, and the device the app addressed it to (empty for every device of the person's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raised {
    /// The id the notice document keeps in the app's record.
    pub doc: String,
    /// What the notice says.
    pub title: String,
    /// The words under its title.
    pub body: String,
    /// The actions it offers, each its key and the words of its label.
    pub actions: Vec<(String, String)>,
    /// The device it is for, empty for every device of the person's.
    pub device: String,
}

/// The rich text a record's published action takes, as the runtime writes it.
pub trait RichText: Send + Sync {
    /// A body naming `to` as `label`, and nothing else.
    ///
    /// # Errors
    /// The value does not write as a record's rich text.
    fn reference(&self, label: &str, to: &tilo_reference::Embed) -> Result<Json, String>;

    /// A body naming `to` as `label`, then `text` after it.
    ///
    /// # Errors
    /// The value does not write as a record's rich text.
    fn mention(&self, label: &str, to: &tilo_reference::Embed, text: &str) -> Result<Json, String>;
}

/// What the binary that links the tilolang runtime hands a workspace service: the records of the
/// apps built into the build, the styles a device's installed packages offer, and the rich text a
/// record's actions take. A service its program mounted nothing on runs the core alone: it reads
/// and changes no tilolang record, and offers the styles a store carries.
#[derive(Clone)]
pub struct Tilolang {
    /// The records of the apps built into the build.
    pub records: Arc<dyn Records>,
    /// The styles the device's installed packages offer ([`crate::styles::Reader`]).
    pub styles: Arc<dyn crate::styles::Reader>,
    /// The rich text a record's published action takes ([`RichText`]).
    pub rich_text: Arc<dyn RichText>,
}

/// An app's record of a workspace, as this device's own workspace service holds it: what a
/// [`Port`] over it reads and writes. It can be sent to another thread; the store it makes
/// cannot.
#[derive(Clone)]
pub struct Record {
    workspaces: Workspaces,
    workspace: String,
    app: String,
    /// The person the record reads as: what of it they may read, and nothing else. Every
    /// record names one, so nothing reads as the person whose device serves it by default.
    reader: String,
    /// The reactor a command waits on. A record without one only reads, waiting as a search
    /// does.
    runtime: Option<tokio::runtime::Handle>,
    /// The first write the workspace refused, as it said why: shared by every copy, so a
    /// program that ran on one learns its run was refused ([`Self::refused`]).
    refused: Arc<Mutex<Option<String>>>,
}

impl Record {
    /// The person the record reads as.
    #[must_use]
    pub fn reader(&self) -> &str {
        &self.reader
    }

    /// Why the workspace refused the first write it refused through this record, when it
    /// refused one: what was written before it stands, and nothing after it was sent.
    #[must_use]
    pub fn refused(&self) -> Option<String> {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `command` carried out by the workspace service, a refusal kept as [`Self::refused`].
    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn carry(
        workspaces: Workspaces,
        (workspace, app, reader): (String, String, String),
        command: Command,
        refused: Arc<Mutex<Option<String>>>,
    ) -> Result<Reply, String> {
        use crate::workspaces::RecordFailure;
        workspaces
            .record_command(&workspace, &app, &reader, command)
            .await
            .map_err(|failure| match failure {
                RecordFailure::Refused(why) => {
                    refused
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get_or_insert_with(|| why.clone());
                    why
                }
                RecordFailure::Failed(error) => error,
            })
    }
}

/// The port a record is read and written through, on the host that serves apps or runs agents.
#[cfg(any(feature = "apps", feature = "agents"))]
impl Port for Record {
    fn held(&self, document: &str) -> Option<Vec<(String, String)>> {
        self.workspaces.held_resources(&self.workspace, document)
    }

    fn command(&self, command: Command) -> Result<Reply, String> {
        // A read answers from the store the workspace reads through without its lock: it needs no
        // reactor and no thread of its own, and never waits for a write that waits on the key
        // service or a link, nor for a whole pass of the record's index.
        if !command.writes() {
            if let Some(answer) =
                self.workspaces
                    .read_record_now(&self.workspace, &self.reader, &command)
            {
                return answer;
            }
            // A workspace whose reading is not kept here yet reads as it opens: it waits for the
            // workspace's lock once, on a thread of its own.
            let workspaces = self.workspaces.clone();
            let (workspace, reader, command) =
                (self.workspace.clone(), self.reader.clone(), command);
            return crate::device_documents::on_its_own(async move {
                workspaces
                    .resource_read(&workspace, &reader, &command)
                    .await
            });
        }
        let workspaces = self.workspaces.clone();
        let (workspace, app, reader) = (
            self.workspace.clone(),
            self.app.clone(),
            self.reader.clone(),
        );
        let Some(runtime) = &self.runtime else {
            return Err(format!(
                "{app}'s record was opened to be read, and takes no write"
            ));
        };
        let carried = Self::carry(
            workspaces,
            (workspace, app, reader),
            command,
            Arc::clone(&self.refused),
        );
        // A first write can wait for this device's group keys on the reactor. A thread already
        // inside the reactor (a bot's run is driven by it) waits from a thread of its own.
        if tokio::runtime::Handle::try_current().is_err() {
            return runtime.block_on(carried);
        }
        std::thread::scope(
            |scope| match scope.spawn(|| runtime.block_on(carried)).join() {
                Ok(reply) => reply,
                Err(panic) => std::panic::resume_unwind(panic),
            },
        )
    }
}

/// One published action of a tilolang record of a workspace, as a host applies it: the app, who
/// reads and invokes it, the action and its arguments, and whether it only reads.
pub struct RecordCall<'a> {
    /// The record's app, as its package and its resources name it.
    pub app: &'a str,
    /// The person the action's reads answer, and as whom it is written.
    pub viewer: tilo_actions::Actor,
    /// Who invokes it, as the API admitted the caller.
    pub invoker: &'a ActionInvoker,
    /// The caller the API admitted: the record's own authority decides the action for it, an
    /// agent held to its delegation and to the bound the workspace's list of agents keeps.
    pub caller: &'a tilo_actions::remote::Caller,
    /// The run the invocation is a step of, as the host stamps it on what it changes.
    pub context: &'a ActionContext,
    /// The action's address in the resource's vocabulary, its resource resolved.
    pub address: &'a ActionAddress,
    /// Its arguments, as the action API binds them.
    pub arguments: &'a Map<String, Json>,
    /// Whether it reads the record rather than changing it: nothing it wrote on the way is kept.
    pub read: bool,
}

/// The member `id` names, as a tilolang actor.
#[must_use]
pub fn member(id: tilo_actions::MemberId) -> tilo_actions::Actor {
    tilo_actions::Actor::Member(id)
}

/// Why a program over the app `app`, which this build does not carry, does not run here.
#[must_use]
pub fn runs_elsewhere(app: &str) -> String {
    format!(
        "this program's app {app} runs only on a device that has it open: this server runs only \
         the apps built into it"
    )
}

impl Workspaces {
    /// `app`'s record of `workspace`, as this device's workspace service holds it, read as
    /// `reader` reads it: a read answers from the store the workspace reads through without its
    /// lock, and a write is carried on the service's writes thread, whose thread nothing else
    /// uses, or on the reactor this is called on where the service has none.
    ///
    /// # Errors
    /// No reactor is here to carry the record's writes.
    pub fn tl_record(&self, workspace: &str, app: &str, reader: &str) -> Result<Record, String> {
        let runtime = self
            .record_writes_runtime()
            .ok_or_else(|| format!("{app}'s record is written on a reactor"))?;
        Ok(self.opened_record(workspace, app, reader, Some(runtime)))
    }

    /// `app`'s record of `workspace` to be read on this thread, waiting as a search does: it
    /// takes no write.
    #[must_use]
    pub fn record_to_read(&self, workspace: &str, app: &str, person: &str) -> Record {
        self.opened_record(workspace, app, person, None)
    }

    fn opened_record(
        &self,
        workspace: &str,
        app: &str,
        reader: &str,
        runtime: Option<tokio::runtime::Handle>,
    ) -> Record {
        Record {
            workspaces: self.clone(),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            reader: reader.to_owned(),
            runtime,
            refused: Arc::default(),
        }
    }

    /// The versions of `route`'s record of `workspace` that `person` may read, oldest first,
    /// each as its history lists it: what a caller reads of the record's changes. `None` for an
    /// app this build runs no package of, a record that does not read now, or a service no
    /// tilolang runtime was mounted on. Nothing is written.
    #[must_use]
    pub fn record_changes(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
    ) -> Option<Vec<Json>> {
        let records = self.tilolang()?.records;
        let record = self.record_to_read(workspace, &route.app_id, person);
        let policy = self.policy_now(workspace).map(Arc::new);
        records.changes(record, policy, &route.app_id)
    }

    /// What `route`'s record of `workspace` publishes, each action in the row of the resource
    /// holding it, read as `person` reads it through its app's built-in package. `None` for an
    /// app this build runs no package of, or a record that does not open.
    #[must_use]
    pub fn record_rows(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
    ) -> Option<Vec<CatalogueRow>> {
        let records = self.tilolang()?.records;
        let record = self.record_to_read(workspace, &route.app_id, person);
        let policy = self.policy_now(workspace).map(Arc::new);
        records.rows(record, policy, &route.app_id)
    }

    /// The page of what `route`'s record of `workspace` publishes that `query` asks for, read as
    /// `person` reads it ([`Workspaces::record_rows`]). `None` as for [`Self::record_rows`].
    #[must_use]
    pub fn record_page(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
        query: &CatalogueQuery,
    ) -> Option<CataloguePage> {
        let records = self.tilolang()?.records;
        let record = self.record_to_read(workspace, &route.app_id, person);
        let policy = self.policy_now(workspace).map(Arc::new);
        records.page(record, policy, &route.app_id, query)
    }

    /// What `app` keeps in `workspace` publishes to search, read from the workspace's resource
    /// store as `person` reads it: every entry of the app's resources, so search finds
    /// the app's resources on a device that never opened a window of it. The app is read through
    /// `installed`, its package as this device installed it and verified as a launch verifies it
    /// (what its windows run here), and without one through the package built into this build.
    ///
    /// # Errors
    /// No tilolang runtime is mounted, no package of `app` is installed or built in, the package
    /// does not load, the workspace's policy does not read here, or the record does not open.
    pub fn record_entries(
        &self,
        workspace: &str,
        app: &str,
        installed: Option<&runtime_portal::process_link::Module>,
        person: &str,
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String> {
        let records = self
            .tilolang()
            .ok_or_else(|| "this device runs no tilolang runtime".to_owned())?
            .records;
        let record = self.record_to_read(workspace, app, person);
        // Without the policy nobody's reads are known, and every resource would read as anyone's.
        let policy = self
            .policy_now(workspace)
            .map(Arc::new)
            .ok_or_else(|| format!("{workspace}'s policy does not read here"))?;
        let viewer = tilo_actions::Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
        records.entries(record, policy, &viewer, app, installed)
    }

    /// What `app` keeps in the store document `document` of `workspace` publishes to search,
    /// read from the workspace's resource store as `person` reads it: every entry the resources
    /// that document holds now. The app and the person are read as [`Self::record_entries`]
    /// reads them.
    ///
    /// # Errors
    /// As [`Self::record_entries`].
    pub fn record_entries_of(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        installed: Option<&runtime_portal::process_link::Module>,
        person: &str,
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String> {
        let records = self
            .tilolang()
            .ok_or_else(|| "this device runs no tilolang runtime".to_owned())?
            .records;
        let record = self.record_to_read(workspace, app, person);
        // Without the policy nobody's reads are known, and every resource would read as anyone's.
        let policy = self
            .policy_now(workspace)
            .map(Arc::new)
            .ok_or_else(|| format!("{workspace}'s policy does not read here"))?;
        let viewer = tilo_actions::Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
        records.entries_of(record, policy, &viewer, app, document, installed)
    }

    /// Apply `call`, a published action of an app's record of `workspace`, through the app's
    /// built-in package. A read answers the core read as the person the caller acts for reads
    /// it, from a record that takes no write. A write reaches the workspace's store as it is
    /// made (`Workspaces::record_command`), stamped with the caller as its invoker: whether they
    /// may was decided under their grants by the host that admitted the call.
    ///
    /// # Errors
    /// No tilolang runtime is mounted, this build carries no package of the app, the record does
    /// not open, the action is unknown, refused or fails, or the workspace refused what it wrote.
    pub async fn apply_record_action(
        &self,
        workspace: &str,
        call: RecordCall<'_>,
    ) -> Result<ActionOutput, ApiError> {
        let RecordCall {
            app,
            viewer,
            invoker,
            caller,
            context,
            address,
            arguments,
            read,
        } = call;
        let records = self
            .tilolang()
            .ok_or_else(|| ApiError::Failed("this device runs no tilolang runtime".to_owned()))?
            .records;
        if !records.built_in(app) {
            return Err(ApiError::Unavailable {
                code: "runs_elsewhere".into(),
                reason: runs_elsewhere(app),
            });
        }
        // The record reads only what the person the caller acts for may read, a write's reads too.
        let person = crate::workspaces::person_reading(caller);
        let record = if read {
            self.record_to_read(workspace, app, &person)
        } else {
            self.tl_record(workspace, app, &person)
                .map_err(ApiError::Failed)?
        };
        let policy = self
            .policy_of(workspace)
            .await
            .map(Arc::new)
            .ok_or_else(|| ApiError::Failed(format!("{workspace}'s policy does not read here")))?;
        // The record's own authority decides the action as the caller: this device writes it,
        // so nothing else stops an agent from doing what its delegation does not give it.
        let acting = caller.acting();
        let access = Arc::new(
            MemberAccess::new(Arc::clone(&policy), acting.clone()).granted(caller.granted()),
        );
        let app = app.to_owned();
        let (invoker, context) = (invoker.clone(), context.clone());
        let (address, arguments) = (address.clone(), arguments.clone());
        tokio::task::spawn_blocking(move || {
            records.apply(
                record,
                policy,
                access,
                viewer,
                &app,
                (address, arguments),
                (invoker, context),
            )
        })
        .await
        .map_err(|error| ApiError::Failed(error.to_string()))?
    }

    /// The notices `app` raised in `workspace` that `person` has not seen, read through
    /// `package`, the app's package as this device has it (`None` for the one this build
    /// carries): what a device of theirs shows them.
    ///
    /// # Errors
    /// The app is neither built in nor installed here, its package does not load, or the record
    /// could not be opened or read.
    pub async fn app_notices(
        &self,
        workspace: &str,
        app: &str,
        package: Option<&Package>,
        person: &str,
    ) -> Result<Vec<Raised>, String> {
        let records = self.running(app)?;
        let record = self.tl_record(workspace, app, person)?;
        let policy = self.policy_of(workspace).await.map(Arc::new);
        let (reader, app, package) = (person.to_owned(), app.to_owned(), package.cloned());
        tokio::task::spawn_blocking(move || {
            records.notices(record, policy, &reader, &app, package.as_ref())
        })
        .await
        .map_err(|error| error.to_string())?
    }

    /// Mark the notice `doc` in `app`'s record of `workspace` seen by `person`: no device of
    /// theirs raises it again.
    ///
    /// # Errors
    /// The app's record could not be opened, or the write was refused.
    pub async fn notice_seen(
        &self,
        workspace: &str,
        app: &str,
        package: Option<&Package>,
        person: &str,
        doc: &str,
    ) -> Result<(), String> {
        let records = self.running(app)?;
        let record = self.tl_record(workspace, app, person)?;
        let policy = self.policy_of(workspace).await.map(Arc::new);
        let (reader, app, package, doc) = (
            person.to_owned(),
            app.to_owned(),
            package.cloned(),
            doc.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            records.notice_seen(record, policy, &reader, &app, package.as_ref(), &doc)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    /// Run `action`, one of `app`'s actions on what the notice `doc` opens, as `person`.
    ///
    /// # Errors
    /// The app's record could not be opened, the action is not the app's, or it was refused.
    pub async fn notice_action(
        &self,
        workspace: &str,
        app: &str,
        package: Option<&Package>,
        person: &str,
        doc: &str,
        action: &str,
    ) -> Result<(), String> {
        let records = self.running(app)?;
        let record = self.tl_record(workspace, app, person)?;
        let policy = self.policy_of(workspace).await.map(Arc::new);
        let (reader, app, package, doc, action) = (
            person.to_owned(),
            app.to_owned(),
            package.cloned(),
            doc.to_owned(),
            action.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            records.notice_action(
                record,
                policy,
                &reader,
                &app,
                package.as_ref(),
                &doc,
                &action,
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }

    /// The records a tilolang app's record is read and changed through here, or why `app` runs
    /// only on a device that has it open.
    fn running(&self, app: &str) -> Result<Arc<dyn Records>, String> {
        self.tilolang()
            .map(|tilolang| tilolang.records)
            .ok_or_else(|| runs_elsewhere(app))
    }
}
