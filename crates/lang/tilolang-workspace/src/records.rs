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
//! - **Opened** ([`Opened`]): a tilolang host over a record's store on the calling thread. It
//!   reads any resource ([`Opened::host`]), and runs any action of a loaded package as any member
//!   ([`Opened::invoke`]): the action's own code runs it, and what it writes goes to the store
//!   stamped with that member as its invoker.
//!
//! The store a host reads and writes is the workspace service's
//! ([`tilo_workspace_service::tl_records::Record`]), held on the workspace service and taken as
//! the port the runtime's store sends each command to. [`with_tl_record`] opens a record for one
//! call. A host is bound to its thread, so the call runs on a blocking thread of its own and
//! hands back only what the caller's closure returns.
//!
//! Only the packages built into this build ([`built_in`]) run here; an app a workspace installed
//! is untrusted code and runs only on a device that installed it: its windows there, and what
//! search reads of it with none open, given that device's verified package.
//!
//! Who may do what is decided by the host's authority, from the workspace's policy as the
//! acting member holds it: an agent within its delegation, a bot within its grants
//! ([`Opened::open_with`]). The caller names the member it acts as; code on this device that
//! calls here is the device's own.

pub mod provided;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value as Json};
use tilo_action_policy::{MemberAccess, WorkspacePolicy};
use tilo_actions::remote::{CataloguePage, CatalogueQuery, CatalogueRow};
use tilo_actions::{
    ActionAddress, ActionContext, ActionInvoker, ActionOutput, ApiError, ResourceSpec,
};
use tilo_resource::search_index::IndexEntry;
use tilo_workspace_service::tl_records::Records;
use tilo_workspace_service::workspaces::Workspaces;

use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{Caller, Planner, Publications, publish_package};
use tilolang_tilo::runtime::store::Run;
use tilolang_tilo::runtime::{Actor, InvokeOutcome, ResourceRef, Value};

