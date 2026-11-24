//! A tilolang app's record of a workspace, as a device's own roles read and change it: through
//! the app's own package, as a given member, each write reaching the workspace's resource store
//! as it is made.
//!
//! This is what a device's own roles use when they act on tilolang resources with no window of
//! the app open: the agent engine writing its reply, a bridge posting into a
//! channel, a moderator reading reports. Nothing here is per app.
//!
//! - **Packages** ([`Packages`]): the packages a record is read through, compiled once and kept,
//!   in the order they load (what an app uses first, the app last).
//! - **The record** ([`Record`]): an app's record of a workspace as this device's workspace
//!   service holds it, which a [`ResourceStore`] reads and writes through
//!   ([`crate::workspaces::Workspaces::record_command`]).
//! - **Opened** ([`Opened`]): a tilolang host over that store on the calling thread. It reads
//!   any resource ([`Opened::host`]), and runs any action of a loaded package as any member
//!   ([`Opened::invoke`]): the action's own code runs it, and what it writes goes to the store
//!   stamped with that member as its invoker.
//!
//! [`crate::workspaces::Workspaces::with_tl_record`] opens a record for one call. A host is
//! bound to its thread, so the call runs on a blocking thread of its own and hands back only what
//! the caller's closure returns.
//!
//! Only the packages built into this build ([`built_in`]) run here; an app a workspace installed
//! is untrusted code and runs only on a device that installed it: its windows there, and what
//! search reads of it with none open ([`crate::workspaces::Workspaces::record_entries`], given
//! that device's verified package).
//!
//! Who may do what is decided by the host's authority, from the workspace's policy as the
//! acting member holds it: an agent within its delegation, a bot within its grants
//! ([`Opened::open_with`]). The caller names the member it acts as; code on this device that
//! calls here is the device's own.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

pub mod provided;
#[cfg(test)]
mod tests;

use tilo_lock::MutexRecover as _;

use serde_json::{Map, Value as Json};
use tilo_actions::remote::{CataloguePage, CatalogueQuery, CatalogueRow};
use tilo_actions::{
    ActionAddress, ActionContext, ActionInvoker, ActionOutput, ApiError, ResourceSpec,
};
use tilo_automation::dispatch::DocumentEvent;
use tilo_resource_store::wire::{Command, Reply};
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{Caller, Planner, Publications, publish_package, resource_id};
use tilolang_tilo::runtime::store::Run;
use tilolang_tilo::runtime::{Actor, InvokeOutcome, ResourceRef, Value};

/// The app and type a tilolang resource kind (`tl.<app>.<Type>`) names.
pub use tilolang_tilo::catalogue::tilolang_kind;
/// A store held in this process, for a [`ResourceStore`] over a copy of a record's documents
/// that no workspace service answers: a page's copy, and the tests.
pub use tilolang_tilo::resources::LocalPort;
/// The store a record is read and written through: [`Record::store`] for a workspace's, or one
/// in memory for a record of no workspace.
pub use tilolang_tilo::resources::ResourceStore;
/// The tilolang runtime's values, references and stores, as a record is read and changed with.
pub use tilolang_tilo::runtime;

/// The packages a record is read and changed through, compiled once: those the app uses first,
/// the app itself last, and the platform's packages any of them uses (the workspace's contact
/// directory), which the session publishes and a host holds by their published types.
#[derive(Clone)]
pub struct Packages(
    Arc<Vec<tilolang_tilo::tilolang::Compiled>>,
    Arc<Vec<tilolang_tilo::tilolang::PackageMeta>>,
);

