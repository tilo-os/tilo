//! Apps as an agent uses them: each record held in the app's own view,
//! attached and never drawn, acted on through that view.
//!
//! [`Replica`] mounts the app's real component over one record, as a window
//! does, on a host without a screen: the record's view is live, and every
//! operation goes through the same API a person's click takes, checked
//! against the member's permissions and attributed to it. Nothing is
//! performed out of sight of a view; what the member did is where its
//! presence shows ([`Replica::presence`]). The person's devices draw the
//! same record from synced state when someone looks.
//!
//! Durable state moves as Automerge changes: [`Replica::take_mutation`]
//! returns this member's own changes since the last call (never ones it
//! received), and [`Replica::apply`] merges another member's.
//!
//! An agent also runs the record's effects and views that answer with what
//! they found or made (a look at an artwork, a PDF's text, an export), in
//! the same view and through the same API ([`offered_to_agents`]): never
//! one reaching a person's own device or needing them attending, and never
//! told where on a device to write, so what an effect makes is kept as
//! content in the store the view is lent ([`Replica::open_lending`]).

use collab_types::DocumentMutation;
use tilo_actions::ActionOutput;
use tilo_actions::api::{ActionAddress, ActionDescriptor, ApiError};
use tilo_server_protocol::live::LiveAnchor;
use tilo_server_protocol::live::presence_anchors;
use ui::application::ApplicationDefinition;
use ui::collaboration::PresencePayload;
use ui::framework::{AttachedView, BesideChange, ResourceSelection};
use ui::permissions::{PermissionContext, Role, policy};
use ui::portable_storage::PortableStorage;
use ui::resource::{DocumentCheckout, DocumentId, ResourceOwners};
use ui::tilo_resource::{MemberId, Place, ResourceKind};

/// An app whose records this host keeps and whose windows another device
/// holds: a hosted workspace's Browser, whose window runs in the workspace's
/// own VM. Its workspaces list it and open a record for it like any app, and
/// the device holding the window runs the app; this host mounts none of it,
/// and reads its records only as bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedApp {
    pub app_id: &'static str,
    pub title: &'static str,
    /// What its open must name, when it opens nothing without it: a
    /// workspace's Linux apps open one app, named by its desktop file. An
    /// open that leaves it out is refused before anything starts, so whoever
    /// opens it is told to name it.
    pub needs: Option<PlacedArgument>,
}

/// The one argument a [`PlacedApp`]'s open needs: its name and what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedArgument {
    pub name: &'static str,
    pub description: &'static str,
}

/// The apps an agent can open, by app id.
#[derive(Clone)]
pub struct AppCatalogue {
    apps: Vec<&'static ApplicationDefinition>,
    placed: Vec<PlacedApp>,
}

/// Every app the browser desktop registers (from the app manifests): what a
/// browser session can mount, and so what an agent in it can open.
#[must_use]
pub fn component_apps() -> Vec<&'static ApplicationDefinition> {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(unused_mut, reason = "only a linux host adds apps")
    )]
    let mut apps = crate::web_apps::web_apps();
    // Connectors run on a workspace's host, never in a browser; the bots there
    // work on their records.
    #[cfg(target_os = "linux")]
    apps.push(discord::definition());
    apps
}

impl AppCatalogue {
    #[must_use]
    pub fn new(apps: Vec<&'static ApplicationDefinition>) -> Self {
        Self {
            apps,
            placed: Vec::new(),
        }
    }

    /// [`component_apps`].
    #[must_use]
    pub fn native() -> Self {
        Self::new(component_apps())
    }

    /// This catalogue, listing `app` too: an app whose windows another device
    /// holds ([`PlacedApp`]).
    #[must_use]
    pub fn placing(mut self, app: PlacedApp) -> Self {
        if self.placed(app.app_id).is_none() {
            self.placed.push(app);
        }
        self
    }

    #[must_use]
    pub fn get(&self, app_id: &str) -> Option<&'static ApplicationDefinition> {
        self.apps
            .iter()
            .copied()
            .find(|definition| definition.app_id() == app_id)
    }

    /// The [`PlacedApp`] `app_id` names, when another device holds its
    /// windows: this host lists it and opens its records, and mounts nothing
    /// of it.
    #[must_use]
    pub fn placed(&self, app_id: &str) -> Option<PlacedApp> {
        self.placed
            .iter()
            .copied()
            .find(|placed| placed.app_id == app_id)
    }

    /// Whether `app_id` names an app of this catalogue: one this host mounts,
    /// or one another device holds the windows of.
    #[must_use]
    pub fn has(&self, app_id: &str) -> bool {
        self.get(app_id).is_some() || self.placed(app_id).is_some()
    }

    /// Every app, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &'static ApplicationDefinition> + '_ {
        self.apps.iter().copied()
    }

    /// Every app another device holds the windows of, in registration order.
    pub fn iter_placed(&self) -> impl Iterator<Item = PlacedApp> + '_ {
        self.placed.iter().copied()
    }

    /// Read existing resource view descriptors without mounting or initializing
    /// an application. The document remains owned by its original workspace.
    #[must_use]
    pub fn resource_views(
        &self,
        app_id: &str,
        doc: &automerge::AutoCommit,
        wanted: &tilo_actions::remote::ViewResourceQuery,
        permissions: &PermissionContext,
    ) -> Vec<ActionDescriptor> {
        // A part of a chain the app keeps beside its record (a chat's
        // messages) is of the part's own resource, as its identity names it.
        let part = ui::resource::identity(doc)
            .filter(|checkout| checkout.id.chain_part().is_some())
            .and_then(|checkout| {
                ui::framework::hosted_kinds()
                    .get(checkout.kind.as_str())
                    .copied()
            })
            .filter(|kind| kind.app_id() == Some(app_id));
        let Some(kind) = part.or_else(|| self.kind(app_id)) else {
            return Vec::new();
        };
        discover_views(kind, app_id, doc, wanted, permissions)
    }

    /// The app whose records are of `kind`: where a trigger that names a
    /// record kind finds the records it watches.
    #[must_use]
    pub fn app_for_kind(&self, kind: &str) -> Option<&'static str> {
        self.iter()
            .find(|definition| definition.resource_type.as_str() == kind)
            .map(ApplicationDefinition::app_id)
    }

    /// What `doc`, a record of `app_id`, is called when its app names it
    /// (a document's first heading): `None` for a name that is only the
    /// resource's label. Run in a confined worker for a record not opened
    /// (`crate::worker::ConfinedTitles`), as it reads untrusted bytes.
    #[must_use]
    pub fn resource_title(&self, app_id: &str, doc: &automerge::AutoCommit) -> Option<String> {
        let kind = self.kind(app_id)?;
        let state = kind.read(doc).ok()?;
        let name = kind.name(state.as_ref());
        let name = name.trim();
        (!name.is_empty() && name != kind.label()).then(|| name.to_owned())
    }

    /// The resource type `app_id`'s records hold.
    fn kind(&self, app_id: &str) -> Option<ResourceKind> {
        let definition = self.get(app_id)?;
        ui::framework::hosted_kinds()
            .get(definition.resource_type.as_str())
            .copied()
    }
}

/// Which record a [`Replica`] holds.
pub enum Seed {
    New,
    /// A new record under the id a workspace route names (the workspace's default
    /// record a host seeds before any member opens it): its genesis names
    /// that id, as the record every member's first window opens.
    NewAt(DocumentId),
    /// An existing record, from the durable changes the host replayed.
    Existing {
        record: DocumentId,
        /// Its Automerge changes (a snapshot, then increments), in order.
        changes: Vec<Vec<u8>>,
    },
}

/// Whom a replica acts as, which decides what its API allows.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReplicaActor {
    /// The host's own device acting for its user (workspace actions): the
    /// record's owner, as a browser tab of the owner is.
    Owner,
    /// A delegated agent, at the cap its delegation bounds it to, for
    /// `person` under `name`. The replica's member is the agent, so what it
    /// writes is the agent's; the person is whose access bounds it.
    Agent {
        /// The person the agent acts for.
        person: MemberId,
        /// What its person calls it, as its delegation says.
        name: String,
    },
    /// An installed bot: its installer's access bounded by the bot's
    /// grants, built by the host from the workspace's policy before each run.
    Bot {
        /// The installation the bot runs as.
        installation: String,
        /// The person who installed it, whom its invocations are for.
        installer: String,
        /// The installer's access bounded by the bot's grants.
        access: policy::MemberAccess,
    },
    /// A person whose action the host runs for them, or an agent acting for
    /// them: their own current access and the workspace's sandbox, as the
    /// workspace's policy gave them to the host, bounded as the host reads
    /// the caller ([`tilo_actions::remote::Caller::invoker`]: an agent's by
    /// the editor preset at most). An agent is offered only what reaches
    /// agents, and is who the changes it makes are by, for the person.
    Member {
        /// The person's current member access in the workspace, bounded by
        /// whoever acts for them.
        access: policy::MemberAccess,
        /// The workspace's sandbox capabilities.
        sandbox: Vec<collab_types::caps::CapOverride>,
        /// When the policy was read, for grant expiry.
        unix_time: u64,
        /// The name the agent acting for the person goes by, as its admission certified it,
        /// when an agent acts (`access`'s acting names it, in its session).
        agent_name: Option<String>,
        /// The device the person's work is admitted on, when the caller names this device:
        /// a server taking a member's work under their delegation. History names it as where
        /// the change was made ([`tilo_actions::ActionInvoker::device`]).
        device: Option<tilo_actions::InvokerDevice>,
    },
}

/// Why a record could not be held or merged.
#[derive(Debug)]
pub enum AppError {
    /// No such app in the catalogue, or no resource type for it.
    UnknownApp(String),
    /// The new record could not be created.
    Launch(String),
    /// The record's durable changes could not be merged.
    Merge(String),
    /// The app worker that holds the record did not start, could not
    /// confine itself, or stopped ([`crate::worker`]).
    Confinement(String),
    /// The app was still working on `request` (its kind, as a log line names
    /// it) past the bound a caller waits: the document was opened again from
    /// what the workspace holds, and nothing that request or the caller sent
    /// after it was applied. A caller acting for a person shows the document
    /// again and sends what the person did since as new acts.
    Reopened { request: String },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownApp(app) => write!(f, "no app {app} can be opened here"),
            Self::Launch(reason) => write!(f, "the record could not be created: {reason}"),
            Self::Merge(reason) => write!(f, "the record did not merge: {reason}"),
            Self::Confinement(reason) => write!(f, "the app worker failed: {reason}"),
            Self::Reopened { request } => write!(
                f,
                "the app was still working on {request}; the document was opened again \
                 and nothing sent after it was applied"
            ),
        }
    }
}

impl std::error::Error for AppError {}

