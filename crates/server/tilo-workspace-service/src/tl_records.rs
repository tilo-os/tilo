//! A tilolang app's record of a workspace, held on a device beside the workspace's documents:
//! read through the app's own package, changed through its own actions as a given member, and
//! the change written into the workspace as that member's write, signed by this device.
//!
//! This is what a device's own roles use when they act on tilolang resources with no window of
//! the app open: the agent engine answering an ask ([`crate::asks`]), a bridge posting into a
//! channel, a moderator reading reports. Nothing here is per app.
//!
//! - **Packages** ([`Packages`]): the packages a record is read through, compiled once and kept,
//!   in the order they load (what an app uses first, the app last).
//! - **The record** ([`RecordBytes`]): an app's document of the workspace and the chunks above
//!   it, as the workspace holds them ([`crate::workspaces::Workspaces::tl_record`]).
//! - **Opened** ([`Opened`]): a tilolang host over those bytes on the calling thread. It reads any
//!   resource ([`Opened::host`]), and runs any action of a loaded package as any member
//!   ([`Opened::invoke`]): the action's own code plans it, its edits are applied where the
//!   record's manifest places them, and the host takes them in. What a mention in written text
//!   asks is made as the host's viewer, as on any device ([`tilolang_tilo::host::Host::ask_from_mentions`]).
//! - **Written** ([`Opened::written`]): what the actions wrote, one change per document, which
//!   [`crate::workspaces::Workspaces::write_tl`] writes as this device's own: signed, sealed under the
//!   document's group where the workspace seals, and committed through the one commit path.
//!
//! [`crate::workspaces::Workspaces::with_tl_record`] does all three for one call. A host is
//! bound to its thread, so the call runs on a blocking thread of its own and hands back only what
//! the caller's closure returns.
//!
//! A program a device runs (a bot, a person's event) holds an app's record as [`ProgramRecord`]:
//! its resources published as every tilolang resource is ([`Opened::rows`]), each step a call of
//! a published action ([`Opened::invoke_published`]), each read the core read. Only the packages
//! built into this build ([`built_in`]) run here; an app a workspace installed is untrusted code
//! and runs only on a device that has it open.
//!
//! Who may do what is decided where every member's write is decided: the workspace admits the
//! written change against the app's surface and the member's grants. The caller names the member
//! it acts as; code on this device that calls here is the device's own.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use automerge::ChangeHash;
use serde_json::{Map, Value as Json};
use tilo_actions::remote::CatalogueRow;
use tilo_actions::{
    ActionAddress, ActionContext, ActionFacet, ActionInvoker, ActionOutput, ApiError, InvocationId,
    ResourceSpec,
};
use tilo_agent_session::bot::held::TilolangRecord;
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::chunk::{self, ChunkNo, Chunks, FIRST};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{Caller, Planner, Publications, publish_package, resource_id};
use tilolang_tilo::runtime::{Actor, InvokeOutcome, ResourceRef, Value};

/// The app and type a tilolang resource kind (`tl.<app>.<Type>`) names.
pub use tilolang_tilo::catalogue::tilolang_kind;
/// The tilolang runtime's values, references and stores, as a record is read and changed with.
pub use tilolang_tilo::runtime;

/// The packages a record is read and changed through, compiled once: those the app uses first,
/// the app itself last.
#[derive(Clone)]
pub struct Packages(Arc<Vec<tilolang_tilo::tilolang::Compiled>>);