impl Packages {
    /// Compile `sources` (each a package's source and its `app.ids`), each against those before
    /// it and the platform's packages.
    ///
    /// # Errors
    /// A source does not compile: its diagnostics.
    pub fn compile(sources: &[(&str, &str)]) -> Result<Self, String> {
        let platform = tilolang_tilo::platform_apps::platform_packages()?;
        let mut compiled: Vec<tilolang_tilo::tilolang::Compiled> = Vec::new();
        for (source, ids) in sources {
            let lock =
                tilolang_tilo::tilolang::IdsLock::parse(ids).map_err(|error| error.to_string())?;
            let imports: Vec<_> = compiled
                .iter()
                .map(|done| done.meta.clone())
                .chain(platform.iter().cloned())
                .collect();
            let package = tilolang_tilo::tilolang::compile_package(source, &lock, &imports)
                .map_err(|diagnostics| {
                    diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.render("app.tl"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })?;
            compiled.push(package);
        }
        let used: Vec<_> = platform
            .into_iter()
            .filter(|meta| {
                compiled
                    .iter()
                    .any(|package| package.meta.uses.contains(&meta.app))
            })
            .collect();
        Ok(Self(Arc::new(compiled), Arc::new(used)))
    }

    /// The app the record is of: the last package's.
    #[must_use]
    pub fn app(&self) -> &str {
        self.0
            .last()
            .map_or("", |package| package.meta.app.as_str())
    }

    /// The id the app's declaration keeps for its action published as `name`: what a change it
    /// made records.
    #[must_use]
    pub fn action_id(&self, name: &str) -> Option<String> {
        let package = self.0.last()?;
        Some(package.meta.action_published_as(name)?.id.clone())
    }

    /// The action whose declaration id is `id`, of whichever of the packages declares it: what a
    /// change it made is decided as.
    #[must_use]
    pub fn declared(&self, id: &str) -> Option<Declared> {
        self.0.iter().find_map(|package| {
            let meta = &package.meta;
            let action = meta.actions.iter().find(|declared| declared.id == id)?;
            Some(Declared {
                app: meta.app.clone(),
                name: action.name.clone(),
                kind: tilolang_tilo::meta::action_kind(action.class),
                targets: meta
                    .targets_of(&action.params)
                    .into_iter()
                    .map(|ty| meta.kind_of(&ty.name))
                    .collect(),
                package: meta.package_kind(),
            })
        })
    }

    /// The name the app publishes the action whose declaration id is `id`: what a change it made
    /// is read as, since history keeps the id a rename does not disturb.
    #[must_use]
    pub fn action_name(&self, id: &str) -> Option<String> {
        let package = self.0.last()?;
        package
            .meta
            .actions
            .iter()
            .find(|declared| declared.id == id)
            .map(|declared| declared.name.clone())
    }
}

/// An action a package declares, as a change it made is decided: at the resource it was invoked
/// on, under the name it is published by.
pub struct Declared {
    /// The app whose package declares it.
    pub app: String,
    /// The name it is published by.
    pub name: String,
    /// What it does: reads, changes what can be taken back, or reaches outside.
    pub kind: tilo_actions::ActionKind,
    /// The kinds of the resources it is invoked on, each type its first parameter takes; none
    /// for an action invoked on no resource.
    pub targets: Vec<String>,
    /// The kind it is published under when it is invoked on no resource: its package's.
    pub package: String,
}

/// The sources of the packages built into this build, each app after those it uses.
const BUILT_IN: &[&[(&str, &str)]] = &[
    &[(
        include_str!("../../../../apps/tl/chat/app.tl"),
        include_str!("../../../../apps/tl/chat/app.ids"),
    )],
    &[
        (
            include_str!("../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../apps/tl/discussion/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../apps/tl/notes/app.ids"),
        ),
        (
            concat!(
                include_str!("../../../../apps/tl/contacts/app.tl"),
                "\n",
                include_str!("../../../../apps/tl/contacts/identities.tl")
            ),
            include_str!("../../../../apps/tl/contacts/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/tasks/app.tl"),
            include_str!("../../../../apps/tl/tasks/app.ids"),
        ),
    ],
];

/// The packages `app`'s record is read through when the app is built into this build, compiled
/// once: what runs in this device's own process. `None` for any other app, a workspace's
/// installed one included.
///
/// # Errors
/// A built-in package does not compile, which is a bug of this build.
pub fn built_in(app: &str) -> Result<Option<Packages>, String> {
    Ok(compiled()
        .as_ref()
        .map_err(Clone::clone)?
        .iter()
        .find(|packages| packages.app() == app)
        .cloned())
}

/// The apps whose records this build runs from its own packages: each group's own app, after
/// those it uses.
///
/// # Panics
/// A built-in package does not compile, which is a bug of this build.
#[must_use]
pub fn built_in_apps() -> Vec<String> {
    compiled()
        .as_ref()
        .expect("the packages built into this build compile")
        .iter()
        .map(|packages| packages.app().to_owned())
        .collect()
}

/// The packages built into this build, compiled once.
fn compiled() -> &'static Result<Vec<Packages>, String> {
    static COMPILED: OnceLock<Result<Vec<Packages>, String>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        BUILT_IN
            .iter()
            .map(|sources| Packages::compile(sources))
            .collect()
    })
}

/// Why a program over the app `app`, which this build does not carry, does not run here.
#[must_use]
pub fn runs_elsewhere(app: &str) -> String {
    format!(
        "this program's app {app} runs only on a device that has it open: this server runs only \
         the apps built into it"
    )
}

/// An app's record of a workspace, as this device's own workspace service holds it: what a
/// [`ResourceStore`] over it reads and writes ([`Self::store`]). It can be sent to another
/// thread; the store it makes cannot.
#[derive(Clone)]
pub struct Record {
    workspaces: crate::workspaces::Workspaces,
    workspace: String,
    app: String,
    /// The reactor a command waits on. A record without one only reads, waiting as a search
    /// does.
    runtime: Option<tokio::runtime::Handle>,
    /// The first write the workspace refused, as it said why: shared by every copy, so a
    /// program that ran on one learns its run was refused ([`Self::refused`]).
    refused: Arc<std::sync::Mutex<Option<String>>>,
}

impl Record {
    /// A store over the record, on this thread: each command is carried out by the workspace
    /// service before the next is sent.
    #[must_use]
    pub fn store(&self) -> ResourceStore {
        ResourceStore::new(Rc::new(self.clone()))
    }

    /// Why the workspace refused the first write it refused through this record, when it
    /// refused one: what was written before it stands, and nothing after it was sent.
    #[must_use]
    pub fn refused(&self) -> Option<String> {
        self.refused.lock_recover().clone()
    }

    /// `command` carried out by the workspace service, a refusal kept as [`Self::refused`].
    async fn carry(
        workspaces: crate::workspaces::Workspaces,
        (workspace, app): (String, String),
        command: Command,
        refused: Arc<std::sync::Mutex<Option<String>>>,
    ) -> Result<Reply, String> {
        use crate::workspaces::RecordFailure;
        workspaces
            .record_command(&workspace, &app, command)
            .await
            .map_err(|failure| match failure {
                RecordFailure::Refused(why) => {
                    refused.lock_recover().get_or_insert_with(|| why.clone());
                    why
                }
                RecordFailure::Failed(error) => error,
            })
    }
}