/// One record of one app, held in the app's own view
/// ([`AttachedView`]): mounted on this host, never drawn.
pub struct Replica {
    app_id: &'static str,
    kind: ResourceKind,
    view: AttachedView,
    /// Where its last operation left it.
    place: Option<Place>,
    /// It also runs the record's effects that answer ([`offered_to_agents`]),
    /// waiting for what they end as: whoever the host runs a record for, a
    /// person or an agent acting for one, has no window of it there (a
    /// script, an agent's session, a key), and is offered what a window
    /// offers them, as the desktop's broker does. The answer is the same as
    /// a window's. A bot's steps read through [`Self::offers_reads`] instead.
    runs_effects: bool,
    /// An agent's or a bot's: offered only what reaches agents. An agent's
    /// also runs the effects its host renders.
    automated: bool,
    /// It also runs the record's views that answer: for whoever has no
    /// window of the record there, an agent or a member reaching the host
    /// with no page of theirs open. A view moves a window, which the host's
    /// own person has their own of.
    runs_views: bool,
    /// What a plain listing of the app's resource has walked to, so its next
    /// page starts where the last ended ([`Self::survey_page_of`]).
    walked: std::cell::RefCell<Option<SurveyProgress>>,
    /// The member access its permissions were made from, and when: what [`Self::renew`]
    /// compares a later read of it with.
    held_access: Option<HeldAccess>,
}

/// A member's access as a replica's permissions were made from it, at `unix_time`:
/// who acts, and what the permission model decides their actions from.
#[derive(Clone)]
struct HeldAccess {
    acting: policy::Acting,
    effective: policy::Effective,
    sandbox: Vec<collab_types::caps::CapOverride>,
    unix_time: u64,
}

impl HeldAccess {
    fn of(actor: &ReplicaActor) -> Option<Self> {
        match actor {
            ReplicaActor::Member {
                access,
                sandbox,
                unix_time,
                ..
            } => Some(Self {
                acting: access.acting.clone(),
                effective: access.effective(),
                sandbox: sandbox.clone(),
                unix_time: *unix_time,
            }),
            _ => None,
        }
    }

    /// Whether the access read again at `later` decides as it did: the same acting, with
    /// nothing it decides by changed (another member admitted or given a role changes
    /// none of it), and no grant of it lapsed in between, which is the one thing time
    /// changes.
    fn decides_alike(&self, later: &Self) -> bool {
        self.acting == later.acting
            && self.effective == later.effective
            && self.sandbox == later.sandbox
            && !self.effective.lapses_between(
                self.unix_time.saturating_mul(1000),
                later.unix_time.saturating_mul(1000),
            )
    }
}

/// The tree a view publishes, shared with what reads it.
type PublishedTree = std::rc::Rc<Vec<tilo_actions::ResourceSpec>>;

/// What a listing walks ([`Replica::survey_page_of`]): the tree the view
/// publishes, kept as the view keeps it when a listing keeps its place in it,
/// or the level of a chain as its parts publish it.
#[derive(Clone, Copy)]
enum Listed<'a> {
    Tree(&'a [tilo_actions::ResourceSpec], Option<&'a PublishedTree>),
    Chain(&'a ui::framework::ChainLevel),
}

/// The level a listing walks: resources of the published tree, or a chain's
/// items, each built when the walk reaches it.
#[derive(Clone, Copy)]
enum Level<'a> {
    Tree(&'a [tilo_actions::ResourceSpec]),
    Chain(&'a ui::framework::ChainLevel),
}

impl Level<'_> {
    /// How many resources the level holds.
    fn len(self) -> usize {
        match self {
            Self::Tree(level) => level.len(),
            Self::Chain(level) => level.len(),
        }
    }
}

/// What a listing that keeps its place counted ([`SurveyProgress`]): the
/// tree or chain level as it was published, so a later page counts on only
/// from the same one.
enum Walked {
    Tree(PublishedTree),
    Chain(ui::framework::ChainLevel),
}

impl Walked {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Tree(one), Self::Tree(other)) => std::rc::Rc::ptr_eq(one, other),
            (Self::Chain(one), Self::Chain(other)) => one.same(other),
            _ => false,
        }
    }
}

/// The parameter an effect is told where on a device to write by. An agent
/// is never offered it, so what the effect makes is kept as content.
pub const PATH_PARAM: &str = "path";

/// The longest an agent's effect runs before it is given up on: well inside
/// the app worker supervisor's limit on one request, so the worker answers
/// first.
pub const EFFECT_TIMEOUT: ui::time::Duration = ui::time::Duration::from_secs(45);

/// Whether `descriptor` carries the contract tag `tag`.
fn tagged(descriptor: &ActionDescriptor, tag: &str) -> bool {
    descriptor.contract.tags.iter().any(|held| held == tag)
}

/// Whether an operation of `contract` answers with something: a value, or
/// a resource (content it made). A job that answers only that it ran does
/// not.
const fn answers(contract: &tilo_actions::ActionContract) -> bool {
    matches!(
        contract.result,
        tilo_actions::ResultSpec::Json { .. } | tilo_actions::ResultSpec::Resource { .. }
    )
}

/// Whether an agent is offered `descriptor`, an effect or view of its
/// record, to run in the record's view: one listed for agents, that
/// neither acts on a person's own device nor needs them attending, nor is
/// rendered by a host's render service ([`tilo_actions::HOST_RENDER_TAG`]),
/// and that answers with what it found or made (a PDF's text, a look, an
/// export kept as content). One answering nothing leaves an agent nothing to
/// go on: a view that only moves a window nobody sees, a read that opens an
/// editor for a person (a vault's "New item"), a file saved or a picker
/// opened. Document operations are offered by the API alone.
#[must_use]
pub fn offered_to_agents(descriptor: &ActionDescriptor) -> bool {
    descriptor.kind != tilo_actions::OperationKind::Document
        && descriptor.contract.offered_to_agents()
        && !tagged(descriptor, tilo_actions::OWN_DEVICE_SCOPE)
        && !tagged(descriptor, tilo_actions::ATTENDED_TAG)
        && !tagged(descriptor, tilo_actions::HOST_RENDER_TAG)
        && answers(&descriptor.contract)
}

/// Whether `descriptor` is an effect an agent is offered when its host
/// renders ([`tilo_actions::HOST_RENDER_TAG`]): one [`offered_to_agents`]
/// would offer but for the tag. A worker lists it in an agent's survey; the
/// host keeps it only when it runs a render service
/// ([`crate::render::offered`]) and answers it there, never in the worker.
#[must_use]
pub fn offered_when_host_renders(descriptor: &ActionDescriptor) -> bool {
    descriptor.kind != tilo_actions::OperationKind::Document
        && tagged(descriptor, tilo_actions::HOST_RENDER_TAG)
        && descriptor.contract.offered_to_agents()
        && !tagged(descriptor, tilo_actions::OWN_DEVICE_SCOPE)
        && !tagged(descriptor, tilo_actions::ATTENDED_TAG)
        && answers(&descriptor.contract)
}

/// Whether an agent may run `descriptor` while it plans a task, to learn
/// what an edit needs before it makes it (an artwork's size, a PDF's text,
/// a look at a page): a view or an effect declared `kind = "read"`, which
/// changes nothing, asks no one first and answers with what it found rather
/// than with content of its own making.
#[must_use]
pub fn readable(descriptor: &ActionDescriptor) -> bool {
    let contract = &descriptor.contract;
    let reads = matches!(
        descriptor.kind,
        tilo_actions::OperationKind::View | tilo_actions::OperationKind::Effect
    ) && contract.kind == policy::ActionKind::Read;
    let found = match &contract.result {
        tilo_actions::ResultSpec::Json { .. } => true,
        tilo_actions::ResultSpec::Resource { resource_kind } => {
            resource_kind != tilo_actions::CONTENT_RESOURCE_KIND
        }
        _ => false,
    };
    reads && found && contract.confirmation == tilo_actions::ConfirmationPolicy::None
}

/// Why an agent may not run `descriptor`, an effect or view of its record
/// that [`offered_to_agents`] leaves out.
fn not_offered(descriptor: &ActionDescriptor) -> ApiError {
    let label = &descriptor.action_label;
    let (code, reason) = if tagged(descriptor, tilo_actions::OWN_DEVICE_SCOPE) {
        (
            "own_device",
            format!("{label} acts on a person's own device; an agent does not run it"),
        )
    } else if tagged(descriptor, tilo_actions::ATTENDED_TAG) {
        (
            "attended",
            format!("{label} needs its person attending; an agent does not run it"),
        )
    } else if tagged(descriptor, tilo_actions::HOST_RENDER_TAG) {
        (
            "host_render",
            format!("{label} is rendered by a host's render service, which this host does not run"),
        )
    } else {
        (
            "effect_not_offered",
            format!(
                "{label} answers with nothing an agent could use; an agent runs the effects \
                 and views that answer with what they found or made"
            ),
        )
    };
    ApiError::Unavailable {
        code: code.into(),
        reason,
    }
}

/// Whether any member of the workspace may ask the host to run `descriptor` for
/// themselves ([`tilo_actions::MEMBER_OWN_TAG`]).
#[must_use]
pub fn member_own(descriptor: &ActionDescriptor) -> bool {
    descriptor
        .contract
        .tags
        .iter()
        .any(|tag| tag == tilo_actions::MEMBER_OWN_TAG)
}

impl Replica {
    /// Hold a record of `app_id` in `workspace` as `member` (its author id),
    /// acting as `actor` ([`ReplicaActor`]).
    pub fn open(
        catalogue: &AppCatalogue,
        app_id: &str,
        workspace: &str,
        member: &MemberId,
        actor: ReplicaActor,
        seed: Seed,
    ) -> Result<Self, AppError> {
        Self::open_lending(
            catalogue, app_id, workspace, member, actor, seed, false, None,
        )
    }