pub use tilo_workspace_service::tl_records::{
    Package, Raised, Record, RecordCall, Wakeup, member, runs_elsewhere,
};
pub use tilolang_tilo::resources::{LocalPort, Port, ResourceStore};
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

    /// The `surface.toml` of `app` among the packages: the types and verbs a workspace's store
    /// judges the app's records' writes by.
    #[must_use]
    pub fn surface(&self, app: &str) -> Option<Vec<u8>> {
        self.surfaces()
            .into_iter()
            .find(|(named, _)| named == app)
            .map(|(_, bytes)| bytes)
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

    /// Each package's surface file, as `tilolang build` writes it for an installed app: the
    /// types and verbs a workspace's store judges the record's writes by, each package reading
    /// those it uses.
    #[must_use]
    pub fn surfaces(&self) -> Vec<(String, Vec<u8>)> {
        self.0
            .iter()
            .filter_map(|package| {
                let reads: Vec<_> = package
                    .meta
                    .uses
                    .iter()
                    .filter_map(|app| {
                        self.0
                            .iter()
                            .map(|used| &used.meta)
                            .chain(self.1.iter())
                            .find(|meta| &meta.app == app)
                            .cloned()
                    })
                    .collect();
                let bytes = tilolang_tilo::surface::surface_toml_reading(
                    &[&package.meta],
                    &reads,
                    &tilolang_tilo::runtime::SourceWords,
                )
                .ok()?;
                Some((package.meta.app.clone(), bytes))
            })
            .collect()
    }

    /// The name of each field of the type `ty` one of the packages declares, by the id the
    /// record keeps it under: how a value the record holds whole is read by its names.
    #[must_use]
    pub fn field_names(&self, ty: &str) -> BTreeMap<String, String> {
        self.0
            .iter()
            .flat_map(|package| &package.meta.types)
            .find(|declared| declared.name == ty)
            .map(|declared| {
                declared
                    .fields
                    .iter()
                    .map(|field| (field.id.clone(), field.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
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
    &[(
        include_str!("../../../../apps/tl/discord/app.tl"),
        include_str!("../../../../apps/tl/discord/app.ids"),
    )],
    // A device's daemon arms the alarms set for it from Clocks' record, window or no window.
    &[(
        include_str!("../../../../apps/tl/clocks/app.tl"),
        include_str!("../../../../apps/tl/clocks/app.ids"),
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
#[expect(
    clippy::expect_used,
    reason = "a built-in package that does not compile is a bug of this build"
)]
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

/// An app's record opened on this thread: a host over it with its packages loaded.
pub struct Opened {
    host: Host,
    app: String,
    /// The Shortcut whose run what is invoked here is ([`Opened::in_run_of`]).
    run_of: Option<String>,
}

impl Opened {
    /// Open the record `store` holds through `packages` for `viewer`: the member as whom the
    /// platform's own writes are made. `policy` is the workspace's, as this device reads it:
    /// the host's members and who of them reads what (`members(r)`, `std.may_read`,
    /// [`Self::readers`]). Without one, the host knows no members, and nobody reads anything.
    ///
    /// # Errors
    /// The host or a package does not load.
    pub fn open(
        store: ResourceStore,
        packages: &Packages,
        viewer: Actor,
        policy: Option<Arc<WorkspacePolicy>>,
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
        access: Option<Arc<MemberAccess>>,
        members: Option<Arc<WorkspacePolicy>>,
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
    /// or the package a device installed.
    pub(crate) fn open_loading(
        store: ResourceStore,
        (viewer, access, members): (
            Actor,
            Option<Arc<MemberAccess>>,
            Option<Arc<WorkspacePolicy>>,
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
        // Whom a resource's people field names reads it as the access decides: the store's
        // reader decision follows it, as a window's host does each frame.
        host.follow_members();
        if let Some(policy) = members
            && let Some(owner) = policy.owner.clone()
        {
            let me = tilo_action_policy::PersonId::parse(&owner).map_err(|error| {
                format!("the workspace's owner {owner:?} is no person: {error}")
            })?;
            // The owner stands at no window here: whoever the policy names, the host names the
            // same.
            let access = MemberAccess::person(policy, me.clone());
            host.give_members(&access, &me);
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
    /// ([`tilolang_tilo::versions::version_entry`]). A version of a resource not every
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
                tilolang_tilo::versions::versions(&self.host, tree)?
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
                let mut entry = tilolang_tilo::versions::version_entry(&self.host, &app, &version);
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
        // The change keeps the call its caller signed, as a change made for a caller must:
        // every holder checks that caller asked for it, and with which key.
        store.serve_signed(context.signed.clone());
        let encoded = self.writing(run, || {
            Ok(self.host.carrying_out(&invoker.acting.by, || {
                tilo_actions::api::encode_json(&resources, address, arguments)
            }))
        });
        store.serve(None);
        store.serve_signed(None);
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

/// The store a record's commands go to.
pub(crate) fn store(record: Record) -> ResourceStore {
    ResourceStore::new(Rc::new(record))
}

/// `record` opened through `package` for `viewer`, with `policy` the workspace's: the app's
/// built-in package, the module this device installed, or the packages a program compiled here.
///
/// # Errors
/// The app is neither built in nor installed here, or its package does not load.
pub(crate) fn opened_with(
    record: Record,
    policy: Option<Arc<WorkspacePolicy>>,
    viewer: Actor,
    app: &str,
    package: Option<Package>,
) -> Result<Opened, String> {
    match package {
        Some(Package::Installed(module)) => {
            let bundle =
                tilolang_tilo::bundle::Bundle::from_parts(module.wasm, module.ids, &module.imports)
                    .map_err(|error| format!("{app}'s package does not read: {error}"))?;
            if bundle.meta.app != app {
                return Err(format!(
                    "{app} is installed as the package of {}",
                    bundle.meta.app
                ));
            }
            Opened::open_loading(store(record), (viewer, None, policy), app, |host| {
                host.load_bundle(&bundle)
                    .map(drop)
                    .map_err(|error| format!("{app} does not load: {error}"))
            })
        }
        Some(Package::Compiled(compiled)) => {
            let packages: &Packages = compiled
                .as_language()
                .ok_or_else(|| format!("{app}'s package is another language's"))?;
            Opened::open(store(record), packages, viewer, policy)
        }
        Some(Package::BuiltIn) | None => {
            let packages = built_in(app)?.ok_or_else(|| runs_elsewhere(app))?;
            Opened::open(store(record), &packages, viewer, policy)
        }
    }
}

/// Open `packages`' app's record of `workspace` for `viewer`, reading what `reader` may read,
/// and run `work` on it: what `work` answered. Each write `work` makes reaches the workspace as
/// it is made, so a write the workspace refuses fails `work` and leaves what came before it
/// standing.
///
/// # Errors
/// The record could not be opened, or `work` failed.
pub async fn with_tl_record<R: Send + 'static>(
    workspaces: &Workspaces,
    workspace: &str,
    packages: &Packages,
    (viewer, reader): (Actor, &str),
    work: impl FnOnce(&mut Opened) -> Result<R, String> + Send + 'static,
) -> Result<R, String> {
    let record = workspaces.tl_record(workspace, packages.app(), reader)?;
    let packages = packages.clone();
    let policy = workspaces.policy_of(workspace).await.map(Arc::new);
    tokio::task::spawn_blocking(move || {
        let mut opened = Opened::open(store(record), &packages, viewer, policy)?;
        work(&mut opened)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The records of the apps built into this build, as a device's own roles read and change them.
impl Records for crate::Service {
    fn built_in(&self, app: &str) -> bool {
        built_in(app).is_ok_and(|packages| packages.is_some())
    }

    fn surface(&self, app: &str) -> Option<Vec<u8>> {
        built_in(app).ok().flatten()?.surface(app)
    }

    #[expect(
        clippy::expect_used,
        reason = "a built-in package that does not compile is a bug of this build"
    )]
    fn surfaces(&self) -> Vec<(String, Vec<u8>)> {
        compiled()
            .as_ref()
            .expect("the packages built into this build compile")
            .iter()
            .flat_map(Packages::surfaces)
            .collect()
    }

    fn field_names(&self, app: &str, ty: &str) -> BTreeMap<String, String> {
        built_in(app)
            .ok()
            .flatten()
            .map(|packages| packages.field_names(ty))
            .unwrap_or_default()
    }

    fn rows(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        app: &str,
    ) -> Option<Vec<CatalogueRow>> {
        let packages = match built_in(app) {
            Ok(packages) => packages?,
            Err(error) => {
                tracing::warn!(%app, %error, "a built-in package does not compile");
                return None;
            }
        };
        let viewer = Actor::Member(tilo_actions::MemberId::person(record.reader().to_owned()));
        match Opened::open(store(record), &packages, viewer, policy) {
            Ok(opened) => Some(opened.rows()),
            Err(error) => {
                tracing::warn!(%app, %error, "a record read does not open");
                None
            }
        }
    }

    fn page(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        app: &str,
        query: &CatalogueQuery,
    ) -> Option<CataloguePage> {
        let packages = match built_in(app) {
            Ok(packages) => packages?,
            Err(error) => {
                tracing::warn!(%app, %error, "a built-in package does not compile");
                return None;
            }
        };
        let viewer = Actor::Member(tilo_actions::MemberId::person(record.reader().to_owned()));
        match Opened::open(store(record), &packages, viewer, policy) {
            Ok(opened) => Some(opened.page(query)),
            Err(error) => {
                tracing::warn!(%app, %error, "a record read does not open");
                None
            }
        }
    }

    fn changes(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        app: &str,
    ) -> Option<Vec<Json>> {
        let packages = match built_in(app) {
            Ok(packages) => packages?,
            Err(error) => {
                tracing::warn!(%app, %error, "a built-in package does not compile");
                return None;
            }
        };
        let viewer = Actor::Member(tilo_actions::MemberId::person(record.reader().to_owned()));
        match Opened::open(store(record), &packages, viewer, policy) {
            Ok(opened) => match opened.changes() {
                Ok(changes) => Some(changes),
                Err(error) => {
                    tracing::warn!(%app, %error, "a record's versions do not read");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%app, %error, "a record read does not open");
                None
            }
        }
    }

    fn undo(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        acting: &tilo_actions::Acting,
        app: &str,
        run: (&str, &str),
        (invocations, call): (&[String], Option<&tilo_actions::remote::SignedCall>),
    ) -> Result<u32, String> {
        // The agent takes the changes back for the person it works for, as every write an agent
        // makes is made for them, and each change records how it acted.
        let viewer = &acting.by;
        let person = record.reader().to_owned();
        let opened = opened_with(record, policy, viewer.clone(), app, None)?;
        let store = opened.host().store();
        let _for = store.acting_for(Some(person), Some(acting.clone()));
        let (id, title) = run;
        store.set_run(Some(Run {
            id: id.to_owned(),
            title: title.to_owned(),
        }));
        // The undo serves the call that asked for it: it carries the call's invocation and
        // keeps what its caller signed, as every write made for a call does.
        store.serve(call.map(|call| call.signed.statement().invocation.0.to_string()));
        store.serve_signed(call.cloned().map(Box::new));
        let mut undone = 0_u32;
        let mut failure = None;
        for invocation in invocations {
            match store.undo(invocation, Some(viewer)) {
                Ok(_) => undone = undone.saturating_add(1),
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        store.set_run(None);
        store.serve(None);
        store.serve_signed(None);
        // What the workspace refused of the undo is the undo's answer, never a count.
        if let Some(refused) = store.take_refusal() {
            return Err(refused);
        }
        failure.map_or(Ok(undone), Err)
    }

    fn wrote(&self, version: &Json, who: &tilo_actions::MemberId) -> bool {
        // What the platform wrote on the person's behalf (`$create`, the record's root made when
        // they first used the app, and every other `$` write) is no action of theirs: taking it
        // back would take the record from everyone.
        let platforms = version
            .get("action")
            .and_then(Json::as_str)
            .is_some_and(|action| action.starts_with('$'));
        !platforms
            && version
                .get("who")
                .and_then(|wrote| {
                    tilo_failure::ignored!(
                        "a version whose who names nobody is no person's change",
                        serde_json::from_value::<Option<Actor>>(wrote.clone()).ok()
                    )
                })
                .flatten()
                .and_then(|wrote| wrote.member_id().cloned())
                .is_some_and(|wrote| &wrote == who)
    }

    fn entries(
        &self,
        record: Record,
        policy: Arc<WorkspacePolicy>,
        viewer: &Actor,
        app: &str,
        installed: Option<&runtime_portal::process_link::Module>,
    ) -> Result<Vec<IndexEntry>, String> {
        let opened = opened_with(
            record,
            Some(policy),
            viewer.clone(),
            app,
            installed.map(|module| Package::Installed(module.clone())),
        )?;
        Ok(tilolang_tilo::search::whole(opened.host()))
    }

    fn entries_of(
        &self,
        record: Record,
        policy: Arc<WorkspacePolicy>,
        viewer: &Actor,
        app: &str,
        document: &str,
        installed: Option<&runtime_portal::process_link::Module>,
    ) -> Result<Vec<IndexEntry>, String> {
        let opened = opened_with(
            record,
            Some(policy),
            viewer.clone(),
            app,
            installed.map(|module| Package::Installed(module.clone())),
        )?;
        Ok(tilolang_tilo::search::entries_in(opened.host(), document))
    }

    fn apply(
        &self,
        record: Record,
        policy: Arc<WorkspacePolicy>,
        access: Arc<MemberAccess>,
        viewer: Actor,
        app: &str,
        action: (ActionAddress, Map<String, Json>),
        call: (ActionInvoker, ActionContext),
    ) -> Result<ActionOutput, ApiError> {
        let (address, arguments) = action;
        let (invoker, context) = call;
        let packages =
            built_in(app)
                .map_err(ApiError::Failed)?
                .ok_or_else(|| ApiError::Unavailable {
                    code: "runs_elsewhere".into(),
                    reason: runs_elsewhere(app),
                })?;
        Opened::open_with(store(record), &packages, viewer, Some(access), Some(policy))
            .map_err(ApiError::Failed)?
            .invoke_published(&address, &arguments, &invoker, &context)
    }

    fn serve(&self, workspaces: &Workspaces) {
        provided::register(workspaces);
    }

    fn notices(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        app: &str,
        package: Option<&Package>,
    ) -> Result<Vec<Raised>, String> {
        let opened = crate::notices::open(record, policy, person, app, package.cloned())?;
        crate::notices::raised(&opened, person)
    }

    fn notice_seen(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        app: &str,
        package: Option<&Package>,
        doc: &str,
    ) -> Result<(), String> {
        let opened = crate::notices::open(record, policy, person, app, package.cloned())?;
        crate::notices::seen(&opened, person, doc)
    }

    fn act_on(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        (app, package): (&str, Option<&Package>),
        opens: (&str, &str),
        action: &str,
    ) -> Result<(), String> {
        let opened = crate::notices::open(record, policy, person, app, package.cloned())?;
        let at = ResourceRef::parse_id(opens.1, opens.0)
            .ok_or_else(|| format!("`{}` names no resource", opens.1))?;
        crate::notices::choose_on(&opened, person, &at, action)
    }

    fn wakeups(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        (person, device): (&str, &str),
        app: &str,
        package: Option<&Package>,
        now_s: i64,
    ) -> Result<Vec<Wakeup>, String> {
        let opened = crate::notices::open(record, policy, person, app, package.cloned())?;
        Ok(crate::notices::wakeups(&opened, person, device, now_s))
    }

    fn notice_action(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        person: &str,
        app: &str,
        package: Option<&Package>,
        doc: &str,
        action: &str,
    ) -> Result<(), String> {
        let opened = crate::notices::open(record, policy, person, app, package.cloned())?;
        crate::notices::choose(&opened, person, doc, action)
    }
}