impl tilolang_tilo::resources::Port for Record {
    fn command(&self, command: Command) -> Result<Reply, String> {
        let workspaces = self.workspaces.clone();
        let (workspace, app) = (self.workspace.clone(), self.app.clone());
        let Some(runtime) = &self.runtime else {
            if command.writes() {
                return Err(format!(
                    "{app}'s record was opened to be read, and takes no write"
                ));
            }
            return crate::device_documents::on_its_own(Self::carry(
                workspaces,
                (workspace, app),
                command,
                Arc::clone(&self.refused),
            ));
        };
        let carried = Self::carry(
            workspaces,
            (workspace, app),
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

/// The heads `since` names, each parsed: the heads are this server's own, written as hashes it
/// read, so one that does not parse is a broken store, and every change since nothing is the
/// safe answer.
fn heads_since(since: &[String]) -> Vec<automerge::ChangeHash> {
    since
        .iter()
        .filter_map(|hex| {
            tilo_failure::ignored!(
                "a head that is no change hash reads every change since nothing",
                hex.parse::<automerge::ChangeHash>()
            )
            .ok()
        })
        .collect()
}

/// An app's record opened on this thread: a host over it with its packages loaded.
pub struct Opened {
    host: Host,
    app: String,
    /// The Shortcut whose run what is invoked here is ([`Opened::in_run_of`]).
    run_of: Option<String>,
}

/// The arguments `declared` (an action of `meta`) was invoked with, by the names its parameters
/// declare, from `values` the position they were given in: what a program following the change
/// reads as the verb's arguments. A resource among them is named as the action API names it
/// (`{"id", "kind"}`, by `published`), so a program addresses it as it would invoke it. Empty when
/// the change recorded none (a change not made by invoking the action).
fn named_arguments(
    meta: &tilolang_tilo::runtime::meta::Meta,
    declared: &tilolang_tilo::runtime::meta::ActionMeta,
    values: &[Json],
    published: &dyn Fn(&str, &str) -> Option<Json>,
) -> Map<String, Json> {
    // An action that acts on a record takes it as its first parameter: a call that gives the
    // record among its arguments carries it there, and one whose caller named it apart carries
    // only the parameters after it.
    let target = usize::from(meta.target_of(&declared.params).is_some());
    let apart = if values.len() < declared.params.len() {
        target
    } else {
        0
    };
    declared
        .params
        .iter()
        .skip(apart)
        .zip(values)
        .map(|(param, value)| (param.name.clone(), published_value(value, published)))
        .collect()
}

/// `value` with each resource it references (`{"ref", "ty"}`, as a stamp records it) named as
/// `published` names it.
fn published_value(value: &Json, published: &dyn Fn(&str, &str) -> Option<Json>) -> Json {
    match value {
        Json::Object(fields) => {
            if let (Some(Json::String(id)), Some(Json::String(ty)), 2) =
                (fields.get("ref"), fields.get("ty"), fields.len())
                && let Some(named) = published(id, ty)
            {
                return named;
            }
            Json::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), published_value(value, published)))
                    .collect(),
            )
        }
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| published_value(item, published))
                .collect(),
        ),
        other => other.clone(),
    }
}

impl Opened {
    /// Open the record `store` holds through `packages` for `viewer`: the member as whom the
    /// platform's own writes are made. `policy` is the workspace's, as this device reads it:
    /// the host's members and who of them reads what (`workspace.members`, `std.may_read`,
    /// [`Self::readers`]). Without one, the host knows no members, and nobody reads anything.
    ///
    /// # Errors
    /// The host or a package does not load.
    pub fn open(
        store: ResourceStore,
        packages: &Packages,
        viewer: Actor,
        policy: Option<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>,
    ) -> Result<Self, String> {
        Self::open_with(store, packages, viewer, None, policy)
    }