    /// [`Self::open`], lending the record's view `content`, as a host lends
    /// a window its store: the bytes a content hash names (a picture
    /// another app made) are read from it, and what the app keeps as
    /// content (an export, a rendered look) is stored there.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_lending(
        catalogue: &AppCatalogue,
        app_id: &str,
        workspace: &str,
        member: &MemberId,
        actor: ReplicaActor,
        seed: Seed,
        asks_workspace: bool,
        content: Option<tilo_content::ContentStore>,
    ) -> Result<Self, AppError> {
        let definition = catalogue
            .get(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let kind = catalogue
            .kind(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let for_agent =
            matches!(&actor, ReplicaActor::Member { access, .. } if access.acting.is_agent());
        let runs_effects = !matches!(actor, ReplicaActor::Bot { .. });
        let automated =
            for_agent || matches!(actor, ReplicaActor::Agent { .. } | ReplicaActor::Bot { .. });
        let runs_views = matches!(
            actor,
            ReplicaActor::Agent { .. } | ReplicaActor::Member { .. }
        );
        // What an agent's replica seeds belongs to the person it acts for: the
        // change is the agent's, the document the person's.
        let acted_for = match &actor {
            ReplicaActor::Agent { person, .. } => Some(person.clone()),
            _ => None,
        };
        let held_access = HeldAccess::of(&actor);
        let (invoker, permissions) = replica_access(actor, workspace, member);
        let creator = acted_for.unwrap_or_else(|| {
            invoker
                .acting
                .by
                .member_id()
                .cloned()
                .unwrap_or_else(|| member.clone())
        });
        let (checkout, bytes, created) = seeded(definition, kind, workspace, &creator, seed)?;
        let opening = AttachedView::opening(definition, workspace, checkout, bytes)
            .for_invoker(&invoker)
            .with_permissions(permissions)
            .created(created);
        let opening = if asks_workspace {
            opening.asking_the_workspace()
        } else {
            opening
        };
        let view = match content {
            Some(store) => opening.lending(store),
            None => opening,
        }
        .open()
        .map_err(AppError::Launch)?;
        Ok(Self {
            app_id: definition.app_id(),
            kind,
            view,
            place: None,
            runs_effects,
            automated,
            runs_views,
            walked: std::cell::RefCell::new(None),
            held_access,
        })
    }

    /// Whether an agent or a bot holds it, so it is offered only what
    /// reaches agents. The host acting for a person (the owner's workspace
    /// actions, a member's comment) is offered what people are,
    /// `audience = "people"` included.
    const fn automated(&self) -> bool {
        self.automated
    }

    /// Whether it offers the record's read effects (a workspace digest, a
    /// route plan): a bot's, held by something automated that runs no
    /// effects. A bot's steps read through them, where a person's reads go
    /// through the record's mounted component or the effects it runs.
    const fn offers_reads(&self) -> bool {
        self.automated && !self.runs_effects
    }

    #[must_use]
    pub const fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// Collaboration route of this record ([`DocumentId::route`]): the record
    /// id a `DocumentRoute` names it by.
    #[must_use]
    pub fn wire_document(&self) -> &str {
        self.view.checkout().id.route()
    }

    #[must_use]
    pub const fn document_id(&self) -> &DocumentId {
        &self.view.checkout().id
    }

    /// The document's view as a page shows it ([`AttachedView::composition`]),
    /// and the theme it is laid out in.
    #[must_use]
    pub fn page(&self) -> (ui::composition::Composition, ui::Theme) {
        (self.view.composition(), self.view.theme())
    }

    /// Show the page at an item named by another app's resource reference.
    ///
    /// # Errors
    /// The app has no registered view of the item.
    pub fn show_page_item(&mut self, item: &str) -> Result<(), String> {
        self.view.show_page_item(item)
    }

    /// What its member did on the view's page ([`AttachedView::act_on_page`]).
    ///
    /// # Errors
    /// The view has no such widget, or refuses the action.
    pub fn act_on_page(
        &mut self,
        widget: &str,
        target: Option<&str>,
        action: &ui::a11y::Action,
    ) -> Result<(), String> {
        self.view.act_on_page(widget, target, action)
    }

    /// Where the member's last operation left it, as the live session's
    /// selection anchors every member's window resolves against the
    /// record's own items: the paragraph it wrote and the caret at its end,
    /// the item it acted on. `None` before any operation, or for one that
    /// lands nowhere in particular.
    #[must_use]
    pub fn presence(&self) -> Option<(LiveAnchor, LiveAnchor)> {
        let place = self.place.clone()?;
        let (key, payload) =
            ResourceSelection::from(place).presence_of(self.kind.kind(), self.app_id);
        let (start, end) = match payload {
            PresencePayload::Caret { pos } => (pos, pos),
            PresencePayload::Selection { start, end } => (start, end),
            _ => return None,
        };
        presence_anchors(&key, start, end)
    }

    /// Every document operation the member may perform through the
    /// view now, as inert descriptors — those on the parts of a chain the
    /// app keeps beside the record included — and the effects any member may ask
    /// the host to run for themselves ([`tilo_actions::MEMBER_OWN_TAG`]);
    /// for a bot, the record's read effects too; for an agent and the host
    /// acting for its person, the effects and views [`offered_to_agents`],
    /// without a [`PATH_PARAM`] to write to. An agent or a bot is offered only what reaches agents.
    #[must_use]
    pub fn survey(&self) -> Vec<ActionDescriptor> {
        let resources = self.view.published();
        let in_chains = chain_resources(&resources);
        tilo_actions::api::survey(&resources)
            .into_iter()
            .filter_map(|descriptor| self.offered(descriptor, &in_chains))
            .collect()
    }

    /// [`Self::survey`] as the rows a catalogue lists it in, kept to answer
    /// its pages ([`Self::page_of`]): each operation in the row of the
    /// resource holding it, so a caller naming one resource of the record
    /// reads the operations on its children and not the record's own. A
    /// resource none of whose own operations are offered, but whose children
    /// offer some, stands in a row of its own, so a caller reading a level at
    /// a time reaches them (a chat's messages below its record).
    #[must_use]
    pub fn survey_rows(&self) -> Vec<tilo_actions::remote::CatalogueRow> {
        let (resources, past_root) = self.survey_resources();
        let in_chains = chain_resources(&resources);
        let mut rows: Vec<_> = tilo_actions::api::survey_placed(&resources)
            .into_iter()
            .filter_map(|placed| {
                let tilo_actions::api::PlacedAction {
                    parent,
                    has_children,
                    action,
                } = placed;
                let action = self.offered(action, &in_chains)?;
                Some(tilo_actions::remote::CatalogueRow::for_placed_action(
                    "",
                    self.app_id(),
                    tilo_actions::api::PlacedAction {
                        parent,
                        has_children,
                        action,
                    },
                ))
            })
            .collect();
        let offering: std::collections::HashSet<String> =
            rows.iter().map(|row| row.resource.id.clone()).collect();
        self.survey_containers(&resources, past_root.as_ref(), &offering, &mut rows);
        rows
    }

    /// The resources [`Self::survey_rows`] lists, and the record's own
    /// resource under which the items past each collection's published page
    /// stand (the one the view leaves out past the page, as children of a
    /// holder pushed last), when there are any.
    fn survey_resources(&self) -> (Vec<tilo_actions::ResourceSpec>, Option<(usize, String)>) {
        let mut resources = self.view.resources();
        // The items each chain keeps past the page each of its parts
        // publishes, under the chain: a listing reaches every one of them.
        for resource in &mut resources {
            let id = resource.id.as_str().to_owned();
            if ui::resource::chain_of_resource_id(&id).is_some() {
                resource
                    .children
                    .extend(self.view.past_page_range_under(&id, 0, usize::MAX));
            }
        }
        // The items past each collection's published page, under the
        // record's resource: a listing reaches every one of them.
        let past = self.view.resources_past_page();
        let mut past_root = None;
        if !past.is_empty()
            && let Some(root) = resources
                .iter()
                .find(|resource| resource.id.as_str() == self.kind.kind())
        {
            let mut holder = tilo_actions::ResourceSpec::new(
                root.id.as_str(),
                root.kind.clone(),
                root.label.clone(),
            );
            holder.app_id.clone_from(&root.app_id);
            holder.children = past;
            past_root = Some((resources.len(), root.id.as_str().to_owned()));
            resources.push(holder);
        }
        (resources, past_root)
    }

    /// The rows of `resources`' resources that offer nothing themselves but
    /// hold what does, so a caller reading a level at a time reaches them:
    /// [`Self::survey_rows`]'s tail, appended to `rows`.
    fn survey_containers(
        &self,
        resources: &[tilo_actions::ResourceSpec],
        past_root: Option<&(usize, String)>,
        offering: &std::collections::HashSet<String>,
        rows: &mut Vec<tilo_actions::remote::CatalogueRow>,
    ) {
        let mut containers = Vec::new();
        for (at, resource) in resources.iter().enumerate() {
            match past_root {
                // The record's resource stands in its row once: its items
                // past the page are containers under it.
                Some((holder, root)) if at == *holder => {
                    for child in &resource.children {
                        containers_into(child, root, offering, self.app_id(), &mut containers);
                    }
                }
                _ => {
                    containers_into(resource, "", offering, self.app_id(), &mut containers);
                }
            }
        }
        rows.extend(containers);
    }

    /// [`Self::survey_rows`] as `query` lists it, made only as far as the page
    /// reaches: the rows are built up to one past the asked window and the
    /// walk stops there, so a page of a record publishing thousands of
    /// actions costs its page and not all of them. Answers how many rows it
    /// admitted up to there (a lower bound once it stopped early, always at
    /// least the window's offset) and those from the `offset`th on, at most
    /// `take`, on a host that `can_render` or not.
    #[must_use]
    pub fn survey_page(
        &self,
        query: &tilo_actions::remote::CatalogueQuery,
        window: (bool, (usize, usize)),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>) {
        let (total, rows, _) = self.survey_page_arriving(query, window);
        (total, rows)
    }

    /// [`Self::survey_page`], and whether the page reads parts of a chain still on their way
    /// here (`AttachedView::listing_arriving`): a page that reads none is
    /// what it will be once every part is in, so it is answered while the rest arrive.
    #[must_use]
    pub fn survey_page_arriving(
        &self,
        query: &tilo_actions::remote::CatalogueQuery,
        window: (bool, (usize, usize)),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>, bool) {
        let (total, rows, arriving) = self.survey_page_rows(query, window);
        // A listing taken a resource at a time leaves the app's worker so: a
        // resource's few dozen actions travel as one row, not a row each.
        if query.by_resource {
            return (total, tilo_actions::remote::compact(rows), arriving);
        }
        (total, rows, arriving)
    }

    /// [`Self::survey_page_arriving`], a row per action.
    fn survey_page_rows(
        &self,
        query: &tilo_actions::remote::CatalogueQuery,
        window: (bool, (usize, usize)),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>, bool) {
        if let Some((total, rows)) = self.looked_up(query, window) {
            // What a lookup found is there; one that found nothing may be in a part on its
            // way.
            let arriving = rows.is_empty() && self.view.listing_arriving("", false);
            return (total, rows, arriving);
        }
        // A lookup (one action by its address, a resource by its name) is
        // answered from the view as it stands: an action just done leaves
        // the view to lay out its change when it is read, and what the
        // lookup names is there already unless only that frame would add
        // it. A listing reads the view laid out, or a chain's level as its
        // parts publish it, which needs no frame.
        if (query.target.is_some() || query.resource_name.is_some()) && !self.view.laid_out() {
            let tree = self.view.published_without_layout();
            let listed = Listed::Tree(tree.as_slice(), None);
            let without_layout = self.survey_page_of(listed, true, query, window);
            if without_layout.0 > 0 && !without_layout.2 {
                return without_layout;
            }
        }
        // A chain's level is walked as its parts publish it: after a write,
        // the view's whole tree, every item of every part, would be laid out
        // and built again for a page of forty.
        if let Some(parent) = query.parent_resource.as_deref()
            && let Some(level) = self.view.chain_level(parent)
        {
            return self.survey_page_of(Listed::Chain(&level), false, query, window);
        }
        let tree = self.view.published();
        self.survey_page_of(
            Listed::Tree(tree.as_slice(), Some(&tree)),
            false,
            query,
            window,
        )
    }

    /// The page a lookup `query` asks for, answered from the one resource it
    /// names and never from the whole tree the view publishes: an action by
    /// its address ([`Self::descriptor`]), a chain's item by its id or
    /// label, built alone from the part keeping it past whatever page the
    /// chain publishes, or a chain at the app's own level, built from its
    /// head part. Either costs the same however many items there are.
    /// `None` for a query that is no such lookup, or a name no chain answers.
    fn looked_up(
        &self,
        query: &tilo_actions::remote::CatalogueQuery,
        (can_render, window): (bool, (usize, usize)),
    ) -> Option<(usize, Vec<tilo_actions::remote::CatalogueRow>)> {
        if let Some(target) = &query.target {
            let rows: Vec<_> = self
                .descriptor(target)
                .map(|descriptor| {
                    tilo_actions::remote::CatalogueRow::for_action("", self.app_id(), descriptor)
                })
                .into_iter()
                .collect();
            return Some(Self::page_of(&rows, query, (can_render, window)));
        }
        let name = query.resource_name.as_deref()?;
        // A name at the app's own level is a chain it keeps beside its
        // resource: answered from the chain's head, never the listing's
        // page, which is built from every item the app's views publish. A
        // name that is no one chain — none, or several, or the resource's
        // own — is listed as before.
        if query.parent_resource.is_none() {
            let same = |text: &str| text.trim().eq_ignore_ascii_case(name);
            if same(self.kind.kind()) || same(self.view.definition().title()) {
                return None;
            }
            let named = self.view.chains_named_at_top(name);
            if let [chain] = named.as_slice() {
                let surveyed = self.rows_of_item(chain, "");
                return Some(Self::page_of(&surveyed, query, (can_render, window)));
            }
            if !named.is_empty() {
                return None;
            }
            // A name that is a resource's own id (`mail.attachment:<id>:0`)
            // is that resource, wherever the view keeps it: one it resolves
            // only when asked by id (a loaded message's attachment) is never
            // in a listing, so a name looked for there is never found.
            let resource = self.view.resource(name.trim())?;
            let surveyed = self.rows_of_item(&resource, "");
            return (!surveyed.is_empty())
                .then(|| Self::page_of(&surveyed, query, (can_render, window)));
        }
        let parent = query.parent_resource.as_deref()?;
        ui::resource::chain_of_resource_id(parent)?;
        let named = self.view.chain_items_named(parent, name);
        if named.is_empty() {
            return None;
        }
        let surveyed: Vec<_> = named
            .iter()
            .flat_map(|child| self.rows_of_item(child, parent))
            .collect();
        Some(Self::page_of(&surveyed, query, (can_render, window)))
    }

    /// The rows the chain item `child` offers under `parent`: its actions as
    /// [`Self::offered`] offers them, and a row of its own where it holds
    /// what offers actions, as a listing under the chain shows it.
    fn rows_of_item(
        &self,
        child: &tilo_actions::ResourceSpec,
        parent: &str,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        let in_chains = chain_resources(std::slice::from_ref(child));
        let mut rows = Vec::new();
        let mut offering = std::collections::HashSet::new();
        tilo_actions::api::survey_placed_under(child, parent, &mut |placed| {
            let tilo_actions::api::PlacedAction {
                parent,
                has_children,
                action,
            } = placed;
            let Some(action) = self.offered(action, &in_chains) else {
                return true;
            };
            offering.insert(action.address.resource.clone());
            rows.push(tilo_actions::remote::CatalogueRow::for_placed_action(
                "",
                self.app_id(),
                tilo_actions::api::PlacedAction {
                    parent,
                    has_children,
                    action,
                },
            ));
            true
        });
        containers_into(child, parent, &offering, self.app_id(), &mut rows);
        rows
    }

    /// [`Self::survey_page`] of what is `listed`: the tree the view publishes,
    /// with the tree as the view keeps it for a plain listing that keeps its
    /// place in it ([`SurveyProgress`]), or a chain's level as its parts
    /// publish it, whose place is kept the same way; `without_layout` when the
    /// view has not laid out what changed, which then reads past the
    /// published pages only the items a name finds.
    #[expect(
        clippy::too_many_lines,
        reason = "one walk of a level: the tree as published, then the items it \
                  leaves past its page, stopped where the page ends"
    )]
    fn survey_page_of(
        &self,
        listed: Listed<'_>,
        without_layout: bool,
        query: &tilo_actions::remote::CatalogueQuery,
        (can_render, (offset, take)): (bool, (usize, usize)),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>, bool) {
        // The level the query lists, and only the items past its pages the
        // page reaches: every row a query admits stands under the resource it
        // names (the top for none), so the resources further down, a
        // notebook's thousands of notes or a chain's issues, are not walked.
        let parent = query.parent_resource.as_deref().unwrap_or("");
        let (resources, identity) = match listed {
            Listed::Tree(resources, identity) => (
                resources,
                identity.map(|tree| Walked::Tree(std::rc::Rc::clone(tree))),
            ),
            Listed::Chain(level) => (&[][..], Some(Walked::Chain(level.clone()))),
        };
        let (level, level_in_chain) = match listed {
            Listed::Tree(..) => match level_of(resources, parent) {
                Some((level, in_chain)) => (Level::Tree(level), in_chain),
                None => return (0, Vec::new(), self.view.listing_arriving(parent, false)),
            },
            Listed::Chain(level) => (Level::Chain(level), true),
        };
        // A level a chain keeps is all of its items, so no set of them is
        // built; elsewhere only the chains among them are.
        let mut in_chains: std::collections::HashSet<String> = match level {
            Level::Tree(level) if !level_in_chain => level
                .iter()
                .filter(|child| ui::resource::chain_of_resource_id(child.id.as_str()).is_some())
                .map(|child| child.id.as_str().to_owned())
                .collect(),
            _ => std::collections::HashSet::new(),
        };
        // A plain listing keeps its place: the rows each resource of the
        // level admits are counted once, and a page starts at the resource
        // its offset falls in, so a page deep in the listing costs its rows
        // and not every resource before it.
        let plain = identity.is_some() && plain_listing(query, self.app_id());
        let mut kept = if plain { self.walked.take() } else { None };
        if let (Some(progress), Some(tree)) = (&kept, &identity)
            && !(progress.tree.same(tree)
                && progress.parent == parent
                && progress.can_render == can_render)
        {
            kept = None;
        }
        let mut start = 0usize;
        let mut start_total = 0usize;
        let mut level_complete = false;
        let mut level_counts = Vec::new();
        let mut past_at = 0usize;
        let mut past_rows = 0usize;
        let mut past_counts: Vec<usize> = Vec::new();
        let mut past_containers: Vec<Vec<tilo_actions::remote::CatalogueRow>> = Vec::new();
        let mut offering = std::collections::HashSet::new();
        if let Some(progress) = kept.take() {
            let level_rows = progress.rows_before;
            offering = progress.offering;
            level_counts = progress.counts;
            past_at = progress.past_at;
            past_rows = progress.past_rows;
            past_counts = progress.past_counts;
            past_containers = progress.past_containers;
            if offset >= level_rows {
                start = level_counts.len();
                start_total = level_rows;
                level_complete = start >= level.len();
            } else {
                // An earlier page asked again: where in the counted
                // resources its offset falls.
                let mut rows = 0usize;
                for (at, count) in level_counts.iter().enumerate() {
                    if rows.saturating_add(*count) > offset {
                        start = at;
                        start_total = rows;
                        break;
                    }
                    rows = rows.saturating_add(*count);
                }
            }
        }
        let mut walk = SurveyWalk {
            query,
            can_render,
            offset,
            take,
            // The rows up to the window's end: the offset is what a caller
            // keeping its place needs, and a window with no room needs only
            // that.
            end: offset.saturating_add(take),
            total: start_total,
            rows: Vec::new(),
            offering,
            resources: 0,
        };
        let all_chained = level_in_chain;
        let visit = |placed: tilo_actions::api::PlacedAction,
                     walk: &mut SurveyWalk<'_>,
                     in_chains: &std::collections::HashSet<String>| {
            let tilo_actions::api::PlacedAction {
                parent,
                has_children,
                action,
            } = placed;
            let Some(action) = self.offered_in(action, all_chained, in_chains) else {
                return walk.total < walk.end;
            };
            walk.offering.insert(action.address.resource.clone());
            if !plain {
                return walk.admit(tilo_actions::remote::CatalogueRow::for_placed_action(
                    "",
                    self.app_id(),
                    tilo_actions::api::PlacedAction {
                        parent,
                        has_children,
                        action,
                    },
                ));
            }
            // Every row of the level is admitted to a plain listing: the ones
            // before the window are counted, and only the window's built.
            if !walk.can_render && crate::render::host_rendered(&action) {
                return walk.total < walk.end;
            }
            walk.total = walk.total.saturating_add(1);
            if walk.total > walk.offset && walk.rows.len() < walk.take {
                walk.keep(tilo_actions::remote::CatalogueRow::for_placed_action(
                    "",
                    self.app_id(),
                    tilo_actions::api::PlacedAction {
                        parent,
                        has_children,
                        action,
                    },
                ));
            }
            walk.total < walk.end
        };
        let mut complete = level_complete;
        let mut counted: Vec<usize> = Vec::new();
        if !level_complete {
            let items = level.len().saturating_sub(start);
            let mut item_rows = 0usize;
            let mut previous: Option<usize> = None;
            complete = self.survey_level_from(level, (start, parent), &mut |local, placed| {
                if previous != Some(local) {
                    // The resources before this one admitted what they did;
                    // those the walk skipped admitted nothing.
                    if previous.is_some() {
                        counted.push(item_rows);
                    }
                    counted.resize(local, 0);
                    item_rows = 0;
                    previous = Some(local);
                }
                let before = walk.total;
                let go = visit(placed, &mut walk, &in_chains);
                if walk.total > before {
                    item_rows = item_rows.saturating_add(1);
                }
                go
            });
            // The resources walked to the end are counted whole; the one the
            // page stopped in is walked again from its start by the next one.
            if complete {
                if previous.is_some() {
                    counted.push(item_rows);
                }
                counted.resize(items, 0);
            }
        }
        // Whether the level's own items filled the page: a page of a chain's listing that
        // reached past them reads the parts older than those held, as they arrive.
        let level_filled = !complete || walk.total >= walk.end;
        // The items past the published pages, built only as far as the page
        // reaches; `past_root` is the app's own resource, which holds them,
        // or the chain a level it keeps is listed under, whose parts each
        // publish a page of what they hold.
        let past_root = if ui::resource::chain_of_resource_id(parent).is_some() {
            Some(parent.to_owned())
        } else {
            resources
                .iter()
                .find(|resource| resource.id.as_str() == self.kind.kind())
                .map(|resource| resource.id.as_str().to_owned())
        };
        // Items past the published pages are rows under that resource, so
        // only a query pinned to its level (or to an exact target, answered
        // alone in [`Self::looked_up`]) can list one of them. Any other query
        // — a top-level name, a level inside it — would walk every item it
        // holds to admit none, so it is not walked.
        let past_wanted = query
            .parent_resource
            .as_deref()
            .is_some_and(|parent| past_root.as_deref() == Some(parent));
        let mut past_built = Vec::new();
        if let Some(root) = &past_root
            && past_wanted
            && complete
            && walk.total < walk.end
        {
            // The tree on its own filled the page: the items past it are not
            // counted for nothing.
            let level_rows = walk.total;
            let mut next_child = 0usize;
            if plain {
                // The items before the last page's are not walked again: the
                // page starts at the item its offset falls in, counted by the
                // pages before as a level item is. A page that starts in the
                // level walks the past items from the first, as the tree's,
                // and counts them again.
                if offset >= level_rows {
                    let past_offset = offset.saturating_sub(level_rows);
                    let mut rows = 0usize;
                    let mut at = 0usize;
                    for (index, count) in past_counts.iter().enumerate() {
                        if rows.saturating_add(*count) > past_offset {
                            at = index;
                            break;
                        }
                        rows = rows.saturating_add(*count);
                        at = index.saturating_add(1);
                    }
                    next_child = at;
                    past_rows = rows;
                    walk.total = level_rows.saturating_add(past_rows);
                } else {
                    past_at = 0;
                    past_rows = 0;
                    past_counts.clear();
                    past_containers.clear();
                    next_child = 0;
                }
            }
            // A query naming a resource is answered by the labels of the
            // items past the page, building only those that match: a note
            // named by its title costs its title, not the notebook. A chain
            // names its items past the parts' pages in [`Self::looked_up`],
            // from whichever part keeps one.
            if let Some(name) = query.resource_name.as_deref() {
                let named = if level_in_chain {
                    Vec::new()
                } else {
                    self.view.past_page_named(name)
                };
                for child in named {
                    in_chains.extend(chain_resources(std::slice::from_ref(&child)));
                    complete =
                        tilo_actions::api::survey_placed_under(&child, root, &mut |placed| {
                            visit(placed, &mut walk, &in_chains)
                        });
                    past_built.push(child);
                    if !complete || walk.total >= walk.end {
                        break;
                    }
                }
            }
            let past_len = if query.resource_name.is_some() || without_layout {
                0
            } else {
                self.view.past_page_len_under(root)
            };
            // The rows an item past the pages is taken to offer: what the
            // level's own items offered each. A window is counted in rows, and an
            // issue offers a couple of dozen, so building as many items as
            // there are rows left would build every item to the end.
            let rows_each = level_rows.checked_div(level.len()).unwrap_or(1).max(1);
            while complete && walk.total < walk.end && next_child < past_len {
                // The items the rows the page still needs take: each item may
                // offer fewer, so the walk asks again from where this window
                // ended.
                let children =
                    self.view
                        .past_page_range_under(root, next_child, walk.items_left(rows_each));
                if children.is_empty() {
                    break;
                }
                let mut met = 0usize;
                for child in children {
                    // A chain part keeps what it offers an agent, whichever
                    // page of the record it is listed on.
                    in_chains.extend(chain_resources(std::slice::from_ref(&child)));
                    let before = walk.total;
                    // As the page's items are walked: the item's own actions,
                    // and none further down its nested resources.
                    complete = tilo_actions::api::survey_placed_level(
                        std::slice::from_ref(&child),
                        root,
                        &mut |placed| visit(placed, &mut walk, &in_chains),
                    );
                    let offers = walk.offering.contains(child.id.as_str());
                    if plain && complete {
                        let admitted = walk.total.saturating_sub(before);
                        past_counts.resize(next_child, 0);
                        if past_counts.len() == next_child {
                            past_counts.push(admitted);
                            past_rows = past_rows.saturating_add(admitted);
                        }
                        while past_containers.len() <= next_child {
                            past_containers.push(Vec::new());
                        }
                        if let Some(found) = past_containers.get_mut(next_child)
                            && !offers
                        {
                            containers_into(&child, root, &walk.offering, self.app_id(), found);
                        }
                    } else {
                        past_built.push(child);
                    }
                    next_child = next_child.saturating_add(1);
                    met = met.saturating_add(1);
                    if !complete || walk.total >= walk.end {
                        break;
                    }
                }
                if met == 0 {
                    break;
                }
            }
            if plain {
                past_at = next_child;
            }
        }
        if complete && walk.total < walk.end {
            let mut containers = Vec::new();
            // A resource of the level that offers nothing itself but holds
            // what does stands in a row of its own, which a caller reading a
            // level at a time goes on into.
            for at in 0..level.len() {
                // A chain's item that offers is not built again to say so.
                if let Level::Chain(chain) = level
                    && chain.id(at).is_some_and(|id| walk.offering.contains(id))
                {
                    continue;
                }
                let Some(child) = self.level_item(level, at) else {
                    continue;
                };
                let child = child.as_ref();
                if !walk.offering.contains(child.id.as_str())
                    && self.offers_below(
                        child,
                        level_in_chain
                            || ui::resource::chain_of_resource_id(child.id.as_str()).is_some(),
                    )
                {
                    containers.push(container_row(child, parent, self.app_id()));
                }
            }
            if let Some(root) = &past_root {
                if plain {
                    // The container rows each item past the page stands in,
                    // decided when its item was walked: the earlier ones were
                    // walked by earlier pages.
                    for found in &past_containers {
                        containers.extend(found.iter().cloned());
                    }
                } else {
                    for child in &past_built {
                        // A row of its own only for one that offers nothing
                        // itself, as the level's items stand.
                        if !walk.offering.contains(child.id.as_str()) {
                            containers_into(
                                child,
                                root,
                                &walk.offering,
                                self.app_id(),
                                &mut containers,
                            );
                        }
                    }
                }
            }
            for row in containers {
                if !walk.admit(row) {
                    break;
                }
            }
        }
        if plain && let Some(tree) = identity {
            let mut counts = level_counts;
            counts.truncate(start);
            counts.extend(counted.iter().copied());
            let rows_before = start_total.saturating_add(counted.iter().copied().sum::<usize>());
            *self.walked.borrow_mut() = Some(SurveyProgress {
                tree,
                parent: parent.to_owned(),
                can_render,
                counts,
                rows_before,
                offering: walk.offering,
                past_at,
                past_rows,
                past_counts,
                past_containers,
            });
        }
        let arriving = self
            .view
            .listing_arriving(parent, level_in_chain && level_filled);
        (walk.total, walk.rows, arriving)
    }

    /// The resource at `at` of `level`, as the member may reach it: a chain's
    /// item is built only here, when the walk reaches it.
    fn level_item<'a>(
        &self,
        level: Level<'a>,
        at: usize,
    ) -> Option<std::borrow::Cow<'a, tilo_actions::ResourceSpec>> {
        match level {
            Level::Tree(level) => level.get(at).map(std::borrow::Cow::Borrowed),
            Level::Chain(level) => self
                .view
                .chain_level_item(level, at)
                .map(std::borrow::Cow::Owned),
        }
    }

    /// [`tilo_actions::api::survey_placed_level_items`] of `level` from its
    /// `start`th resource on, each placed under `parent` and told to `visit`
    /// with its index counted from `start`: a chain's items are built one at
    /// a time as the walk reaches them, and none past where it stopped.
    fn survey_level_from(
        &self,
        level: Level<'_>,
        (start, parent): (usize, &str),
        visit: &mut dyn FnMut(usize, tilo_actions::api::PlacedAction) -> bool,
    ) -> bool {
        match level {
            Level::Tree(level) => tilo_actions::api::survey_placed_level_items(
                level.get(start..).unwrap_or_default(),
                parent,
                visit,
            ),
            Level::Chain(chain) => (start..chain.len()).all(|at| {
                self.level_item(level, at).is_none_or(|item| {
                    tilo_actions::api::survey_placed_level_items(
                        std::slice::from_ref(item.as_ref()),
                        parent,
                        &mut |_, placed| visit(at.saturating_sub(start), placed),
                    )
                })
            }),
        }
    }

    /// Whether anything below `resource` is offered ([`Self::offered`]), its
    /// subtree kept by a chain when `in_chain`: walked only as far as the
    /// first that is.
    fn offers_below(&self, resource: &tilo_actions::ResourceSpec, in_chain: bool) -> bool {
        let mut offered = false;
        for child in &resource.children {
            let chained =
                in_chain || ui::resource::chain_of_resource_id(child.id.as_str()).is_some();
            tilo_actions::api::survey_placed_under(child, resource.id.as_str(), &mut |placed| {
                let held: std::collections::HashSet<String> = chained
                    .then(|| placed.action.address.resource.clone())
                    .into_iter()
                    .collect();
                offered = self.offered(placed.action, &held).is_some();
                !offered
            });
            if offered {
                return true;
            }
        }
        false
    }

    /// What `surveyed` ([`Self::survey_rows`]) lists as `query` asks, on a
    /// host that `can_render` or not: how many rows it admits, and
    /// those from the `offset`th on, at most `take`. Only the page is
    /// handed back, each row where it stands.
    #[must_use]
    pub fn page_of(
        surveyed: &[tilo_actions::remote::CatalogueRow],
        query: &tilo_actions::remote::CatalogueQuery,
        (can_render, (offset, take)): (bool, (usize, usize)),
    ) -> (usize, Vec<tilo_actions::remote::CatalogueRow>) {
        let mut total = 0usize;
        let mut page = Vec::new();
        for row in surveyed {
            let rendered = row
                .action
                .as_ref()
                .is_some_and(|descriptor| !can_render && crate::render::host_rendered(descriptor));
            if rendered || !query.admits(row) {
                continue;
            }
            if total >= offset && page.len() < take {
                page.push(row.clone());
            }
            total = total.saturating_add(1);
        }
        (total, page)
    }

    /// [`Self::survey`] for the one action `address` names: what a host that
    /// already knows the action asks for, built from the one resource it
    /// names and never from the whole tree the view publishes, however many
    /// items there are. An action published for a chain's parts, or on an
    /// item a chain keeps, is offered as the whole listing offers it.
    #[must_use]
    pub fn descriptor(&self, address: &ActionAddress) -> Option<ActionDescriptor> {
        let descriptor = self.view.descriptor(address)?;
        // Only an effect is offered by where it stands: one on a chain or an
        // item it keeps is the chain's, as the listing offers it.
        let in_chains = if descriptor.kind == tilo_actions::OperationKind::Effect {
            self.chained(address.resource.as_str())
        } else {
            std::collections::HashSet::new()
        };
        self.offered(descriptor, &in_chains)
    }

    /// `resource` alone when it is a chain or an item a chain keeps, found
    /// in the part keeping it: the resources of [`chain_resources`] an
    /// action on it is offered by, without the tree that lists them all.
    fn chained(&self, resource: &str) -> std::collections::HashSet<String> {
        (ui::resource::chain_of_resource_id(resource).is_some() || self.view.in_chain(resource))
            .then(|| resource.to_owned())
            .into_iter()
            .collect()
    }

    /// `descriptor` as [`Self::survey`] offers it, of a tree whose chains
    /// publish the resources `in_chains`: `None` for one this member is
    /// not offered.
    fn offered(
        &self,
        descriptor: ActionDescriptor,
        in_chains: &std::collections::HashSet<String>,
    ) -> Option<ActionDescriptor> {
        self.offered_in(descriptor, false, in_chains)
    }

    /// [`Self::offered`] of a level whose every resource is kept by a chain
    /// when `all_chained`: no set of their ids is built for it.
    fn offered_in(
        &self,
        mut descriptor: ActionDescriptor,
        all_chained: bool,
        in_chains: &std::collections::HashSet<String>,
    ) -> Option<ActionDescriptor> {
        // An action for people only (copying a link to the clipboard) means
        // nothing to an agent; the host acting for a person offers it (a
        // reviewed feedback report a person files).
        if self.automated() && !descriptor.contract.reaches_agents() {
            return None;
        }
        if !(self.offers_in(&descriptor, all_chained, in_chains)
            || (self.runs_effects && self.automated && offered_when_host_renders(&descriptor)))
        {
            return None;
        }
        if self.runs_effects && descriptor.kind != tilo_actions::OperationKind::Document {
            descriptor.params.retain(|param| param.name != PATH_PARAM);
        }
        Some(descriptor)
    }

    /// Whether [`Self::survey`] lists `descriptor`, of a tree whose chains
    /// publish the resources `in_chains`.
    fn offers(
        &self,
        descriptor: &ActionDescriptor,
        in_chains: &std::collections::HashSet<String>,
    ) -> bool {
        self.offers_in(descriptor, false, in_chains)
    }

    /// [`Self::offers`] where every resource is kept by a chain when
    /// `all_chained`.
    fn offers_in(
        &self,
        descriptor: &ActionDescriptor,
        all_chained: bool,
        in_chains: &std::collections::HashSet<String>,
    ) -> bool {
        match descriptor.kind {
            tilo_actions::OperationKind::Document => true,
            // A chain the app keeps beside the record publishes its parts'
            // document actions routed to the part they change: document
            // operations all the same.
            // So does what the app keeps for its whole workspace, carried out on that
            // document ([`Self::kept_for_workspace`]).
            tilo_actions::OperationKind::Effect
                if all_chained
                    || in_chains.contains(descriptor.address.resource.as_str())
                    || self.kept_for_workspace(descriptor) =>
            {
                true
            }
            tilo_actions::OperationKind::Effect => {
                member_own(descriptor)
                    || (self.offers_reads() && descriptor.contract.kind == policy::ActionKind::Read)
                    || (self.runs_effects && offered_to_agents(descriptor))
            }
            // The read of what the app keeps for its whole workspace moves no window: the
            // server's own person, who has no window of it there either, is offered it too.
            tilo_actions::OperationKind::View => {
                (self.runs_views || self.kept_for_workspace(descriptor))
                    && offered_to_agents(descriptor)
            }
        }
    }

    /// Whether `descriptor` is of what the app keeps for its whole workspace beside this
    /// document ([`collab_types::WORKSPACE_DOCUMENTS`]: Chat's of the workspace's agents and
    /// emoji): that document's own resource, which its kind names, or an item of it.
    fn kept_for_workspace(&self, descriptor: &ActionDescriptor) -> bool {
        let resource = descriptor.address.resource.as_str();
        collab_types::WORKSPACE_DOCUMENTS.iter().any(|kept| {
            kept.app == self.app_id
                && resource
                    .strip_prefix(kept.kind)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }

    /// For an agent or the host acting for its person, whether the
    /// operation at `address` is an effect or
    /// view of the record, which it then runs to the end, refused when it
    /// is not one [`Self::survey`] offers or names where to write. A view of
    /// the agent's own caret runs as any member's does.
    fn agent_effect(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<bool, ApiError> {
        if !self.runs_effects {
            return Ok(false);
        }
        // The one action, resolved by its address: never every resource the
        // record publishes, which grows with everything it holds.
        let Some(descriptor) = self.view.descriptor(address) else {
            // The API refuses what the view does not publish.
            return Ok(false);
        };
        if descriptor.kind == tilo_actions::OperationKind::Document
            || (descriptor.kind == tilo_actions::OperationKind::View
                && !self.runs_views
                && !self.kept_for_workspace(&descriptor))
        {
            return Ok(false);
        }
        // A view that moves only the agent's own caret, or types at it, is
        // the agent's own to run as the API admits it for any member,
        // though it answers nothing a plan is offered it for.
        let device = tagged(&descriptor, tilo_actions::OWN_DEVICE_SCOPE)
            || tagged(&descriptor, tilo_actions::ATTENDED_TAG)
            || tagged(&descriptor, tilo_actions::HOST_RENDER_TAG);
        if descriptor.kind == tilo_actions::OperationKind::View
            && descriptor.contract.is_presence_view()
            && !device
        {
            return Ok(false);
        }
        if !descriptor.contract.reaches_agents()
            || !self.offers(&descriptor, &self.chained(address.resource.as_str()))
        {
            return Err(not_offered(&descriptor));
        }
        if arguments.contains_key(PATH_PARAM) {
            return Err(ApiError::Unavailable {
                code: "effect_path".into(),
                reason: format!(
                    "an agent's {} writes to no path: leave `{PATH_PARAM}` out and what it \
                     makes is kept as content",
                    descriptor.action_label
                ),
            });
        }
        Ok(true)
    }

    /// Discover one existing resource's local view metadata under the caller's
    /// current read policy. Views are inert; `invoke` remains document-only.
    #[must_use]
    pub fn resource_views(
        &self,
        id: &str,
        kind: &str,
        permissions: &PermissionContext,
    ) -> Vec<ActionDescriptor> {
        let wanted = tilo_actions::remote::ViewResourceQuery {
            id: id.to_owned(),
            kind: kind.to_owned(),
        };
        // An item of a chain the app keeps beside its record (an issue in
        // one of the tracker's parts) is in no record's document alone: the
        // view publishes it, with the view its opener shows.
        if let Some(published) = published_resource(&self.view.published(), id) {
            return views_of(published, self.app_id, &wanted, permissions);
        }
        self.read(|doc| discover_views(self.kind, self.app_id, doc, &wanted, permissions))
            .unwrap_or_default()
    }

    /// Perform one published operation through the view, the API a
    /// person's click takes: permission, confirmation and strict argument
    /// binding exactly as for a person. The change it made is taken with
    /// [`Self::take_mutation`].
    pub fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_with(None, None, address, arguments, confirm)
    }

    /// [`Self::invoke`] showing the appearance an API admitted.
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub fn invoke_appearing(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
        appearance: Option<tilo_actions::Appearance>,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_within(None, None, appearance, address, arguments, confirm)
    }

    /// [`Self::invoke`] as `invoker`, with `context` as the ambient entry of
    /// this one invocation (its source invocation and cause). `None`s act
    /// as the member with the view's own context.
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub fn invoke_with(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        context: Option<tilo_actions::ActionContext>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_within(invoker, context, None, address, arguments, confirm)
    }

    /// [`Self::invoke_with`] showing `appearance`. Only an API that admitted
    /// it passes one; `None` clears any appearance the invoker or context
    /// carried.
    pub(crate) fn invoke_within(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        context: Option<tilo_actions::ActionContext>,
        appearance: Option<tilo_actions::Appearance>,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ActionOutput, ApiError> {
        let effect = self.agent_effect(address, arguments)?;
        // An agent goes by the name of the session the call came in, as its window's mark names
        // it, whatever name this replica was opened under: one agent's replica serves each of its
        // sessions.
        let session = context
            .as_ref()
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
            .and_then(|group| group.session.clone());
        let previous = context.as_ref().map(|_| self.view.context().cloned());
        if context.is_some() {
            self.view.set_context(context);
        }
        let invoker = invoker.or_else(|| {
            let session = session?;
            let acting = self.view.invoker_appearing(None);
            acting
                .acting
                .is_agent()
                .then(|| acting.named(Some(session)))
        });
        let output = match (invoker, effect) {
            // An agent's effect answers with what it ended as: a job (a
            // render, an export) is waited for, bounded.
            (invoker, true) => {
                let invoker = match invoker {
                    Some(invoker) => invoker.with_appearance(appearance),
                    None => self.view.invoker_appearing(appearance),
                };
                self.view
                    .invoke_as_awaiting(invoker, address, arguments, confirm, EFFECT_TIMEOUT)
            }
            (Some(invoker), false) => self.view.invoke_as(
                invoker.with_appearance(appearance),
                address,
                arguments,
                confirm,
            ),
            (None, false) => self
                .view
                .invoke_appearing(address, arguments, confirm, appearance),
        };
        if let Some(previous) = previous {
            self.view.set_context(previous);
        }
        let output = output?;
        let landed = self.view.landed();
        self.place = landed
            .and_then(|envelope| self.read(|doc| self.kind.lands(doc, &envelope.action)))
            .flatten()
            .or_else(|| {
                (address.resource != self.kind.kind())
                    .then(|| Place::Item(address.resource.clone()))
            });
        Ok(output)
    }

    /// Read the record's document.
    pub fn read<R>(&self, read: impl FnOnce(&automerge::AutoCommit) -> R) -> Option<R> {
        self.view.read(read)
    }

    /// The tree the record's view publishes to this member now.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.view.resources()
    }

    /// This member's changes since the last call, or `None` when it
    /// changed nothing. The first call on a new record returns the whole
    /// document as a snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when the changes could not be collected:
    /// a write that made them is not saved, and must not be answered as
    /// done. They stay in the record and go with the next call.
    pub fn take_mutation(&mut self) -> Result<Option<DocumentMutation>, AppError> {
        self.view.take_changes().map_err(AppError::Merge)
    }

    /// A member's access read again: at a later time, what was granted until
    /// a time is held to the new one; after the workspace's policy or sandbox
    /// moved, the replica acts under what they say now. Nothing of the record
    /// is opened again for it, which costs more the more it holds.
    ///
    /// # Errors
    ///
    /// [`AppError::Launch`] for any actor but a member.
    pub fn renew(&mut self, actor: ReplicaActor) -> Result<(), AppError> {
        // The same access read again, with no grant lapsed since: the replica keeps the
        // permissions it has, and with them everything its view laid out. Taking them in
        // again laid the view out whole, which for a tracker of thousands of issues cost the
        // next listing as much as the rest of it.
        let later = HeldAccess::of(&actor);
        if let (Some(held), Some(later)) = (&self.held_access, &later)
            && held.decides_alike(later)
        {
            return Ok(());
        }
        let ReplicaActor::Member {
            access,
            sandbox,
            unix_time,
            ..
        } = actor
        else {
            return Err(AppError::Launch("only a member's access is renewed".into()));
        };
        self.view
            .set_permissions(member_permissions(access, sandbox, unix_time));
        self.held_access = later;
        Ok(())
    }

    /// Merge another member's durable change into the view's record.
    /// Changes that build on nothing this record holds belong to another
    /// record and are left alone; one already merged changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when a whole document is not one of this
    /// record's kind.
    pub fn apply(&mut self, mutation: &DocumentMutation) -> Result<(), AppError> {
        if let DocumentMutation::Snapshot(bytes) = mutation {
            let holds = ui::tilo_resource::Doc::load(bytes)
                .is_ok_and(|incoming| self.kind.holds_or_older(&incoming));
            if !holds {
                return Err(AppError::Merge("the document is not this record".into()));
            }
        }
        self.view.merge(mutation).map_err(AppError::Merge)?;
        Ok(())
    }

    /// Whether `record` is one the app keeps beside this one — a part of a
    /// chain it publishes ([`ui::framework::Chain`]), such as a chat's
    /// messages — which the replica holds with its own
    /// ([`Self::apply_document`]) and the app's actions reach through the
    /// chain, as in a window.
    #[must_use]
    pub fn keeps_beside(&self, record: &DocumentId) -> bool {
        self.view.beside(record).is_some()
    }

    /// Take this replica's records to come from its workspace's hosts, which
    /// send a public workspace's records only once they are asked for: a
    /// chain it holds no part of asks for its head, and writes to it wait
    /// for the answer ([`Self::wanted_resources`]).
    pub fn ask_workspace_for_missing(&self) {
        self.view.ask_workspace_for_missing();
    }

    /// What this replica's records ask the workspace's hosts for.
    #[must_use]
    pub fn wanted_resources(&self) -> Vec<DocumentId> {
        self.view.wanted_resources()
    }

    /// The workspace's hosts answered `record`, which this replica asked for: they hold none
    /// of it, or it is another person's own, which never reaches this replica.
    pub fn workspace_answered(
        &mut self,
        record: DocumentId,
        answer: ui::resource::WorkspaceAnswer,
    ) {
        self.view.workspace_answered(record, answer);
    }

    /// The records held beside this one ([`Self::keeps_beside`]), by id.
    #[must_use]
    pub fn resources_beside(&self) -> Vec<DocumentId> {
        self.view
            .resources_beside()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect()
    }

    /// Merge another member's durable change into `record`: this
    /// replica's own ([`Self::apply`]), or one its app keeps beside it
    /// ([`Self::keeps_beside`]), opened from a whole document the first
    /// time. Changes that build on nothing that record holds are left
    /// alone.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when the record is neither, or refuses
    /// the change.
    pub fn apply_document(
        &mut self,
        record: &DocumentId,
        mutation: &DocumentMutation,
    ) -> Result<(), AppError> {
        if record == self.document_id() {
            return self.apply(mutation);
        }
        self.view
            .merge_resource(record, mutation)
            .map(drop)
            .map_err(AppError::Merge)
    }

    /// This member's changes to the records held beside this one
    /// since the last call, by record: a part it started whole, any other
    /// as its new changes.
    ///
    /// # Errors
    ///
    /// As [`Self::take_mutation`]: none is taken then, every part's changes
    /// go with the next call.
    pub fn take_document_mutations(
        &mut self,
    ) -> Result<Vec<(DocumentId, DocumentMutation)>, AppError> {
        Ok(self
            .take_document_handover()?
            .into_iter()
            .map(|handed| {
                let written = handed.mutation();
                (handed.document, written)
            })
            .collect())
    }

    /// [`Self::take_document_mutations`] as a worker hands them to its host:
    /// this member's changes, and, for a part it started, the part whole.
    ///
    /// # Errors
    ///
    /// As [`Self::take_mutation`].
    pub fn take_document_handover(&mut self) -> Result<Vec<BesideChange>, AppError> {
        self.view
            .take_resources_beside_changes()
            .map_err(AppError::Merge)
    }

    /// Read `record`: this replica's own, or one held beside it.
    pub fn read_resource<R>(
        &self,
        record: &DocumentId,
        read: impl FnOnce(&automerge::AutoCommit) -> R,
    ) -> Option<R> {
        if record == self.document_id() {
            self.read(read)
        } else {
            self.view.read_resource(record, read)
        }
    }
}

/// The ids of every resource the chains among `resources` publish.
fn chain_resources(resources: &[tilo_actions::ResourceSpec]) -> std::collections::HashSet<String> {
    let mut in_chains = std::collections::HashSet::new();
    for resource in resources
        .iter()
        .flat_map(|resource| std::iter::once(resource).chain(&resource.children))
    {
        if ui::resource::chain_of_resource_id(resource.id.as_str()).is_some() {
            chain_ids(resource, &mut in_chains);
        }
    }
    in_chains
}

/// The rows of the resources at and below `resource` (under `parent`) that
/// offer nothing of their own but hold resources that do, into `out`; whether
/// anything at or below it is offered.
/// The page [`Replica::survey_page`] makes as it walks: the rows it admits up
/// to `end`, and those of the window among them.
struct SurveyWalk<'a> {
    query: &'a tilo_actions::remote::CatalogueQuery,
    can_render: bool,
    offset: usize,
    take: usize,
    end: usize,
    total: usize,
    rows: Vec<tilo_actions::remote::CatalogueRow>,
    /// The resources a row was admitted for: what the containers after the
    /// rows are decided by.
    offering: std::collections::HashSet<String>,
    /// The resources the page holds rows of so far.
    resources: usize,
}