impl Packages {
    /// Compile `sources` (each a package's `app.tl` and its `app.ids`), each against those
    /// before it.
    ///
    /// # Errors
    /// A source does not compile: its diagnostics.
    pub fn compile(sources: &[(&str, &str)]) -> Result<Self, String> {
        let mut compiled: Vec<tilolang_tilo::tilolang::Compiled> = Vec::new();
        for (source, ids) in sources {
            let lock =
                tilolang_tilo::tilolang::IdsLock::parse(ids).map_err(|error| error.to_string())?;
            let imports: Vec<_> = compiled.iter().map(|done| done.meta.clone()).collect();
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
        Ok(Self(Arc::new(compiled)))
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

/// The sources of the packages built into this build, each app after those it uses.
const BUILT_IN: &[&[(&str, &str)]] = &[
    &[
        (
            include_str!("../../../../apps/tl/agent/app.tl"),
            include_str!("../../../../apps/tl/agent/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/chat/app.tl"),
            include_str!("../../../../apps/tl/chat/app.ids"),
        ),
    ],
    &[
        (
            include_str!("../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../apps/tl/discussion/app.ids"),
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
    static COMPILED: OnceLock<Result<Vec<Packages>, String>> = OnceLock::new();
    let compiled = COMPILED.get_or_init(|| {
        BUILT_IN
            .iter()
            .map(|sources| Packages::compile(sources))
            .collect()
    });
    Ok(compiled
        .as_ref()
        .map_err(Clone::clone)?
        .iter()
        .find(|packages| packages.app() == app)
        .cloned())
}

/// Why a program over the app `app`, which this build does not carry, does not run here.
#[must_use]
pub fn runs_elsewhere(app: &str) -> String {
    format!(
        "this program's app {app} runs only on a device that has it open: this server runs only \
         the apps built into it"
    )
}

/// An app's record of a workspace as bytes: its document (the app's primary, `""`) and the
/// chunks above it, by document id. A document the workspace does not hold yet is absent.
#[derive(Clone, Debug, Default)]
pub struct RecordBytes {
    pub documents: BTreeMap<String, Vec<u8>>,
}

/// The chunk the document `document` of a record is, or `None` for no document of the record.
#[must_use]
pub fn chunk_of_document(document: &str) -> Option<ChunkNo> {
    if document.is_empty() {
        return Some(FIRST);
    }
    let number = document.strip_prefix("derived:chunk.")?.strip_suffix('@')?;
    tilo_failure::ignored!(
        "a document named like a chunk without a number is no chunk of the record",
        number.parse().ok()
    )
}

/// The document chunk `chunk` of a record is kept in.
#[must_use]
pub fn document_of_chunk(chunk: ChunkNo) -> String {
    if chunk == FIRST {
        String::new()
    } else {
        chunk::document_id("", chunk)
    }
}

/// One document's change, as [`Opened::written`] answers it: the document, its changes since it
/// was read, and whether this write founds it (then the changes are the document whole).
#[derive(Clone, Debug)]
pub struct Written {
    pub document: String,
    pub changes: Vec<u8>,
    pub founding: bool,
}

/// An app's record opened on this thread: a host over it with its packages loaded.
pub struct Opened {
    host: Host,
    app: String,
    chunks: Chunks,
    /// The heads each document stood at when it was read; a chunk missing here was not held.
    read_at: BTreeMap<ChunkNo, Vec<ChangeHash>>,
    /// The Shortcut whose run what is invoked here is ([`Opened::in_run_of`]).
    run_of: Option<String>,
}

impl Opened {
    /// Open `record` through `packages` for `viewer`: the member whose device this is, as whom
    /// what its written text asks is asked. `policy` is the workspace's, as this device reads it:
    /// the host's members and who of them reads what (`workspace.members`, `std.may_read`,
    /// [`Self::readers`]). Without one, the host knows no members, and nobody reads anything.
    ///
    /// # Errors
    /// A document does not load, or a package does not.
    pub fn open(
        record: &RecordBytes,
        packages: &Packages,
        viewer: Actor,
        policy: Option<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>,
    ) -> Result<Self, String> {
        Self::open_with(record, packages, viewer, None, policy)
    }

    /// [`Self::open`] with the access its own authority decides invocations by given: the
    /// workspace's policy as the actor runs it, for a program with its own grants; `None` leaves
    /// its authority open, as code on this device that acts for the workspace does. `members` is
    /// the policy the host's members and their reads are read from.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn open_with(
        record: &RecordBytes,
        packages: &Packages,
        viewer: Actor,
        access: Option<std::sync::Arc<tilo_action_policy::MemberAccess>>,
        members: Option<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>,
    ) -> Result<Self, String> {
        let mut chunks = Chunks::default();
        let mut read_at = BTreeMap::new();
        for (document, bytes) in &record.documents {
            let Some(chunk) = chunk_of_document(document) else {
                continue;
            };
            let mut doc = tilo_resource::Doc::load(bytes).map_err(|error| error.to_string())?;
            read_at.insert(chunk, doc.get_heads());
            chunks.insert(chunk, doc);
        }
        chunks.reread_placement();
        let first = chunks.first().cloned().unwrap_or_default();
        // What the host's own authority decides invocations by: without an access it knows no
        // grants, and leaves every action to the caller.
        let acting = access.clone();
        let host = Host::new(&first, viewer, move || {
            acting.as_ref().map(|access| (**access).clone())
        })
        .map_err(|error| error.to_string())?;
        for package in packages.0.iter() {
            host.load_compiled(package)
                .map_err(|error| error.to_string())?;
        }
        host.sync(&chunks);
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
        let mut opened = Self {
            host,
            app: packages.app().to_owned(),
            chunks,
            read_at,
            run_of: None,
        };
        // What the host did of its own as it took the record in (a mention's ask) is the
        // viewer's, written as any change made here.
        opened.commit()?;
        Ok(opened)
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

    /// Mark what is invoked from here on as a run of the Shortcut `shortcut` (an event, a
    /// reminder) acting for its person: each change it makes carries the `shortcut` facet, so
    /// what reads the workspace's changes (a look-round, the Shortcut's own trigger) knows its
    /// own output by it rather than by who it acted as.
    pub fn in_run_of(&mut self, shortcut: impl Into<String>) {
        self.run_of = Some(shortcut.into());
    }

    /// Run `app`'s action `action` with `args` as `who`, made from `from` when given (what it
    /// creates is made from that resource: `created(r)?.from`). Answers what the action returned.
    ///
    /// # Errors
    /// The package has no such action, the action was refused or failed, or what it wrote does
    /// not apply.
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
        let outcome = match from {
            Some(from) => runtime.invoke_action_from(who, (id.as_str(), args), from),
            None => runtime.invoke_action(who, (id.as_str(), args)),
        }
        .map_err(|error| error.to_string())?;
        let value = match outcome {
            InvokeOutcome::Done(value) => value,
            InvokeOutcome::Denied => return Err(format!("{app}#{action} was refused")),
            InvokeOutcome::Suggested(_) => {
                return Err(format!("{app}#{action} waits for an answer"));
            }
        };
        self.commit()?;
        Ok(value)
    }

    /// The app's root resource, made the first time it is wanted, as a launcher opening the app
    /// makes it: what the app's own actions are published on.
    ///
    /// # Errors
    /// The package has no root, or making it does not apply.
    pub fn open_root(&mut self) -> Result<ResourceRef, String> {
        let root = self
            .host
            .root_of(&self.app)
            .map_err(|error| error.to_string())?;
        self.commit()?;
        Ok(root)
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
        let words = self.host.runtime().words();
        for package in self.host.packages() {
            if package.meta.app != self.app {
                continue;
            }
            let resource = publish_package(&package.meta, planner, &*words);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
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

    /// Run the published action `address` with `arguments` as `invoker`: found among the
    /// record's published resources, its arguments typed by its declared parameters, planned by
    /// its own code and applied where the record's manifest places its edits, recorded with what
    /// `context` says of the run it is a step of. A `read` answers the core read, as `invoker`
    /// reads.
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
        let encoded = tilo_actions::api::encode_json(&resources, address, arguments)?;
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
        self.apply_as(&action, Some((invoker, context)))
            .map_err(ApiError::Failed)?;
        self.host.sync(&self.chunks);
        self.commit().map_err(ApiError::Failed)?;
        let made = self
            .host
            .take_returned(&action.action_id)
            .map(|made| resource_id(&action.app, &made.id_string()));
        Ok(ActionOutput {
            value: None,
            resources: made.into_iter().collect(),
        })
    }

    /// Apply every action the host committed since the last call to the record, and take the
    /// record back into the host; again while taking it in commits more (a mention's ask).
    fn commit(&mut self) -> Result<(), String> {
        loop {
            let committed = self.host.take_committed();
            if committed.is_empty() {
                return Ok(());
            }
            for entry in committed {
                self.apply(&entry.action)?;
            }
            self.host.sync(&self.chunks);
        }
    }

    /// Apply `action` where its edits belong: each chunk it writes holds its part as one change,
    /// and every part names the one invocation the action ran in.
    fn apply(&mut self, action: &DynAction) -> Result<(), String> {
        self.apply_as(action, None)
    }

    /// [`Self::apply`], recorded as `step`'s invoker's when given, with the run, step and cause
    /// its context names.
    fn apply_as(
        &mut self,
        action: &DynAction,
        step: Option<(&ActionInvoker, &ActionContext)>,
    ) -> Result<(), String> {
        let actor = action.invoker();
        let invoker = step.map_or_else(
            || ActionInvoker::admitted(&actor.clone().into()),
            |(invoker, _)| invoker.clone(),
        );
        let invocation = action
            .authorship
            .invocation
            .as_deref()
            .map_or_else(InvocationId::new, tilolang_tilo::action::invocation_id);
        let mut first = false;
        for (chunk, part) in self.chunks.parts_of(action) {
            let encoded = part.encode().map_err(|error| error.to_string())?;
            let mut context = ActionContext::for_resource(
                part.resources().first().copied().unwrap_or("workspace"),
                &part.kind,
                &part.kind,
            )
            .with_app(&self.app, "tilolang")
            .with_source_invocation(Some(invocation))
            .in_gesture(action.authorship.gesture)
            .with_invoker(invoker.clone());
            if let Some((_, step)) = step {
                context.group.clone_from(&step.group);
                context.facets.clone_from(&step.facets);
                context.caused_by.clone_from(&step.caused_by);
                context.purpose.clone_from(&step.purpose);
            }
            let context = match &self.run_of {
                Some(shortcut) => {
                    context.with_facet(ActionFacet::new("shortcut", shortcut.clone()))
                }
                None => context,
            };
            let doc = self.chunks.open(chunk);
            self.host
                .with_schemas(|| {
                    tilo_actions::apply_local_with_context(
                        doc,
                        &part,
                        encoded,
                        Some(&actor.actor_bytes()),
                        context,
                    )
                })
                .map_err(|error| error.to_string())?;
            doc.commit();
            first |= chunk == FIRST;
        }
        if first {
            self.chunks.reread_placement();
        }
        Ok(())
    }

    /// What the actions run here wrote, one change per document: the changes since the document
    /// was read, or the document whole where this founds it.
    #[must_use]
    pub fn written(&self) -> Vec<Written> {
        let mut written = Vec::new();
        for (chunk, doc) in self.chunks.all() {
            let document = document_of_chunk(chunk);
            let mut doc = doc.clone();
            match self.read_at.get(&chunk) {
                Some(heads) if doc.get_heads() == *heads => {}
                Some(heads) => {
                    written.push(Written {
                        document,
                        changes: doc.save_after(heads),
                        founding: false,
                    });
                }
                None => {
                    written.push(Written {
                        document,
                        changes: doc.save(),
                        founding: true,
                    });
                }
            }
        }
        written
    }
}

/// The member `id` names, as a tilolang actor.
#[must_use]
pub fn member(id: tilo_actions::MemberId) -> Actor {
    Actor::Member(id)
}

/// What the documents `after` gained since the record stood as `before`, one change per
/// document: the whole document for one `before` lacked.
///
/// # Errors
/// A document does not load.
pub fn written_since(
    before: &RecordBytes,
    after: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<Written>, String> {
    let mut written = Vec::new();
    for (document, bytes) in after {
        let Some(was) = before.documents.get(document) else {
            written.push(Written {
                document: document.clone(),
                changes: bytes.clone(),
                founding: true,
            });
            continue;
        };
        if was == bytes {
            continue;
        }
        let heads = automerge::Automerge::load(was)
            .map_err(|error| error.to_string())?
            .get_heads();
        let now = automerge::Automerge::load(bytes).map_err(|error| error.to_string())?;
        if now.get_heads() != heads {
            written.push(Written {
                document: document.clone(),
                changes: now.save_after(&heads),
                founding: false,
            });
        }
    }
    Ok(written)
}

/// An app's record as a program run on this device holds it: its documents as the workspace has
/// them with what the program's steps changed since, opened through the app's packages for each
/// step, so every step reads what the steps before it wrote.
pub struct ProgramRecord {
    packages: Packages,
    record: RecordBytes,
    /// The access its own authority decides the run's invocations by: the bot's grants, as the
    /// workspace made them.
    access: Option<std::sync::Arc<tilo_action_policy::MemberAccess>>,
}

impl ProgramRecord {
    /// `record` as the workspace holds it now, read through `packages`, its invocations decided
    /// by `access`.
    #[must_use]
    pub const fn new(
        packages: Packages,
        record: RecordBytes,
        access: Option<std::sync::Arc<tilo_action_policy::MemberAccess>>,
    ) -> Self {
        Self {
            packages,
            record,
            access,
        }
    }

    /// The record opened as its app, so nothing a mention in it asks is made here for anyone.
    fn open(&self) -> Result<Opened, String> {
        let app = runtime::ids::AppId::from(self.packages.app());
        let members = self
            .access
            .as_ref()
            .map(|access| std::sync::Arc::clone(&access.policy));
        let mut opened = Opened::open_with(
            &self.record,
            &self.packages,
            Actor::App(app),
            self.access.clone(),
            members,
        )?;
        opened.open_root()?;
        Ok(opened)
    }

    /// Keep what `written` changed in the record's documents.
    fn keep(&mut self, written: Vec<Written>) -> Result<(), String> {
        for write in written {
            let bytes = match self.record.documents.get(&write.document) {
                Some(bytes) if !write.founding => {
                    let mut doc =
                        automerge::Automerge::load(bytes).map_err(|error| error.to_string())?;
                    doc.load_incremental(&write.changes)
                        .map_err(|error| error.to_string())?;
                    doc.save()
                }
                _ => write.changes,
            };
            self.record.documents.insert(write.document, bytes);
        }
        Ok(())
    }
}

/// The `record` a whole-app read asks for: every resource of the app with its collections
/// expanded, references kept as ids. What a program planning over a whole app reads, such as a
/// bridge that mirrors a chat.
pub const TREE: &str = "tree";

/// How deep a whole-app read expands collections: a conversation's channels, their messages and
/// threads, and a thread's messages. References are never followed, so a loop back to a parent
/// costs nothing and no bound is needed beyond this.
const TREE_DEPTH: usize = 5;

impl TilolangRecord for ProgramRecord {
    fn rows(&self) -> Result<Vec<CatalogueRow>, String> {
        Ok(self.open()?.rows())
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Json>,
        invoker: &ActionInvoker,
        context: &ActionContext,
    ) -> Result<ActionOutput, ApiError> {
        let mut opened = self.open().map_err(ApiError::Failed)?;
        let output = opened.invoke_published(address, arguments, invoker, context)?;
        self.keep(opened.written()).map_err(ApiError::Failed)?;
        Ok(output)
    }

    fn read(&self, resource: &str, reader: &Actor) -> Result<Json, String> {
        let opened = self.open()?;
        let host = opened.host();
        if resource == TREE {
            return tree_read(host, reader);
        }
        let args = if resource.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "item": resource })
        };
        host.answer_read(&args, reader)
    }

    fn documents(&self) -> BTreeMap<String, Vec<u8>> {
        self.record.documents.clone()
    }
}

/// The whole app as the core read answers it: its documents list, each root resource with every
/// collection expanded ([`TREE_DEPTH`] levels), and each reference kept as `{type, id, label}`.
fn tree_read(host: &Host, reader: &Actor) -> Result<Json, String> {
    let mut documents = host.answer_read(&serde_json::json!({}), reader)?;
    if let Some(records) = documents.get_mut("records").and_then(Json::as_array_mut) {
        for record in records.iter_mut() {
            let Some(item) = record.get("item").and_then(Json::as_str).map(str::to_owned) else {
                continue;
            };
            let whole = tree_item(host, &item, reader, TREE_DEPTH)?;
            if let (Json::Object(dst), Json::Object(src)) = (&mut *record, whole)
                && let Some(fields) = src.get("fields")
            {
                dst.insert("fields".into(), fields.clone());
            }
        }
    }
    Ok(documents)
}

/// `item`, one resource of the app, with each collection field's items deepened by `depth` more
/// levels: each collection is read whole (`items` and `next`, page by page), never only its first
/// page, since a chat's newest message is its last.
fn tree_item(host: &Host, item: &str, reader: &Actor, depth: usize) -> Result<Json, String> {
    let mut answer = host.answer_read(&serde_json::json!({ "item": item }), reader)?;
    if depth == 0 {
        return Ok(answer);
    }
    // A collection reads as a page (`{items}`); a list, set or map projects with its `count`.
    let collections: Vec<String> = answer
        .get("fields")
        .and_then(Json::as_object)
        .map(|fields| {
            fields
                .iter()
                .filter(|(_, value)| {
                    value.get("items").and_then(Json::as_array).is_some()
                        && value.get("count").is_none()
                })
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default();
    for name in collections {
        let mut rows: Vec<Json> = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut ask = serde_json::json!({ "item": item, "field": name });
            if let Some(after) = &after {
                ask["after"] = Json::String(after.clone());
            }
            let answer = host.answer_read(&ask, reader)?;
            // A read of one collection of a resource answers the resource, that collection's
            // page under its fields.
            let page = answer
                .get("fields")
                .and_then(|fields| fields.get(&name))
                .cloned()
                .unwrap_or(Json::Null);
            if let Some(items) = page.get("items").and_then(Json::as_array) {
                rows.extend(items.iter().cloned());
            }
            after = page.get("next").and_then(Json::as_str).map(str::to_owned);
            if after.is_none() {
                break;
            }
        }
        for row in &mut rows {
            let Some(child) = row.get("item").and_then(Json::as_str).map(str::to_owned) else {
                continue;
            };
            let whole = tree_item(host, &child, reader, depth - 1)?;
            if let (Json::Object(dst), Json::Object(src)) = (&mut *row, whole)
                && let Some(fields) = src.get("fields")
            {
                dst.insert("fields".into(), fields.clone());
            }
        }
        if let Some(fields) = answer.get_mut("fields").and_then(Json::as_object_mut) {
            fields.insert(name, serde_json::json!({ "items": rows }));
        }
    }
    Ok(answer)
}

impl crate::workspaces::Workspaces {
    /// `app`'s record of `workspace`, as this device holds it: its document and every chunk
    /// above it this device reads.
    ///
    /// # Errors
    /// The workspace is not held here, or a document of it cannot be read.
    pub async fn tl_record(&self, workspace: &str, app: &str) -> Result<RecordBytes, String> {
        let routes = self
            .document_routes(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut record = RecordBytes::default();
        for (of, document) in routes {
            if of != app || chunk_of_document(&document).is_none() {
                continue;
            }
            let bytes = self
                .resource_document(workspace, app, &document)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(bytes) = bytes {
                record.documents.insert(document, bytes);
            }
        }
        Ok(record)
    }

    /// What `route`'s record of `workspace` publishes to search, read as `person` reads it: the
    /// entries its app tells search about, one per resource, or `None` for an app this build
    /// runs no package of (a compiled-in app, whose entries an app worker reads instead).
    ///
    /// The record is opened through its app's built-in package as that person, so what a direct
    /// conversation keeps is found only by the people it is between. Nothing is written: what a
    /// search reads leaves the record as it was.
    pub fn record_entries(
        &self,
        workspace: &str,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        person: &str,
    ) -> Option<Arc<[ui::search::index_publication::IndexEntry]>> {
        let Some(packages) = built_in(&route.app_id).ok().flatten() else {
            return None;
        };
        let record = self.tl_record_now(workspace, &route.app_id)?;
        let viewer = Actor::Member(tilo_actions::MemberId::person(person.to_owned()));
        let policy = self.policy_now(workspace).map(Arc::new);
        let opened = Opened::open(&record, &packages, viewer, policy).ok()?;
        Some(tilolang_tilo::search::entries(opened.host()).into())
    }

    /// [`Self::tl_record`] read here and now, on this thread, with nothing written: what search
    /// reads of a record.
    fn tl_record_now(&self, workspace: &str, app: &str) -> Option<RecordBytes> {
        /// The most bytes of one document a search reads out to index it.
        const MOST_BYTES: usize = 64 * 1024 * 1024;
        let mut record = RecordBytes::default();
        for (of, document) in self.own_listing(workspace).ok()? {
            if of != app || chunk_of_document(&document).is_none() {
                continue;
            }
            let workspaces = self.clone();
            let (of_workspace, of_app, of_document) =
                (workspace.to_owned(), app.to_owned(), document.clone());
            let read = crate::device_documents::on_its_own(async move {
                workspaces
                    .own_read(&of_workspace, (&of_app, &of_document), &[], MOST_BYTES)
                    .await
            });
            if let Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }) =
                read
            {
                record.documents.insert(document, bytes);
            }
        }
        Some(record)
    }

    /// Write `written`, changes of `app`'s record of `workspace`, as this device's own: each
    /// document's change signed, sealed under its group where the workspace seals, and
    /// committed.
    ///
    /// # Errors
    /// The workspace refused a write, or its keys could not be had: what was written before it
    /// stands.
    pub async fn write_tl(
        &self,
        workspace: &str,
        app: &str,
        written: Vec<Written>,
    ) -> Result<(), String> {
        for write in written {
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: app.to_owned(),
                cluster_id: workspace.to_owned(),
                document: write.document.clone(),
            };
            let sealed = match self.own_route_keys(&route, write.founding).await {
                Some(Ok(keys)) => Some(keys),
                Some(Err(reason)) => return Err(reason),
                None => None,
            };
            self.own_write(
                workspace,
                (app, &write.document),
                None,
                &write.changes,
                write.founding,
                write.founding,
                sealed.as_ref(),
            )
            .await
            .map_err(|refusal| refusal.to_string())?;
        }
        Ok(())
    }

    /// Open `packages`' app's record of `workspace` for `viewer`, run `work` on it, and write what
    /// it changed as this device's own ([`Self::write_tl`]): what `work` answered. The record is
    /// read anew for each call, so `work` acts on what the workspace holds now.
    ///
    /// # Errors
    /// The record could not be read or opened, `work` failed (nothing it changed is written), or
    /// the write was refused.
    pub async fn with_tl_record<R: Send + 'static>(
        &self,
        workspace: &str,
        packages: &Packages,
        viewer: Actor,
        work: impl FnOnce(&mut Opened) -> Result<R, String> + Send + 'static,
    ) -> Result<R, String> {
        let app = packages.app().to_owned();
        let record = self.tl_record(workspace, &app).await?;
        let packages = packages.clone();
        let policy = self.policy_of(workspace).await.map(std::sync::Arc::new);
        let (answer, written) = tokio::task::spawn_blocking(move || {
            let mut opened = Opened::open(&record, &packages, viewer, policy)?;
            let answer = work(&mut opened)?;
            Ok::<_, String>((answer, opened.written()))
        })
        .await
        .map_err(|error| error.to_string())??;
        self.write_tl(workspace, &app, written).await?;
        Ok(answer)
    }
}