    /// [`Self::open`] with the access its own authority decides invocations by given: the
    /// workspace's policy as the actor runs it, for a program with its own grants; `None` leaves
    /// its authority open, as code on this device that acts for the workspace does. `members` is
    /// the policy the host's members and their reads are read from.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn open_with(
        store: ResourceStore,
        packages: &Packages,
        viewer: Actor,
        access: Option<std::sync::Arc<tilo_action_policy::MemberAccess>>,
        members: Option<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>,
    ) -> Result<Self, String> {
        Self::open_loading(store, (viewer, access, members), packages.app(), |host| {
            // The platform's packages the app uses are the session's, held here by the types
            // they publish, as a window holds them.
            for meta in packages.1.iter() {
                host.runtime()
                    .load_foreign(meta.clone())
                    .map_err(|error| format!("{} does not load: {error:?}", meta.app))?;
            }
            for package in packages.0.iter() {
                host.load_compiled(package)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }

    /// [`Self::open_with`] for `app`'s record, its packages loaded by `load`: the built-in ones,
    /// or the package a device installed
    /// ([`crate::workspaces::Workspaces::record_entries`]).
    fn open_loading(
        store: ResourceStore,
        (viewer, access, members): (
            Actor,
            Option<std::sync::Arc<tilo_action_policy::MemberAccess>>,
            Option<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>,
        ),
        app: &str,
        load: impl FnOnce(&Host) -> Result<(), String>,
    ) -> Result<Self, String> {
        // What the host's own authority decides invocations by: without an access it knows no
        // grants, and leaves every action to the caller.
        let host = Host::open(store, viewer, None, move || {
            access.as_ref().map(|access| (**access).clone())
        })
        .map_err(|error| error.to_string())?;
        load(&host)?;
        if let Some(policy) = members
            && let Some(owner) = policy.owner.clone()
        {
            let me = tilo_action_policy::PersonId::parse(&owner).map_err(|error| {
                format!("the workspace's owner {owner:?} is no person: {error}")
            })?;
            // Each agent the workspace holds as a member acts for the person its entry names,
            // with that person's authority bounded by the agent's delegation: an ask taken, a
            // reply written or a step of a turn recorded here is the agent's action, decided as
            // it is where the agent's own device does the same.
            for (agent, member) in &policy.agents {
                host.authority().admit_agent(
                    &Actor::Member(tilo_actions::MemberId::agent(agent.clone())),
                    Some(member.for_person.as_str()),
                );
            }
            // The owner stands at no window here: whoever the policy names, the host names the
            // same.
            let access = tilo_action_policy::MemberAccess::person(policy, me.clone());
            host.runtime().set_members(
                tilolang_tilo::members::members_of(&access, &me),
                Some(tilolang_tilo::members::reads_of(&access, &me, &host)),
            );
        }
        Ok(Self {
            host,
            app: app.to_owned(),
            run_of: None,
        })
    }

    /// The host the record is read through: [`Host::runtime`] reads any field of any resource,
    /// [`Host::store`] its history and links.
    #[must_use]
    pub const fn host(&self) -> &Host {
        &self.host
    }

    /// The app the record is of.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The members of the workspace who may read `at`, as its policy decides
    /// (`std.may_read`): none where the record was opened without the policy.
    #[must_use]
    pub fn readers(&self, at: &ResourceRef) -> Vec<Actor> {
        let runtime = self.host.runtime();
        runtime
            .members()
            .iter()
            .map(tilolang_tilo::runtime::members::Member::who)
            .filter(|who| runtime.may_read(who, at))
            .collect()
    }

    /// Every version of the record's resources, oldest first, each as its history lists it
    /// ([`tilolang_service::version_entry`]). A version of a resource not every
    /// member may read names the members who may as `readers`, the way a change only some
    /// members read is listed, so that what passes it on tells it to them alone: the store's
    /// versions as a feed of the workspace's changes reads them.
    ///
    /// # Errors
    /// The store did not read a resource's versions.
    pub fn changes(&self) -> Result<Vec<Json>, String> {
        let planner = self.planner(None);
        let trees = Publications::new(&self.host).everything(&self.host, &planner);
        let mut versions = Vec::new();
        for tree in &trees {
            let app = tilolang_tilo::publish::app_of(tree.id.as_str())
                .unwrap_or(&self.app)
                .to_owned();
            versions.extend(
                tilolang_service::versions(&self.host, tree)?
                    .into_iter()
                    .map(|version| (app.clone(), version)),
            );
        }
        versions.sort_by(|(_, a), (_, b)| a.order().cmp(&b.order()));
        let everyone = self.host.runtime().members().len();
        let store = self.host.store();
        Ok(versions
            .into_iter()
            .map(|(app, version)| {
                let mut entry = tilolang_service::version_entry(&self.host, &app, &version);
                let readers = store
                    .type_of_id(&version.resource)
                    .and_then(|ty| ResourceRef::parse_id(&version.resource, ty))
                    .map(|at| self.readers(&at));
                if let Some(readers) = readers.filter(|readers| readers.len() < everyone)
                    && let Some(fields) = entry.as_object_mut()
                {
                    let named: Vec<Json> = readers
                        .iter()
                        .filter_map(|who| match who {
                            Actor::Member(member) => {
                                Some(Json::String(tilo_actions::contract::reader_name(member)))
                            }
                            _ => None,
                        })
                        .collect();
                    fields.insert("readers".to_owned(), Json::Array(named));
                }
                entry
            })
            .collect())
    }

    /// Mark what is invoked from here on as a run of the Shortcut `shortcut` (an event, a
    /// reminder) acting for its person: each change it makes is a step of that run, so what
    /// reads the workspace's changes knows its own output by it rather than by who it acted as.
    pub fn in_run_of(&mut self, shortcut: impl Into<String>) {
        self.run_of = Some(shortcut.into());
    }

    /// Run `act`, what is written while it runs a step of `run` (or of the Shortcut this was
    /// marked a run of), and fail with what the store refused of it.
    fn writing<R>(
        &self,
        run: Option<Run>,
        act: impl FnOnce() -> Result<R, String>,
    ) -> Result<R, String> {
        let store = self.host.store();
        let run = run.or_else(|| {
            self.run_of.clone().map(|shortcut| Run {
                id: shortcut,
                title: String::new(),
            })
        });
        let ran = run.is_some();
        if ran {
            store.set_run(run);
        }
        let answer = act();
        if ran {
            store.set_run(None);
        }
        let answer = answer?;
        match store.take_refusal() {
            Some(refused) => Err(refused),
            None => Ok(answer),
        }
    }

    /// Run `app`'s action `action` with `args` as `who`, made from `from` when given (what it
    /// creates is made from that resource: `created(r)?.from`). Answers what the action returned.
    ///
    /// # Errors
    /// The package has no such action, the action was refused or failed, or the workspace
    /// refused what it wrote.
    pub fn invoke(
        &mut self,
        who: &Actor,
        (app, action): (&str, &str),
        args: Vec<Value>,
        from: Option<&ResourceRef>,
    ) -> Result<Value, String> {
        let package = self
            .host
            .package(app)
            .ok_or_else(|| format!("{app} is not loaded here"))?;
        let id = package
            .meta
            .actions
            .iter()
            .find(|declared| declared.name == action)
            .map(|declared| declared.id.clone())
            .ok_or_else(|| format!("{app} has no action {action}"))?;
        let runtime = self.host.runtime();
        let outcome = self.writing(None, || {
            match from {
                Some(from) => runtime.invoke_action_from(who, (id.as_str(), args), from),
                None => runtime.invoke_action(who, (id.as_str(), args)),
            }
            .map_err(|error| error.to_string())
        })?;
        match outcome {
            InvokeOutcome::Done(value) => Ok(value),
            InvokeOutcome::Denied => Err(format!("{app}#{action} was refused")),
            InvokeOutcome::Asked(_) | InvokeOutcome::Waits { .. } => {
                Err(format!("{app}#{action} waits for an answer"))
            }
        }
    }

    /// The app's root resource, made the first time it is wanted, as a launcher opening the app
    /// makes it: what the app's own actions are published on.
    ///
    /// # Errors
    /// The package has no root, or the workspace refused making it.
    pub fn open_root(&mut self) -> Result<ResourceRef, String> {
        self.writing(None, || {
            self.host
                .root_of(&self.app)
                .map_err(|error| error.to_string())
        })
    }

    /// What plans an action of the record for `invoker`, or for the host's viewer.
    fn planner(&self, invoker: Option<&ActionInvoker>) -> Arc<dyn Planner> {
        let handle = self.host.handle();
        match invoker.map(Caller::from) {
            Some(caller) => Arc::new(handle.with_caller(move || Some(caller.clone()))),
            None => Arc::new(handle),
        }
    }

    /// Every resource of the record as the action API publishes it, its app's own actions
    /// beside them, each planned by `planner`.
    fn published(&self, planner: &Arc<dyn Planner>) -> Vec<ResourceSpec> {
        let mut out: Vec<ResourceSpec> = Publications::new(&self.host)
            .everything(&self.host, planner)
            .into_iter()
            .map(std::rc::Rc::unwrap_or_clone)
            .collect();
        out.extend(self.own(planner));
        out
    }

    /// The resources of the app's own packages, published after the record's documents: what
    /// carries the actions that take no resource.
    fn own(&self, planner: &Arc<dyn Planner>) -> Vec<ResourceSpec> {
        let words = self.host.runtime().words();
        self.host
            .packages()
            .iter()
            .filter(|package| package.meta.app == self.app)
            .map(|package| publish_package(&package.meta, planner, &*words))
            .filter(|resource| !resource.actions.is_empty())
            .collect()
    }

    /// The events the changes of `doc`, the record's document `record` of `workspace`, made after
    /// `since` (hex heads) are, in causal order, each with the heads after it: what a program
    /// following the record answers. Each change an action stamped is one event, read from the
    /// store's versions: the stamp names the action and who invoked it, and the resource it acted
    /// on is one the change changed, or the one it put a new resource in, of a type the action
    /// targets.
    ///
    /// Changes no action stamped, and steps taken back, yield no event but still advance the
    /// heads, so a cursor set from the last heads never reads them again.
    #[must_use]
    pub fn events_since(
        &self,
        workspace: &str,
        record: &str,
        doc: &automerge::Automerge,
        since: &[String],
    ) -> Vec<(DocumentEvent, Vec<String>)> {
        use tilolang_tilo::meta::MetaExt as _;
        let since = heads_since(since);
        let mut heads = since.clone();
        let package = self.host.package(&self.app);
        let store = self.host.store();
        let mut events = Vec::new();
        for change in doc.get_changes(&since) {
            heads.retain(|head| !change.deps().contains(head));
            heads.push(change.hash());
            let Some(stamp) = tilo_resource_store::Stamp::of(&change) else {
                continue;
            };
            if stamp.verb.is_empty() || stamp.undoes.is_some() {
                continue;
            }
            let Some(touched) = tilo_resource_store::touched_by(doc, record, &change) else {
                continue;
            };
            let action = package.as_ref().and_then(|package| {
                package
                    .meta
                    .actions
                    .iter()
                    .find(|declared| declared.id == stamp.verb)
                    .map(|declared| (&package.meta, declared))
            });
            let targets: Vec<String> = action
                .map(|(meta, declared)| {
                    meta.targets_of(&declared.params)
                        .into_iter()
                        .map(|ty| meta.kind_of(&ty.name))
                        .collect()
                })
                .unwrap_or_default();
            // What each touched resource says the action acted on: a resource it made stands in
            // the one it was put in, a resource it changed for itself.
            let acted: Vec<String> = touched
                .resources
                .iter()
                .filter_map(|(id, touch)| {
                    if touch.made {
                        touch
                            .parent
                            .as_deref()
                            .and_then(|parent| store.kind_of_id(parent))
                    } else {
                        touch.kind.clone().or_else(|| store.kind_of_id(id))
                    }
                })
                .collect();
            // What the change names it acted on, by the types of the resources its arguments
            // reference: a resource it removed or moved out of its place is no longer read
            // where the document held it, so the reference says what it was.
            let named: Vec<String> = stamp
                .args
                .iter()
                .filter_map(|arg| store.kind_of_type_id(arg.get("ty")?.as_str()?))
                .collect();
            let Some(kind) = acted
                .iter()
                .find(|kind| targets.contains(kind))
                .or_else(|| named.iter().find(|kind| targets.contains(kind)))
                .or_else(|| acted.first())
                .cloned()
            else {
                continue;
            };
            let name =
                action.map_or_else(|| stamp.verb.clone(), |(_, declared)| declared.name.clone());
            let item = touched
                .resources
                .iter()
                .find(|(_, touch)| touch.made)
                .map(|(id, touch)| tilo_automation::dispatch::ItemRef {
                    kind: touch.kind.clone().unwrap_or_default(),
                    id: id.clone(),
                    label: String::new(),
                });
            let actor = stamp
                .invoked_by()
                .map_or_else(|| tilo_actions::Acting::person(String::new()), Into::into);
            let published = |id: &str, ty: &str| {
                let kind = store.kind_of_type_id(ty)?;
                Some(serde_json::json!({ "id": resource_id(&self.app, id), "kind": kind }))
            };
            let arguments = action.map_or_else(Map::new, |(meta, declared)| {
                named_arguments(meta, declared, &stamp.args, &published)
            });
            events.push((
                DocumentEvent {
                    workspace: workspace.to_owned(),
                    record: record.to_owned(),
                    app: self.app.clone(),
                    did: tilo_actions::entry::Did::verb(
                        tilo_actions::ActionKey::new(&self.app, &kind, &name),
                        [],
                    ),
                    kind,
                    change: change.hash().to_string(),
                    actor,
                    arguments,
                    item,
                    before: None,
                    after: None,
                    caused_by: None,
                    origin: None,
                    incoming_message: false,
                    housekeeping: false,
                    shortcut: None,
                    invocation: stamp.group.map(|group| group.id),
                },
                heads.iter().map(ToString::to_string).collect(),
            ));
        }
        events
    }

    /// Every action the record's resources publish, each in the row of the resource holding it,
    /// as a catalogue lists them.
    #[must_use]
    pub fn rows(&self) -> Vec<CatalogueRow> {
        let resources = self.published(&self.planner(None));
        tilo_actions::api::survey_placed(&resources)
            .into_iter()
            .map(|placed| CatalogueRow::for_placed_action("", &self.app, placed))
            .collect()
    }

    /// The page of [`Self::rows`] `query` asks for: the rows it admits from its cursor on, at
    /// most its page size, and a cursor past them while more follow. What a caller listing the
    /// record a level at a time is answered.
    ///
    /// A page publishes the level it lists and not the record: the resources directly under
    /// `query`'s parent (or the top), and of them the ones whose rows it answers, so a page of
    /// 40 rows under ten thousand tasks costs about 40 rows. A query that narrows by nothing a
    /// row says counts the rows before its cursor by resource without publishing them; one that
    /// narrows by its words, a resource's name or a view publishes each resource of the level
    /// to ask, still never the rest of the record. Only a query for one exact action (`target`,
    /// `action`) publishes the whole record: it finds the action under whichever parent holds it.
    #[must_use]
    pub fn page(&self, query: &CatalogueQuery) -> CataloguePage {
        let (offset, size) = (query.offset(), query.page_size());
        if query.target.is_some() || query.action.is_some() {
            let mut admitted = self
                .rows()
                .into_iter()
                .filter(|row| query.admits(row))
                .skip(offset);
            let rows: Vec<CatalogueRow> = admitted.by_ref().take(size).collect();
            let more = admitted.next().is_some();
            return CataloguePage::window(String::new(), rows, query, more);
        }
        // Every row the record publishes is its app's.
        if query.app_id.as_deref().is_some_and(|app| app != self.app) {
            return CataloguePage::window(String::new(), Vec::new(), query, false);
        }
        let planner = self.planner(None);
        // An empty parent names the top, as it does for the rows a query admits.
        let parent = query
            .parent_resource
            .as_deref()
            .filter(|parent| !parent.is_empty());
        let mut level = Publications::new(&self.host).level(&self.host, &planner, parent);
        if parent.is_none() {
            for resource in self.own(&planner) {
                level.then(resource);
            }
        }
        let every = query.needle.is_empty()
            && query.resource_name.is_none()
            && query.view_resource.is_none();
        let (rows, more) = level.page(&self.host, (offset, size), every, &mut |placed| {
            let row = CatalogueRow::for_placed_action("", &self.app, placed);
            query.admits(&row).then_some(row)
        });
        CataloguePage::window(String::new(), rows, query, more)
    }

    /// Run the published action `address` with `arguments` as `invoker`: found among the
    /// record's published resources, its arguments typed by its declared parameters, run by its
    /// own code as it is planned, and written as a step of the work `context` groups it in,
    /// under the invocation `context` names. A `read` answers the core read, as `invoker` reads.
    ///
    /// # Errors
    /// As any call of a published action: unknown, unavailable, invalid arguments, refused.
    pub fn invoke_published(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Json>,
        invoker: &ActionInvoker,
        context: &ActionContext,
    ) -> Result<ActionOutput, ApiError> {
        let resources = self.published(&self.planner(Some(invoker)));
        let store = self.host.store();
        let run = context.group.as_ref().map(|group| Run {
            id: group.id.clone(),
            title: group.label.clone().unwrap_or_default(),
        });
        // What the action writes is stamped with the invocation it serves, so the workspace's
        // history tells the step by the call that made it.
        store.serve(
            context
                .source_invocation
                .map(|invocation| invocation.0.to_string()),
        );
        let encoded = self.writing(run, || {
            Ok(self.host.carrying_out(&invoker.acting.by, || {
                tilo_actions::api::encode_json(&resources, address, arguments)
            }))
        });
        store.serve(None);
        let encoded = encoded.map_err(ApiError::Failed)??;
        if encoded.type_id == tilo_resource::read::TYPE {
            let args = if encoded.payload.is_empty() {
                Json::Null
            } else {
                tilo_actions::decode_json(&encoded.payload)
                    .map_err(|error| ApiError::InvalidArguments(error.to_string()))?
            };
            return self
                .host
                .answer_read(&args, &Caller::from(invoker).reader())
                .map(|value| ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                })
                .map_err(ApiError::Failed);
        }
        if encoded.type_id != ACTION_TYPE {
            return Err(ApiError::Unknown(address.clone()));
        }
        let action: DynAction = serde_json::from_slice(&encoded.payload)
            .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
        let output = self
            .host
            .take_returned(&action.action_id)
            .map(|value| {
                self.host
                    .action_output(&action.app, &action.action_id, &value)
            })
            .transpose()
            .map_err(ApiError::Failed)?;
        Ok(output.unwrap_or_default())
    }
}

/// The member `id` names, as a tilolang actor.
#[must_use]
pub fn member(id: tilo_actions::MemberId) -> Actor {
    Actor::Member(id)
}

/// One published action of a tilolang record of a workspace, as a host applies it: the app, who
/// reads and invokes it, the action and its arguments, and whether it only reads.
pub struct RecordCall<'a> {
    /// The record's app, as its package and its resources name it.
    pub app: &'a str,
    /// The person the action's reads answer, and as whom it is written.
    pub viewer: Actor,
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

impl crate::workspaces::Workspaces {
    /// `app`'s record of `workspace`, as this device's workspace service holds it, its commands
    /// waiting on the reactor this is called on.
    ///
    /// # Errors
    /// This is called outside a reactor.
    pub fn tl_record(&self, workspace: &str, app: &str) -> Result<Record, String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("{app}'s record is read on a reactor: {error}"))?;
        Ok(Record {
            workspaces: self.clone(),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            runtime: Some(runtime),
            refused: Arc::default(),
        })
    }

    /// What `app` keeps in `workspace` publishes to search, read from the workspace's resource
    /// store as `person` reads it: every entry a window of the app publishes
    /// ([`tilolang_tilo::search::whole`]), so search finds the app's resources on a device that
    /// never opened a window of it. The app is read through `installed`, its package as this
    /// device installed it and verified as a launch verifies it (what its windows run here), and
    /// without one through the package built into this build ([`built_in`]).
    ///
    /// The record is opened as that person, under the workspace's policy as this device reads
    /// it, so what a direct conversation keeps is found only by the people it is between.
    /// Nothing is written: what a search reads leaves the record as it was.
    ///
    /// # Errors
    /// No package of `app` is installed or built in, the package does not load, the workspace's
    /// policy does not read here, or the record does not open.
    pub fn record_entries(
        &self,
        workspace: &str,
        app: &str,
        installed: Option<&runtime_portal::process_link::Module>,
        person: &str,
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String> {
        let record = self.record_to_read(workspace, app).store();
        let viewer = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
        // Without the policy nobody's reads are known, and every resource would read as anyone's.
        let policy = self
            .policy_now(workspace)
            .map(Arc::new)
            .ok_or_else(|| format!("{workspace}'s policy does not read here"))?;
        let opened = match installed {
            Some(module) => {
                let bundle = tilolang_tilo::bundle::Bundle::from_parts(
                    module.wasm.clone(),
                    module.ids.clone(),
                    &module.imports,
                )
                .map_err(|error| format!("{app}'s package does not read: {error}"))?;
                if bundle.meta.app != app {
                    return Err(format!(
                        "{app} is installed as the package of {}",
                        bundle.meta.app
                    ));
                }
                Opened::open_loading(record, (viewer, None, Some(policy)), app, |host| {
                    host.load_bundle(&bundle)
                        .map(drop)
                        .map_err(|error| format!("{app} does not load: {error}"))
                })?
            }
            None => {
                let packages = built_in(app)?.ok_or_else(|| runs_elsewhere(app))?;
                Opened::open_with(record, &packages, viewer, None, Some(policy))?
            }
        };
        Ok(tilolang_tilo::search::whole(opened.host()))
    }

    /// What `route`'s record of `workspace` publishes, each action in the row of the resource
    /// holding it, read as `person` reads it through its app's built-in package: what a caller
    /// is offered of a tilolang app no catalogue holds and no `.tapp` installs. `None` for an app
    /// this build runs no package of, or a record that does not open. Nothing is written.
    pub fn record_rows(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
    ) -> Option<Vec<CatalogueRow>> {
        Some(self.opened_to_read(workspace, route, person)?.rows())
    }

    /// The page of what `route`'s record of `workspace` publishes that `query` asks for, read as
    /// `person` reads it ([`Opened::page`]). `None` as for [`Self::record_rows`].
    pub fn record_page(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
        query: &CatalogueQuery,
    ) -> Option<CataloguePage> {
        Some(self.opened_to_read(workspace, route, person)?.page(query))
    }

    /// Apply `call`, a published action of an app's record of `workspace`, through the app's
    /// built-in package. A read answers the core read as the person the caller acts for reads
    /// it, from a record that takes no write. A write reaches the workspace's store as it is
    /// made ([`Self::record_command`]), stamped with the caller as its invoker: whether they may
    /// was decided under their grants by the host that admitted the call.
    ///
    /// # Errors
    /// This build carries no package of the app, the record does not open, the action is
    /// unknown, refused or fails, or the workspace refused what it wrote.
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
        let packages =
            built_in(app)
                .map_err(ApiError::Failed)?
                .ok_or_else(|| ApiError::Unavailable {
                    code: "runs_elsewhere".into(),
                    reason: runs_elsewhere(app),
                })?;
        let record = if read {
            self.record_to_read(workspace, app)
        } else {
            self.tl_record(workspace, app).map_err(ApiError::Failed)?
        };
        let policy = self
            .policy_of(workspace)
            .await
            .map(Arc::new)
            .ok_or_else(|| ApiError::Failed(format!("{workspace}'s policy does not read here")))?;
        // The record's own authority decides the action as the caller: this device writes it,
        // so nothing else stops an agent from doing what its delegation does not give it.
        let acting = caller.acting();
        let kept = match acting.agent_id() {
            Some(agent) => self
                .kept_bound(workspace, agent, caller.turn().is_some())
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let access = Arc::new(
            tilo_action_policy::MemberAccess::new(Arc::clone(&policy), acting.clone())
                .granted(caller.granted(&kept)),
        );
        let (invoker, context) = (invoker.clone(), context.clone());
        let (address, arguments) = (address.clone(), arguments.clone());
        tokio::task::spawn_blocking(move || {
            Opened::open_with(
                record.store(),
                &packages,
                viewer,
                Some(access),
                Some(policy),
            )
            .map_err(ApiError::Failed)?
            .invoke_published(&address, &arguments, &invoker, &context)
        })
        .await
        .map_err(|error| ApiError::Failed(error.to_string()))?
    }

    /// The changes of `route`'s record of `workspace`, each version of its resources as its
    /// history lists it, read through its app's built-in package ([`Opened::changes`]). `None`
    /// for an app this build runs no package of, or a record that does not read now. Nothing is
    /// written.
    pub fn record_changes(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
    ) -> Option<Vec<Json>> {
        let owner = self.policy_now(workspace)?.owner?;
        let opened = self.opened_to_read(workspace, route, &owner)?;
        match opened.changes() {
            Ok(changes) => Some(changes),
            Err(error) => {
                tracing::warn!(app = %route.app_id, %error, "a record's versions do not read");
                None
            }
        }
    }

    /// `app`'s record of `workspace` to be read on this thread, waiting as a search does: it
    /// takes no write.
    fn record_to_read(&self, workspace: &str, app: &str) -> Record {
        Record {
            workspaces: self.clone(),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            runtime: None,
            refused: Arc::default(),
        }
    }

    /// `route`'s record of `workspace` opened on this thread through its app's built-in package
    /// as `person` reads it, taking no write. `None` for an app this build runs no package of,
    /// or a record that does not open.
    fn opened_to_read(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
    ) -> Option<Opened> {
        let packages = match built_in(&route.app_id) {
            Ok(packages) => packages?,
            Err(error) => {
                tracing::warn!(app = %route.app_id, %error, "a built-in package does not compile");
                return None;
            }
        };
        let record = self.record_to_read(workspace, &route.app_id);
        let viewer = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
        let policy = self.policy_now(workspace).map(Arc::new);
        match Opened::open(record.store(), &packages, viewer, policy) {
            Ok(opened) => Some(opened),
            Err(error) => {
                tracing::warn!(app = %route.app_id, %error, "a record read does not open");
                None
            }
        }
    }

    /// Open `packages`' app's record of `workspace` for `viewer` and run `work` on it: what
    /// `work` answered. Each write `work` makes reaches the workspace as it is made, so a write
    /// the workspace refuses fails `work` and leaves what came before it standing.
    ///
    /// # Errors
    /// The record could not be opened, or `work` failed.
    pub async fn with_tl_record<R: Send + 'static>(
        &self,
        workspace: &str,
        packages: &Packages,
        viewer: Actor,
        work: impl FnOnce(&mut Opened) -> Result<R, String> + Send + 'static,
    ) -> Result<R, String> {
        let record = self.tl_record(workspace, packages.app())?;
        let packages = packages.clone();
        let policy = self.policy_of(workspace).await.map(std::sync::Arc::new);
        tokio::task::spawn_blocking(move || {
            let mut opened = Opened::open(record.store(), &packages, viewer, policy)?;
            work(&mut opened)
        })
        .await
        .map_err(|error| error.to_string())?
    }
}