impl SurveyWalk<'_> {
    /// Take `row` into the page: whether the walk goes on.
    fn admit(&mut self, row: tilo_actions::remote::CatalogueRow) -> bool {
        let rendered = row
            .action
            .as_ref()
            .is_some_and(|descriptor| !self.can_render && crate::render::host_rendered(descriptor));
        if !rendered && self.query.admits(&row) {
            self.total = self.total.saturating_add(1);
            if self.total > self.offset && self.rows.len() < self.take {
                self.keep(row);
            }
        }
        self.total < self.end
    }

    /// How many more items, each taken to offer `rows_each` rows, the walk
    /// may still need: those its rows still take, and on a page bounded in
    /// resources no more than reach the page and the one past its bound.
    /// Rows are only a guess at an item, so a page asked for forty issues
    /// would otherwise build as many as a page's worth of rows guesses.
    fn items_left(&self, rows_each: usize) -> usize {
        let by_rows = self
            .end
            .saturating_sub(self.total)
            .max(1)
            .div_ceil(rows_each);
        let Some(most) = self.query.resource_bound() else {
            return by_rows;
        };
        let to_page = self.offset.saturating_sub(self.total).div_ceil(rows_each);
        let on_page = most.saturating_add(1).saturating_sub(self.resources).max(1);
        by_rows.min(to_page.saturating_add(on_page))
    }

    /// Put `row` on the page. A page bounded in resources
    /// ([`tilo_actions::remote::CatalogueQuery::max_resources`]) ends once it
    /// holds a row of one past the bound: the caller sees from it that more
    /// follow and cuts the page before it, and no resource further on is
    /// built.
    fn keep(&mut self, row: tilo_actions::remote::CatalogueRow) {
        if self
            .rows
            .last()
            .is_none_or(|last| last.resource.id != row.resource.id)
        {
            self.resources = self.resources.saturating_add(1);
            if self
                .query
                .resource_bound()
                .is_some_and(|most| self.resources > most)
            {
                self.end = self.total;
            }
        }
        self.rows.push(row);
    }
}

/// What a plain listing of one level has walked to: how many rows each
/// resource before the page admitted, so the next page starts where the last
/// ended instead of at the level's first resource. Kept for the published
/// tree it counted; a change to that tree drops it.
struct SurveyProgress {
    /// The published tree, or chain level, the counts are of.
    tree: Walked,
    /// The level listed: the resource whose children it holds (`""` the top).
    parent: String,
    /// Whether this device renders rows, which decides what is admitted.
    can_render: bool,
    /// Rows admitted by each counted resource of the level, in order.
    counts: Vec<usize>,
    /// Rows admitted by the counted resources, counted once.
    rows_before: usize,
    /// The resources a row was admitted for among the counted ones: what the
    /// container rows are decided by.
    offering: std::collections::HashSet<String>,
    /// The resources past the page the tree publishes that were counted, in
    /// order, and the rows they admitted.
    past_at: usize,
    past_rows: usize,
    past_counts: Vec<usize>,
    /// The container rows each item past the page stands in, decided when its
    /// item was walked.
    past_containers: Vec<Vec<tilo_actions::remote::CatalogueRow>>,
}

/// Whether `query` admits every row a level offers, so a listing of it can be
/// counted a resource at a time and resumed where its last page ended: no
/// name, action, target or view narrows it, and the app it names is this one.
fn plain_listing(query: &tilo_actions::remote::CatalogueQuery, app_id: &str) -> bool {
    query.target.is_none()
        && query.action.is_none()
        && query.resource_name.is_none()
        && query.view_resource.is_none()
        && query.needle.is_empty()
        && query.app_id.as_deref().is_none_or(|app| app == app_id)
}

fn containers_into(
    resource: &tilo_actions::ResourceSpec,
    parent: &str,
    offering: &std::collections::HashSet<String>,
    app_id: &str,
    out: &mut Vec<tilo_actions::remote::CatalogueRow>,
) -> bool {
    let id = resource.id.as_str();
    let mut below = false;
    for child in &resource.children {
        below |= containers_into(child, id, offering, app_id, out);
    }
    let own = offering.contains(id);
    if below && !own {
        out.push(container_row(resource, parent, app_id));
    }
    own || below
}

/// The row of `resource`, under `parent`, that offers nothing itself but
/// holds what does: what a caller reading a level at a time goes on into.
fn container_row(
    resource: &tilo_actions::ResourceSpec,
    parent: &str,
    app_id: &str,
) -> tilo_actions::remote::CatalogueRow {
    tilo_actions::remote::CatalogueRow {
        offered: None,
        provider: String::new(),
        app_id: app_id.to_owned(),
        parent_resource: parent.to_owned(),
        resource: tilo_actions::remote::CatalogueResource {
            id: resource.id.as_str().to_owned(),
            kind: resource.kind.clone(),
            label: resource.label.clone(),
            presentation: resource.presentation.clone(),
        },
        action: None,
        has_children: true,
        visibility: None,
        surface: None,
    }
}

/// The resources `parent` holds in `resources` (the forest itself for the
/// top, `""`), and whether a chain keeps them: the level a listing under
/// `parent` lists. `None` when no resource there is `parent`.
fn level_of<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    parent: &str,
) -> Option<(&'a [tilo_actions::ResourceSpec], bool)> {
    fn find<'a>(
        resources: &'a [tilo_actions::ResourceSpec],
        parent: &str,
        in_chain: bool,
    ) -> Option<(&'a [tilo_actions::ResourceSpec], bool)> {
        resources.iter().find_map(|resource| {
            let chained =
                in_chain || ui::resource::chain_of_resource_id(resource.id.as_str()).is_some();
            if resource.id.as_str() == parent {
                return Some((resource.children.as_slice(), chained));
            }
            find(&resource.children, parent, chained)
        })
    }
    if parent.is_empty() {
        return Some((resources, false));
    }
    find(resources, parent, false)
}

/// The ids of `resource` and everything published under it.
fn chain_ids(resource: &tilo_actions::ResourceSpec, out: &mut std::collections::HashSet<String>) {
    out.insert(resource.id.as_str().to_owned());
    for child in &resource.children {
        chain_ids(child, out);
    }
}

/// Device name a record created here is seeded on.
const DEVICE: &str = "agent";

/// A new record of `kind`: its genesis, with the identity its route names,
/// as every member creating it derives it, then what its app seeds a record
/// `creator` creates with.
/// The record a view opens: minted, or the one whose changes were handed over.
/// A record kept before the app changed its storage is brought up to the app's
/// version when its view opens, as in a window.
fn seeded(
    definition: &ApplicationDefinition,
    kind: ResourceKind,
    workspace: &str,
    member: &MemberId,
    seed: Seed,
) -> Result<(DocumentCheckout, Vec<u8>, bool), AppError> {
    let (checkout, bytes, created) = match seed {
        Seed::New => {
            let checkout = if definition.keeps_workspace_resource() {
                DocumentCheckout::package_primary(definition.resource_type.clone())
            } else {
                DocumentCheckout::new(DocumentId::minted(), definition.resource_type.clone())
            };
            let bytes = genesis(
                definition,
                workspace,
                &checkout,
                record_resource(definition, kind, &checkout),
                member,
            )?;
            (checkout, bytes, true)
        }
        Seed::NewAt(record) => {
            if definition.keeps_workspace_resource() != record.is_package_primary() {
                return Err(AppError::Merge(
                    "record route does not match the app's workspace scope".into(),
                ));
            }
            let checkout =
                DocumentCheckout::new(record.clone(), record_kind(definition, &record, None));
            let bytes = genesis(
                definition,
                workspace,
                &checkout,
                record_resource(definition, kind, &checkout),
                member,
            )?;
            (checkout, bytes, true)
        }
        Seed::Existing { record, changes } => {
            if definition.keeps_workspace_resource() != record.is_package_primary() {
                return Err(AppError::Merge(
                    "record route does not match the app's workspace scope".into(),
                ));
            }
            let mut doc = automerge::AutoCommit::new();
            for change in &changes {
                tilo_automerge::ApplyIncoming::load_incoming(&mut doc, change)
                    .map_err(|error| AppError::Merge(error.to_string()))?;
            }
            let held = ui::resource::identity(&doc).filter(|found| found.id == record);
            let checkout = DocumentCheckout::new(
                record.clone(),
                record_kind(definition, &record, held.as_ref()),
            );
            (checkout, doc.save(), false)
        }
    };
    let doc =
        ui::tilo_resource::Doc::load(&bytes).map_err(|error| AppError::Merge(error.to_string()))?;
    let opened_as = record_resource(definition, kind, &checkout);
    if !opened_as.holds_or_older(&doc) {
        opened_as
            .read(&doc)
            .map_err(|error| AppError::Merge(error.to_string()))?;
    }
    Ok((checkout, bytes, created))
}

/// The resource kind the record `record` of `definition`'s app is of: the identity it was handed
/// over with, else the kind the app keeps it under — a document every app of the workspace opens
/// (Terminal's machine secrets), or a protected record of a fixed id (`terminal.history`) — and
/// the app's own kind for a record of its own. A host opening the record binds it to this kind,
/// as the device that made it did.
fn record_kind(
    definition: &ApplicationDefinition,
    record: &DocumentId,
    held: Option<&DocumentCheckout>,
) -> ui::resource::ResourceType {
    held.map(|found| found.kind.clone())
        .or_else(|| {
            collab_types::workspace_document(definition.app_id(), record.as_str())
                .map(|kept| ui::resource::ResourceType::new(kept.kind))
        })
        .or_else(|| {
            tilo_actions::policy::roles::protected_resource_kind(
                definition.app_id(),
                record.as_str(),
            )
            .map(ui::resource::ResourceType::new)
        })
        .unwrap_or_else(|| definition.resource_type.clone())
}

/// [`record_kind`] as this process reads and initializes records of that kind: the app's own
/// kind's reader for a record of its own, and the kind's reader for one the app keeps besides.
fn record_resource(
    definition: &ApplicationDefinition,
    kind: ResourceKind,
    checkout: &DocumentCheckout,
) -> ResourceKind {
    if checkout.kind == definition.resource_type {
        return kind;
    }
    ui::framework::hosted_kinds()
        .get(checkout.kind.as_str())
        .copied()
        .unwrap_or(kind)
}

fn genesis(
    definition: &ApplicationDefinition,
    workspace: &str,
    checkout: &DocumentCheckout,
    kind: ResourceKind,
    creator: &MemberId,
) -> Result<Vec<u8>, AppError> {
    let launch = |error: &dyn std::fmt::Display| AppError::Launch(error.to_string());
    let cache = PortableStorage::new("agent-records", DEVICE).map_err(|e| launch(&e))?;
    let records = ResourceOwners::portable(cache)
        .open(definition.app_id(), workspace)
        .map_err(|e| launch(&e))?;
    records
        .reserve_resource(checkout)
        .map_err(|e| launch(&e))?
        .initialize_seeded(
            |doc| kind.initialize(doc).map_err(|error| error.to_string()),
            |doc| ui::framework::seed_resource(kind.kind(), doc, creator, DEVICE),
        )
        .map_err(|e| launch(&e))?;
    records
        .read(&checkout.id, |doc| doc.clone().save())
        .ok_or_else(|| AppError::Launch("the new record did not open".into()))
}

fn discover_views(
    resource_kind: ResourceKind,
    app_id: &str,
    doc: &automerge::AutoCommit,
    wanted: &tilo_actions::remote::ViewResourceQuery,
    permissions: &PermissionContext,
) -> Vec<ActionDescriptor> {
    let published = match resource_kind.child(doc, &wanted.id) {
        Ok(Some(child)) if child.id.as_str() == wanted.id => child,
        _ => match resource_kind.publish(resource_kind.kind(), doc) {
            Ok(published) => published,
            Err(_) => return Vec::new(),
        },
    };
    views_of(published, app_id, wanted, permissions)
}

/// The resource `id` among `resources` and their children, as published.
fn published_resource(
    resources: &[tilo_actions::ResourceSpec],
    id: &str,
) -> Option<tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            published_resource(&resource.children, id)
        }
    })
}

/// The views `wanted` offers in `published`, a resource of `app_id`, that
/// `permissions` let the caller open.
fn views_of(
    published: tilo_actions::ResourceSpec,
    app_id: &str,
    wanted: &tilo_actions::remote::ViewResourceQuery,
    permissions: &PermissionContext,
) -> Vec<ActionDescriptor> {
    let Some(permitted) = permissions.permitted_resource(app_id, published) else {
        return Vec::new();
    };
    tilo_actions::api::survey(&[permitted])
        .into_iter()
        .filter(|descriptor| {
            descriptor.address.resource.as_str() == wanted.id
                && descriptor.resource_kind == wanted.kind
                && descriptor.kind == tilo_actions::OperationKind::View
                && permissions.check(&ui::permissions::PermissionScope::Action {
                    node: tilo_actions::action_node(
                        &ui::permissions::TreePath::root(),
                        app_id,
                        &descriptor.resource_kind,
                        &descriptor.address.resource,
                        descriptor.contract.field.as_deref(),
                        &descriptor.address.action,
                    ),
                    kind: descriptor.contract.need().kind,
                })
        })
        .collect()
}

/// Who a replica acts as and what it may do in `workspace`: an agent with the
/// operator preset under its person, an owner, a bot acting for whoever
/// installed it, or a member with their access.
fn replica_access(
    actor: ReplicaActor,
    workspace: &str,
    member: &MemberId,
) -> (tilo_actions::ActionInvoker, PermissionContext) {
    let mut invoker = tilo_actions::ActionInvoker::member(member.clone());
    let permissions = match actor {
        ReplicaActor::Agent { person, name } => {
            let id = member.id();
            // The delegation its person handed it is its acting's own, and the agent is who the
            // changes are by, for the person it acts for, under the name its delegation gives it:
            // its changes name that acting, so every receiver decides them as this replica does.
            let acting = policy::Acting::handed_agent(id, person.id());
            invoker = tilo_actions::ActionInvoker::admitted(&acting)
                .named(Some(name).filter(|name| !name.trim().is_empty()));
            let permissions = PermissionContext::member_of(workspace, &person, Role::Editor);
            match permissions.access().clone().as_acting(acting) {
                Some(access) => permissions.with_access(access),
                None => permissions.read_only(),
            }
        }
        ReplicaActor::Owner => PermissionContext::member_of(workspace, member, Role::Owner),
        ReplicaActor::Bot {
            installer, access, ..
        } => {
            invoker = invoker.acting_for(installer);
            PermissionContext::from_access(access)
        }
        ReplicaActor::Member {
            access,
            sandbox,
            unix_time,
            agent_name,
            device,
        } => {
            if access.acting.is_agent() {
                // The agent's changes name it as it was admitted (`Caller::acting`): for the
                // person, in its session, with the delegation its person handed it. So History
                // reads the agent, and every receiver decides its changes as this access does.
                invoker = tilo_actions::ActionInvoker::admitted(&access.acting).named(agent_name);
            }
            // Where the work was made, when the caller named this device: a server taking
            // a member's work under the delegation that admits it.
            invoker.device = device;
            member_permissions(access, sandbox, unix_time)
        }
    };
    (invoker, permissions)
}

/// A member's permissions on `access`: the host bounded the member access as it
/// reads the caller (`Caller::invoker`), and puts an ask to the person on
/// their own devices when their agent acts.
fn member_permissions(
    access: policy::MemberAccess,
    sandbox: Vec<collab_types::caps::CapOverride>,
    unix_time: u64,
) -> PermissionContext {
    let person = access.person_id().unwrap_or_default().to_owned();
    PermissionContext::new(ui::permissions::Acting::person(person), sandbox, access)
        .at_unix_time(unix_time)
        .reaching_person()
}
