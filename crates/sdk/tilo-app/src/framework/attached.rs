//! An app's view of one record, attached and never drawn: how a host
//! without a screen — an agent host, a relay, a workspace service — performs
//! actions.
//!
//! Nothing is drawn if no one is looking, but no action is performed out of
//! sight. An [`AttachedView`] mounts the app's real component over the
//! record exactly as a window does, lays it out and never paints it; every
//! operation goes through the view's bound dispatcher, the API a person's
//! click takes, admitted for the participant as invoker and checked against
//! its permissions. Where the action landed is where the participant's
//! presence shows ([`AttachedView::landed`]), and the person's devices draw
//! the same record from synced state when someone looks.
//!
//! Sync is the caller's: [`AttachedView::take_changes`] returns this
//! participant's own changes since the last call (the whole document the
//! first time, for a record it created) and [`AttachedView::merge`] takes a
//! peer's, through the record store's replication frontier as a browser
//! page's mounts do. The parts of a chain the app keeps beside the record
//! ([`Chain`](super::Chain)) sync the same way on routes of their own
//! ([`AttachedView::take_record_changes`], [`AttachedView::merge_record`]),
//! and the app's actions reach them through the chain the view publishes.

use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

use tilo_actions::api::{ActionAddress, ActionDescriptor, ApiError};
use tilo_actions::{ActionEnvelope, ActionOutput, ActionTransport};
use ui_core::application::{
    ApplicationDefinition, ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget,
    RestoreInput,
};
use ui_core::application_view::InstanceViewStore;
use ui_core::collaboration::DocumentMutation;
use ui_core::headless::FrameHarness;
use ui_core::permissions::{MemberId, PermissionContext, SharedPermissions};
use ui_core::portable_storage::{PortableStorage, StorageKey};
use ui_core::record::{RecordCheckout, RecordOwners, RecordStore, RecordSync};

/// The component instance a view mounts.
const INSTANCE: u64 = 1;

/// Where the view lays out. Never drawn; it only gives the view a size, as
/// a window would.
const VIEWPORT: (u32, u32) = (1280, 720);

/// Device name the view's storage is kept under.
const DEVICE: &str = "attached-view";

/// The actions the view applied, as its transport replicates them.
#[derive(Clone, Default)]
struct Landed(Rc<RefCell<Vec<ActionEnvelope>>>);

impl ActionTransport for Landed {
    fn replicate(&mut self, env: &ActionEnvelope) {
        self.0.borrow_mut().push(env.clone());
    }
}

/// The tree an [`AttachedView`] published, and the state it was published
/// in: kept until that state moves.
struct Published {
    at: Standing,
    /// The tree as the view's frame publishes it.
    tree: Rc<Vec<tilo_actions::ResourceSpec>>,
    /// The tree as the participant may reach it, made from `tree` the first
    /// time it is asked for.
    permitted: Option<Rc<Vec<tilo_actions::ResourceSpec>>>,
}

/// What a published tree is read from: what the view was asked to do, the
/// record it holds, the input and models the toolkit was handed, and the
/// permissions in force. A tree published at one standing is the tree at an
/// equal one.
#[derive(PartialEq, Eq)]
struct Standing {
    moves: u64,
    heads: Vec<ui_core::automerge::ChangeHash>,
    input: u64,
    model: u64,
    permissions: u64,
}

/// One record held in its app's view, attached and never drawn. See the
/// module docs.
pub struct AttachedView {
    definition: &'static ApplicationDefinition,
    checkout: RecordCheckout,
    /// The app's view of the record: mounted, laid out, never drawn. Laid
    /// out when it is next read after a change ([`Self::frame`]).
    frame: RefCell<Box<FrameHarness>>,
    /// The view has not laid out what was merged since its last frame: a
    /// host merges a write's echo and each peer's change before it reads
    /// anything, and one frame then shows them all.
    unsettled: Cell<bool>,
    /// What the view published as last asked, while nothing it reads moved
    /// ([`Published`]): one call resolves a path, finds its chains and checks
    /// what it offers against the same tree, built once.
    published: RefCell<Option<Published>>,
    /// Counts what the view was asked to do that may change what it
    /// publishes: a frame, an invocation, a merge, a new standing.
    moves: Cell<u64>,
    /// The record store the view reads and writes.
    records: RecordStore,
    permissions: SharedPermissions,
    /// Who acts through the view: the invoker its operations are admitted for.
    me: String,
    /// The invoker as admitted: its kind and the person it acts for.
    invoker: tilo_actions::ActionInvoker,
    /// The task the invoker is carrying out, recorded with what it does.
    context: Option<tilo_actions::ActionContext>,
    landed: Landed,
    /// What this participant sent of the record, and its own changes set
    /// aside for the next publication.
    sync: RecordSync,
    /// Each record the view holds beside its own (the parts of a chain), by
    /// id: what was taken or merged of it so far.
    beside: std::collections::HashMap<ui_core::record::RecordId, RecordSync>,
    /// Where the view keeps its records, and the workspace: what a record
    /// beside its own that a peer started is kept in before it opens.
    cache: PortableStorage,
    workspace: String,
}

/// An [`AttachedView`] about to open ([`AttachedView::opening`]): whom it
/// is for, what they may do, and what the host lends the view.
pub struct OpenView<'a> {
    definition: &'static ApplicationDefinition,
    workspace: &'a str,
    checkout: RecordCheckout,
    document: Vec<u8>,
    invoker: tilo_actions::ActionInvoker,
    permissions: PermissionContext,
    created: bool,
    content: Option<tilo_content::ContentStore>,
}

impl OpenView<'_> {
    /// Open it for `invoker` (by default the record's owner).
    #[must_use]
    pub fn for_invoker(mut self, invoker: &tilo_actions::ActionInvoker) -> Self {
        self.invoker = invoker.clone();
        self
    }

    /// What the invoker may do through it (by default anything).
    #[must_use]
    pub fn with_permissions(mut self, permissions: PermissionContext) -> Self {
        self.permissions = permissions;
        self
    }

    /// Whether the invoker just created the record, so the first
    /// [`AttachedView::take_changes`] sends it whole.
    #[must_use]
    pub const fn created(mut self, created: bool) -> Self {
        self.created = created;
        self
    }

    /// Lend the view the workspace's content store, as a host lends a window
    /// its: the bytes a content hash names (an image another app made).
    #[must_use]
    pub fn lending(mut self, store: tilo_content::ContentStore) -> Self {
        self.content = Some(store);
        self
    }

    /// Mount the view.
    ///
    /// # Errors
    /// As [`AttachedView::open`].
    pub fn open(self) -> Result<AttachedView, String> {
        AttachedView::open_view(self)
    }
}

impl AttachedView {
    /// Mount `definition`'s view over the record `document` holds in
    /// `workspace`, for `me` with `permissions`. `created` when `me` just created
    /// the record, so the first [`Self::take_changes`] sends it whole.
    pub fn open(
        definition: &'static ApplicationDefinition,
        workspace: &str,
        checkout: RecordCheckout,
        document: Vec<u8>,
        me: &str,
        permissions: PermissionContext,
        created: bool,
    ) -> Result<Self, String> {
        Self::open_for(
            definition,
            workspace,
            checkout,
            document,
            &tilo_actions::ActionInvoker::person(me),
            permissions,
            created,
        )
    }

    /// [`Self::open`] for `invoker` — an agent, say, which may act for a
    /// person — rather than a person by their id alone. Its changes are
    /// written under the record writer's actor for its handle on this host.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn open_for(
        definition: &'static ApplicationDefinition,
        workspace: &str,
        checkout: RecordCheckout,
        document: Vec<u8>,
        invoker: &tilo_actions::ActionInvoker,
        permissions: PermissionContext,
        created: bool,
    ) -> Result<Self, String> {
        Self::opening(definition, workspace, checkout, document)
            .for_invoker(invoker)
            .with_permissions(permissions)
            .created(created)
            .open()
    }

    /// The view of `definition` over the record `document` holds in `workspace`,
    /// to open once said for whom, with what and what the host lends it
    /// ([`OpenView`]).
    #[must_use]
    pub fn opening<'a>(
        definition: &'static ApplicationDefinition,
        workspace: &'a str,
        checkout: RecordCheckout,
        document: Vec<u8>,
    ) -> OpenView<'a> {
        OpenView {
            definition,
            workspace,
            checkout,
            document,
            invoker: tilo_actions::ActionInvoker::person("owner"),
            permissions: PermissionContext::permissive(),
            created: false,
            content: None,
        }
    }

    fn open_view(view: OpenView<'_>) -> Result<Self, String> {
        let OpenView {
            definition,
            workspace,
            checkout,
            document,
            invoker,
            permissions,
            created,
            content,
        } = view;
        let invoker = &invoker;
        let me = &*invoker.acting.by.key();
        let joined_at = (!created).then(|| heads_of(&document)).transpose()?;
        let cache = PortableStorage::new("attached-views", DEVICE)?;
        let key = if checkout.id.is_package_primary() {
            StorageKey::Primary {
                package: definition.app_id().into(),
                workspace: workspace.to_owned(),
            }
        } else {
            StorageKey::Record {
                package: definition.app_id().into(),
                workspace: workspace.to_owned(),
                record: checkout.id.as_str().into(),
            }
        };
        // The record is written by the page that opens it, which mints its
        // own writer then; the handle writes what it does through actions.
        cache.hydrate(key, document, None)?;
        let cache_kept = cache.clone();
        let records = RecordOwners::portable(cache.clone())
            .open(definition.app_id(), workspace)
            .map_err(|error| error.to_string())?;
        let views = Rc::new(
            InstanceViewStore::portable(cache, workspace).map_err(|error| error.to_string())?,
        );
        let permissions = SharedPermissions::new(permissions);
        let target = LaunchTarget::Record {
            checkout: checkout.clone(),
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records: records.clone(),
                views,
                device_id: DEVICE.into(),
                instance_id: INSTANCE,
                permissions: permissions.clone(),
                author: MemberId::person(me.to_owned()),
                action_context: tilo_actions::ActionContext::default()
                    .with_invoker(invoker.clone()),
            },
            &target,
        )
        .map_err(|error| error.to_string())?;
        let mut context = context;
        if let Some(store) = content {
            context.provide_content(store);
        }
        let component = definition
            .instantiate(context, target, RestoreInput::None)
            .map_err(|error| error.to_string())?;
        let mut frame = FrameHarness::from_component(
            component,
            VIEWPORT,
            ComponentRunnerInputs {
                theme: ui_core::Theme::default(),
                text_renderer: ui_core::TextRenderer::new(),
            },
        )
        .map_err(|error| error.to_string())?;
        let landed = Landed::default();
        frame.set_action_transport(Box::new(landed.clone()))?;
        if let Some(joined_at) = &joined_at {
            refuse_written_on_open(definition, &records, &checkout, joined_at)?;
        }
        // A record joined starts where it was handed over: the only write
        // its view may make on open is a storage migration, which is sent
        // with the participant's first changes, as a window's is, so the
        // workspace and its other holders take it rather than diverge.
        let started = joined_at.or_else(|| {
            records
                .with(&checkout.id, ui_core::automerge::AutoCommit::get_heads)
                .filter(|_| !created)
        });
        let sync = started.map_or_else(RecordSync::new, RecordSync::delivered);
        Ok(Self {
            definition,
            checkout,
            frame: RefCell::new(Box::new(frame)),
            unsettled: Cell::new(false),
            published: RefCell::new(None),
            moves: Cell::new(0),
            records,
            permissions,
            me: me.to_owned(),
            invoker: invoker.clone(),
            context: None,
            landed,
            sync,
            beside: std::collections::HashMap::new(),
            cache: cache_kept,
            workspace: workspace.to_owned(),
        })
    }

    /// The app whose view this is.
    #[must_use]
    pub const fn definition(&self) -> &'static ApplicationDefinition {
        self.definition
    }

    #[must_use]
    pub const fn checkout(&self) -> &RecordCheckout {
        &self.checkout
    }

    /// Who acts through the view.
    #[must_use]
    pub fn me(&self) -> &str {
        &self.me
    }

    /// The participant's permissions changed: a role granted or taken away.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        self.permissions.set(permissions);
        self.unsettle();
    }

    /// The view's frame, laid out first when a change it has not shown was
    /// merged since its last frame. A host merges every change a record
    /// takes, its own write's echo among them, and reads the view far less
    /// often: the view lays out once for all of them, when it is read.
    fn frame(&self) -> Ref<'_, FrameHarness> {
        if self.unsettled.replace(false) {
            self.moved();
            self.frame.borrow_mut().relayout();
        }
        Ref::map(self.frame.borrow(), |frame| &**frame)
    }

    /// [`Self::frame`], to act through: what it does may change anything the
    /// view publishes.
    fn frame_mut(&mut self) -> &mut FrameHarness {
        self.moved();
        let frame = self.frame.get_mut();
        if self.unsettled.replace(false) {
            frame.relayout();
        }
        frame
    }

    /// Lay out what was merged since the view's last frame, if anything: what
    /// the records hold and ask for after a merge is what the view makes of
    /// it, before anything of them is read.
    fn settle(&self) {
        drop(self.frame());
    }

    /// The view holds what it has not laid out yet: its next read lays it out.
    fn unsettle(&self) {
        self.moved();
        self.unsettled.set(true);
    }

    fn moved(&self) {
        self.moves.set(self.moves.get().wrapping_add(1));
    }

    /// Where the view stands now ([`Standing`]).
    fn standing(&self) -> Standing {
        Standing {
            moves: self.moves.get(),
            heads: self
                .records
                .with(&self.checkout.id, ui_core::automerge::AutoCommit::get_heads)
                .unwrap_or_default(),
            input: ui_core::input_generation::generation(),
            model: ui_core::input_generation::model(),
            permissions: ui_core::permissions::shared_revision(),
        }
    }

    /// The tree the view's frame publishes now: built once for every read
    /// until the view, its record or its permissions move, however often a
    /// call asks for it (a path resolved a level at a time, the chains the
    /// view keeps, what an action is offered as).
    fn frame_tree(&self) -> Rc<Vec<tilo_actions::ResourceSpec>> {
        let frame = self.frame();
        if let Some(published) = self.published.borrow().as_ref()
            && published.at == self.standing()
        {
            return Rc::clone(&published.tree);
        }
        let tree = Rc::new(frame.resources());
        drop(frame);
        // Read after building: what building took in (a worker's reply that
        // had landed) is part of the standing the tree was published at.
        *self.published.borrow_mut() = Some(Published {
            at: self.standing(),
            tree: Rc::clone(&tree),
            permitted: None,
        });
        tree
    }

    /// The tree the view publishes, as the participant may reach it, shared:
    /// [`Self::resources`] without a copy of it.
    #[must_use]
    pub fn published(&self) -> Rc<Vec<tilo_actions::ResourceSpec>> {
        let tree = self.frame_tree();
        if let Some(permitted) = self
            .published
            .borrow()
            .as_ref()
            .and_then(|published| published.permitted.clone())
        {
            return permitted;
        }
        let permitted = {
            let permissions = self.permissions.borrow();
            Rc::new(
                tree.iter()
                    .cloned()
                    .filter_map(|resource| {
                        permissions.permitted_resource_on(
                            self.definition.app_id(),
                            self.checkout.id.as_str(),
                            resource,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        if let Some(published) = self.published.borrow_mut().as_mut() {
            published.permitted = Some(Rc::clone(&permitted));
        }
        permitted
    }

    /// The tree the view publishes, as the participant may reach it.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.published().as_ref().clone()
    }

    /// The items the view's tree leaves out past each collection's published
    /// page, as the participant may reach them: children of the record's
    /// resource.
    #[must_use]
    pub fn resources_past_page(&self) -> Vec<tilo_actions::ResourceSpec> {
        let permissions = self.permissions.borrow();
        self.frame()
            .resources_past_page()
            .into_iter()
            .filter_map(|resource| {
                permissions.permitted_resource_on(
                    self.definition.app_id(),
                    self.checkout.id.as_str(),
                    resource,
                )
            })
            .collect()
    }

    /// How many items the view's tree leaves out past each collection's
    /// published page: counted without building them.
    #[must_use]
    pub fn past_page_len(&self) -> usize {
        self.frame().past_page_len()
    }

    /// The items the view's tree leaves out past each collection's published
    /// page whose label is `name`, built alone and only if the participant
    /// may reach them: what a path naming one of them costs, however many
    /// items the record holds.
    #[must_use]
    pub fn past_page_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        let permissions = self.permissions.borrow();
        let frame = self.frame();
        frame
            .past_page_labels()
            .into_iter()
            .filter(|(_, label)| label.eq_ignore_ascii_case(name))
            .filter_map(|(id, _)| frame.resource(&id))
            .filter_map(|resource| {
                permissions.permitted_resource_on(
                    self.definition.app_id(),
                    self.checkout.id.as_str(),
                    resource,
                )
            })
            .collect()
    }

    /// The items the view's tree leaves out past each collection's published
    /// page from `offset` on, built alone as [`Self::resources_past_page`]
    /// builds them, and only those the participant may reach: what a page
    /// costs.
    #[must_use]
    pub fn past_page_range(&self, offset: usize, take: usize) -> Vec<tilo_actions::ResourceSpec> {
        let permissions = self.permissions.borrow();
        self.frame()
            .past_page_range(offset, take)
            .into_iter()
            .filter_map(|resource| {
                permissions.permitted_resource_on(
                    self.definition.app_id(),
                    self.checkout.id.as_str(),
                    resource,
                )
            })
            .collect()
    }

    /// The resource `id` the view publishes for this participant, resolved
    /// without building every other one: what an invocation on an address
    /// already known asks for.
    #[must_use]
    pub fn resource(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        let resource = self.frame().resource(id)?;
        let permissions = self.permissions.borrow();
        permissions.permitted_resource_on(
            self.definition.app_id(),
            self.checkout.id.as_str(),
            resource,
        )
    }

    /// The resource `id` with its own actions, as the participant may reach
    /// them, and none of its children: what an address on it is answered
    /// from, which costs that resource and not the items it holds.
    fn resource_alone(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        let mut resource = self.frame().resource(id)?;
        resource.children.clear();
        let permissions = self.permissions.borrow();
        permissions.permitted_resource_on(
            self.definition.app_id(),
            self.checkout.id.as_str(),
            resource,
        )
    }

    /// The contract of the action `address` names, as the view publishes it
    /// now: `None` when the view publishes no such action.
    #[must_use]
    pub fn descriptor(&self, address: &ActionAddress) -> Option<ActionDescriptor> {
        if let Some(alone) = self.resource_alone(&address.resource)
            && alone
                .actions
                .iter()
                .any(|action| action.id == address.action)
        {
            // The one resource's own actions, never its children's: an
            // action elsewhere in the tree is not this address's.
            if let Some(descriptor) = tilo_actions::api::survey(&[alone])
                .into_iter()
                .find(|descriptor| descriptor.address == *address)
            {
                return Some(descriptor);
            }
        }
        // A resource only the whole tree names (an accessibility or search
        // surface) is answered as it always was.
        tilo_actions::api::survey(&self.published())
            .into_iter()
            .find(|descriptor| descriptor.address == *address)
    }

    /// Every document operation the participant may perform through the
    /// view now, with its typed parameters.
    #[must_use]
    pub fn operations(&self) -> Vec<ActionDescriptor> {
        tilo_actions::api::survey(&self.published())
            .into_iter()
            .filter(|descriptor| descriptor.kind == tilo_actions::OperationKind::Document)
            .collect()
    }

    /// Perform the operation `address` names through the view, for the
    /// participant: permission, confirmation and strict argument binding
    /// exactly as for a person's click (`confirmed` when the person it acts
    /// for confirmed it). The change it made leaves with
    /// [`Self::take_changes`].
    pub fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
        self.invoke_appearing(address, arguments, confirmed, None)
    }

    /// [`Self::invoke`] showing `appearance`, which the API already admitted
    /// for this one invocation. `None` clears any appearance the view's
    /// context carried: only an API sets one.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_appearing(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
        appearance: Option<tilo_actions::Appearance>,
    ) -> Result<ActionOutput, ApiError> {
        let invoker = self.invoker_appearing(appearance);
        self.invoke_as(invoker, address, arguments, confirmed)
    }

    /// Whom [`Self::invoke_appearing`] admits an operation for: the
    /// participant, or the person the task says it acts for now, showing
    /// `appearance`.
    #[must_use]
    pub fn invoker_appearing(
        &self,
        appearance: Option<tilo_actions::Appearance>,
    ) -> tilo_actions::ActionInvoker {
        self.context
            .as_ref()
            .and_then(|context| context.invoker.clone())
            .filter(|invoker| invoker.acting.by == self.invoker.acting.by)
            .unwrap_or_else(|| self.invoker.clone())
            .with_appearance(appearance)
    }

    /// [`Self::invoke`] for `invoker`, whom the host admitted for this one
    /// invocation (an agent's, delivered to the app that owns the record):
    /// the operation and every edit it makes are theirs. Any operation the
    /// view offers — a document action, or an edit at the invoker's own
    /// caret.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_as(
        &mut self,
        invoker: tilo_actions::ActionInvoker,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
        let (_, started) = self.start_as(invoker, address, arguments, confirmed)?;
        // It answers once what it caused in the app has landed: a reply the
        // app asked a worker for in turn is applied as part of it, not of
        // whatever is invoked next. A job it started answers by itself.
        self.frame_mut().settle_owed();
        Ok(started.completed().unwrap_or_default())
    }

    /// [`Self::invoke_as`], waiting up to `within` for an operation that
    /// runs as a job (rendering a picture, exporting a file's worth) to end
    /// and answering with what it ended as, where [`Self::invoke_as`]
    /// answers a job at once with nothing: for a host that hands an
    /// effect's result back to whoever asked, an agent planning its next
    /// step on it. A job still running when `within` is up is cancelled.
    ///
    /// # Errors
    /// As [`Self::invoke_as`]; the job's failure or cancellation, or that it
    /// did not end in time.
    pub fn invoke_as_awaiting(
        &mut self,
        invoker: tilo_actions::ActionInvoker,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
        within: ui_core::time::Duration,
    ) -> Result<ActionOutput, ApiError> {
        let (invocation, started) = self.start_as(invoker, address, arguments, confirmed)?;
        if let Some(output) = started.completed() {
            self.frame_mut().settle_owed();
            return Ok(output);
        }
        let ended = |frame: &FrameHarness| {
            frame
                .job_updates(invocation)
                .iter()
                .find(|update| update.is_terminal())
                .cloned()
        };
        let frame = self.frame_mut();
        if frame.wait_until(within, |frame| ended(frame).is_some()) {
            // What the job's end caused is applied before it answers too.
            frame.settle_owed();
        }
        match ended(frame) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(output),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(ApiError::Failed(error.message))
            }
            Some(_) => Err(ApiError::Failed(format!(
                "{} was cancelled",
                address.action
            ))),
            None => {
                let _ = frame.cancel_job(invocation);
                Err(ApiError::Failed(format!(
                    "{} did not finish within {} seconds",
                    address.action,
                    within.as_secs()
                )))
            }
        }
    }

    /// Start the operation `address` names for `invoker`, as
    /// [`Self::invoke_as`] does: its invocation, and whether it finished.
    fn start_as(
        &mut self,
        invoker: tilo_actions::ActionInvoker,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<
        (
            tilo_actions::InvocationId,
            ui_core::host_operation::ResourceOperationStart,
        ),
        ApiError,
    > {
        if self.descriptor(address).is_none() {
            return Err(ApiError::Unknown(address.clone()));
        }
        self.landed.0.borrow_mut().clear();
        // History names the app the view is of, as its window does, within
        // whatever task the participant carries out.
        let ambient = self.context.clone().unwrap_or_default().enriched_by(
            &tilo_actions::ActionContext::default()
                .with_app(self.definition.app_id(), self.definition.title()),
        );
        self.frame_mut().start_bound_job_within(
            Some(invoker),
            Some(&ambient),
            &ui_core::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: address.clone(),
            },
            arguments,
            confirmed,
        )
    }

    /// Perform `invoke`, an action of the record's resource `R` built from
    /// its typed reference (`Note::EDIT.with(..)`, `Invoke::edit(..)`),
    /// through the view as [`Self::invoke`] does.
    ///
    /// # Errors
    /// As [`Self::invoke`], and when `invoke` does not bind or the view
    /// publishes no `R`.
    pub fn invoke_typed<R: tilo_resource::Resource>(
        &mut self,
        invoke: &tilo_resource::Invoke<R>,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(ApiError::InvalidArguments(refusal.to_owned()));
        }
        // The record this view draws is the one its actions are published
        // under: one of another app's, kept here, is reached through the
        // provider that keeps it.
        let app = self.definition.app_id();
        let resource = self
            .resources()
            .into_iter()
            .find(|resource| resource.kind == R::KIND && resource.app().is_none_or(|of| of == app))
            .ok_or_else(|| ApiError::Unknown(ActionAddress::new(R::KIND, invoke.id)))?;
        self.invoke(
            &ActionAddress::new(resource.id.as_str(), invoke.id),
            &invoke.arguments,
            confirmed,
        )
    }

    /// [`Self::invoke`] acting at `place` instead of at the participant's
    /// caret: `place` is given as the operation's place parameter
    /// (`#[param(place)]`), and the participant lands there.
    ///
    /// # Errors
    /// As [`Self::invoke`]; refused as invalid arguments when the operation
    /// takes no place.
    pub fn invoke_at(
        &mut self,
        place: &tilo_resource::Place,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
        let resources = self.resources();
        let param = tilo_actions::api::survey(&resources)
            .into_iter()
            .find(|entry| &entry.address == address)
            .ok_or_else(|| ApiError::Unknown(address.clone()))?
            .params
            .iter()
            .find(|param| param.is_place())
            .map(|param| param.name.to_string())
            .ok_or_else(|| {
                ApiError::InvalidArguments(format!("`{}` takes no place to act at", address.action))
            })?;
        let mut arguments = arguments.clone();
        arguments.insert(
            param,
            serde_json::to_value(place)
                .map_err(|error| ApiError::InvalidArguments(error.to_string()))?,
        );
        self.invoke(address, &arguments, confirmed)
    }

    /// The task the participant carries out from now on (`None`: none),
    /// recorded with each thing it does.
    pub fn set_context(&mut self, context: Option<tilo_actions::ActionContext>) {
        self.context = context;
    }

    /// The task the participant carries out, when it has one.
    #[must_use]
    pub const fn context(&self) -> Option<&tilo_actions::ActionContext> {
        self.context.as_ref()
    }

    /// Record one instruction that changed several records as one run in
    /// this record's history: a marker, no content. Leaves with
    /// [`Self::take_changes`]; whether anything was recorded.
    pub fn record_run(
        &mut self,
        label: &str,
        issued_by: Option<&str>,
        decision: Option<&str>,
        steps: &[ui_core::RunStep],
    ) -> bool {
        self.records
            .with(&self.checkout.id, |doc| {
                let by = doc.get_actor().to_hex_string();
                let recorded = ui_core::record_run_with_details(
                    doc,
                    label,
                    Some(&by),
                    issued_by,
                    Some(label),
                    decision,
                    steps,
                );
                if recorded.is_some() {
                    tilo_resource::history::commit_stamped_with_message(
                        doc,
                        Some(tilo_actions::FrameworkChange::Run.change_message()),
                    );
                }
                recorded.is_some()
            })
            .unwrap_or(false)
    }

    /// The last action the view applied for the participant's latest
    /// invocation: where its presence shows.
    #[must_use]
    pub fn landed(&self) -> Option<ActionEnvelope> {
        self.landed.0.borrow().last().cloned()
    }

    /// Read the record's document.
    pub fn read<R>(&self, read: impl FnOnce(&ui_core::automerge::AutoCommit) -> R) -> Option<R> {
        self.settle();
        self.records.read(&self.checkout.id, read)
    }

    /// The participant's changes since the last call, or `None` when it
    /// changed nothing. The first call on a record the participant created
    /// returns it whole.
    ///
    /// # Errors
    /// The record's changes could not be collected (it is not open here or
    /// its store is busy). Distinct from `Ok(None)`: what the participant
    /// changed is still in the record, unsent, and goes with the next call.
    pub fn take_changes(&mut self) -> Result<Option<DocumentMutation>, String> {
        // What the view writes as it lays a merged change out goes with
        // this participant's changes, as it did when it laid out on merge.
        self.settle();
        publish(&mut self.sync, &self.records, &self.checkout.id)
    }

    /// Run `during` while the view's records are held for reading, as a
    /// frame's read holds them: collecting this participant's changes, or
    /// setting them aside, finds the store busy. `None` when the record is
    /// not open. For tests of what a host answers then.
    #[cfg(any(test, feature = "test-support"))]
    pub fn while_records_busy<R>(&mut self, during: impl FnOnce(&mut Self) -> R) -> Option<R> {
        let records = self.records.clone();
        let id = self.checkout.id.clone();
        records.read(&id, |_| during(self))
    }

    /// Merge a peer's change into the view's record. Incremental changes
    /// that build on nothing this record holds belong to another record (or
    /// arrived ahead of what they build on) and are left alone: `Ok(false)`.
    pub fn merge(&mut self, mutation: &DocumentMutation) -> Result<bool, String> {
        if let DocumentMutation::Incremental(bytes) = mutation
            && !self
                .records
                .with(&self.checkout.id, |doc| RecordSync::builds_on(doc, bytes))
                .unwrap_or(false)
        {
            return Ok(false);
        }
        // What this participant did and has not taken stays its own: when
        // it cannot be set aside, the peer's change is not merged either.
        if self.sync.snapshotted() {
            self.sync
                .set_aside(&self.records, &self.checkout.id)
                .map_err(|error| set_aside_failed(&self.checkout.id, &error))?;
        }
        let changed = self
            .sync
            .merge(&self.records, &self.checkout, mutation.clone())?;
        // Laid out when the view is next read: a change the record already
        // held moved nothing to lay out.
        if changed {
            self.unsettle();
        }
        Ok(changed)
    }

    /// Take the view's records to come from its workspace's hosts, which may
    /// send a record only once it is asked for (a public workspace):
    /// [`RecordStore::ask_workspace_for_missing`]. A chain the view holds no
    /// part of asks for its head then, and writes to it wait for the answer.
    pub fn ask_workspace_for_missing(&self) {
        self.records.ask_workspace_for_missing();
    }

    /// The keys `mutation` writes in `record`, as the view holds that record
    /// now: a fork of it, so the answer costs a head set rather than the
    /// record. `None` for a record this view does not hold.
    pub fn changed_keys(
        &self,
        record: &ui_core::record::RecordId,
        mutation: &DocumentMutation,
    ) -> Option<Result<Vec<Vec<String>>, String>> {
        self.settle();
        self.records
            .with(record, |doc| changed_keys_of(doc, mutation))
    }

    /// What the view's records ask the workspace's hosts for
    /// ([`RecordStore::wanted_records`]).
    #[must_use]
    pub fn wanted_records(&self) -> Vec<ui_core::record::RecordId> {
        // What the records ask for is what the view's last frame asked.
        self.settle();
        self.records.wanted_records()
    }

    /// The workspace's hosts sent all they hold of `record`, none of it
    /// ([`RecordStore::note_workspace_answered`]); the view takes it up at its
    /// next frame.
    pub fn workspace_answered(
        &mut self,
        record: ui_core::record::RecordId,
        answer: ui_core::record::WorkspaceAnswer,
    ) {
        self.records.note_workspace_answer(record, answer);
        self.unsettle();
    }

    /// The chains of records the app keeps beside its own
    /// ([`Chain`](super::Chain)), as the view publishes them: each chain's
    /// id and the kind of its parts.
    #[must_use]
    pub fn chains(&self) -> Vec<RecordCheckout> {
        // Published beside the app's resource, among its children.
        self.frame_tree()
            .iter()
            .flat_map(|resource| std::iter::once(resource).chain(&resource.children))
            .filter_map(|resource| {
                let chain = super::chain_of_resource_id(resource.id.as_str())?;
                Some(RecordCheckout::new(
                    chain,
                    ui_core::record::RecordKind::new(resource.kind.clone()),
                ))
            })
            .collect()
    }

    /// `record` as the view holds it beside its own: a part of one of the
    /// app's chains ([`Self::chains`]), of that chain's kind. `None` for
    /// any other record.
    #[must_use]
    pub fn beside(&self, record: &ui_core::record::RecordId) -> Option<RecordCheckout> {
        let (chain, _) = record.chain_part()?;
        self.chains()
            .into_iter()
            .find(|held| held.id == chain)
            .map(|held| RecordCheckout::new(record.clone(), held.kind))
    }

    /// The records the view holds beside its own: the parts of the app's
    /// chains ([`Self::chains`]) open here, as they sync on routes of their
    /// own.
    #[must_use]
    pub fn records_beside(&self) -> Vec<RecordCheckout> {
        let chains = self.chains();
        self.records
            .open_records()
            .into_iter()
            .filter(|checkout| {
                checkout
                    .id
                    .chain_part()
                    .is_some_and(|(chain, _)| chains.iter().any(|held| held.id == chain))
            })
            .collect()
    }

    pub fn read_record<R>(
        &self,
        record: &ui_core::record::RecordId,
        read: impl FnOnce(&ui_core::automerge::AutoCommit) -> R,
    ) -> Option<R> {
        self.beside(record)?;
        self.settle();
        self.records.read(record, read)
    }

    /// The participant's changes to `record`, one the view holds beside
    /// its own ([`Self::records_beside`]), since the last call: the whole
    /// record the first time, for one the participant started.
    ///
    /// # Errors
    /// As [`Self::take_changes`].
    pub fn take_record_changes(
        &mut self,
        record: &ui_core::record::RecordId,
    ) -> Result<Option<DocumentMutation>, String> {
        self.settle();
        let sync = self.beside.entry(record.clone()).or_default();
        publish(sync, &self.records, record)
    }

    /// The participant's changes to every record the view holds beside its
    /// own ([`Self::take_record_changes`]), by record; all or none of them.
    ///
    /// # Errors
    /// As [`Self::take_changes`], for any one of them: those taken before it
    /// are kept to go with the next call, so a write is never half reported.
    pub fn take_records_beside_changes(
        &mut self,
    ) -> Result<Vec<(ui_core::record::RecordId, DocumentMutation)>, String> {
        let mut taken = Vec::new();
        for checkout in self.records_beside() {
            match self.take_record_changes(&checkout.id) {
                Ok(Some(mutation)) => taken.push((checkout.id, mutation)),
                Ok(None) => {}
                Err(error) => {
                    for (record, mutation) in taken {
                        if let Some(sync) = self.beside.get_mut(&record) {
                            sync.put_back(mutation);
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(taken)
    }

    /// Merge a peer's change into `record`, a part of one of the app's
    /// chains ([`Self::beside`]), opening it first when the view holds it
    /// not yet (a part another device started). Changes that build on
    /// nothing the part holds are left alone: `Ok(false)`. Whether it
    /// changed.
    pub fn merge_record(
        &mut self,
        record: &ui_core::record::RecordId,
        mutation: &DocumentMutation,
    ) -> Result<bool, String> {
        let checkout = self.beside(record).ok_or_else(|| {
            format!(
                "{} is not a part of a chain {} keeps",
                record.as_str(),
                self.definition.app_id()
            )
        })?;
        let opened = !self.records.is_ready(record);
        if opened && self.records.try_open_record(&checkout).is_err() {
            // Kept as the peer sent it, then opened as the view's own are.
            let DocumentMutation::Snapshot(bytes) = mutation else {
                return Ok(false);
            };
            self.cache.hydrate(
                StorageKey::Record {
                    package: self.definition.app_id().into(),
                    workspace: self.workspace.clone(),
                    record: record.as_str().into(),
                },
                bytes.clone(),
                None,
            )?;
            self.records
                .try_open_record(&checkout)
                .map_err(|error| error.to_string())?;
        }
        if let DocumentMutation::Incremental(bytes) = mutation
            && !self
                .records
                .with(record, |doc| RecordSync::builds_on(doc, bytes))
                .unwrap_or(false)
        {
            return Ok(false);
        }
        // What this participant did and has not taken stays its own. A part
        // first met through a peer's change is not this participant's to
        // publish whole: it starts from what the record already holds.
        let heads = self
            .records
            .with(record, ui_core::automerge::AutoCommit::get_heads)
            .unwrap_or_default();
        let sync = self
            .beside
            .entry(record.clone())
            .or_insert_with(move || RecordSync::delivered(heads));
        if sync.snapshotted() {
            sync.set_aside(&self.records, record)
                .map_err(|error| set_aside_failed(record, &error))?;
        }
        let changed = sync.merge(&self.records, &checkout, mutation.clone())?;
        // A part opened here is new to the view even when the change merged
        // into it is what it opened from.
        if changed || opened {
            self.unsettle();
        }
        Ok(changed)
    }
}

/// What to publish next of `record` as `sync` stands: the record whole the
/// first time, then this participant's own changes set aside since; `None`
/// when it changed nothing.
///
/// A failure to collect them is an error, never `None`: a host answers a
/// write by what this returns, and "nothing changed" would answer one that
/// was never saved as done.
fn publish(
    sync: &mut RecordSync,
    records: &RecordStore,
    id: &ui_core::record::RecordId,
) -> Result<Option<DocumentMutation>, String> {
    let failed = |error: String| {
        format!(
            "this participant's change to {} could not be collected: {error}",
            id.as_str()
        )
    };
    if !sync.snapshotted() {
        let Some(whole) = sync.take_outbound(records, id).map_err(failed)? else {
            return Ok(None);
        };
        sync.advance(whole.heads);
        return Ok(Some(whole.mutation));
    }
    sync.set_aside(records, id).map_err(failed)?;
    Ok(sync.take_set_aside())
}

/// Why a peer's change to `record` was not merged: this participant's own
/// changes to it could not be set aside first, so the record is left as it
/// was, its own changes still to go out, rather than merged around them.
fn set_aside_failed(record: &ui_core::record::RecordId, error: &str) -> String {
    format!(
        "a peer's change to {} was not merged: this participant's own changes to it \
         could not be set aside first ({error})",
        record.as_str()
    )
}

/// The heads of the record `document` holds: what a participant joining it
/// was given, past which anything its view writes while it opens is refused
/// ([`refuse_written_on_open`]).
/// The storage keys `mutation` writes, top down to what it put or deleted, in
/// `doc`: forked, so the document itself is not touched.
fn changed_keys_of(
    doc: &mut ui_core::automerge::AutoCommit,
    mutation: &DocumentMutation,
) -> Result<Vec<Vec<String>>, String> {
    use ui_core::automerge::PatchAction;
    let key = |prop: &ui_core::automerge::Prop| match prop {
        ui_core::automerge::Prop::Map(key) => key.clone(),
        ui_core::automerge::Prop::Seq(index) => index.to_string(),
    };
    let mut probe = doc.fork();
    // Indexed from here: the diff below reads the change alone, not every
    // operation the record holds.
    probe.update_diff_cursor();
    let was = probe.get_heads();
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    tilo_automerge::ApplyIncoming::load_incoming(&mut probe, bytes)
        .map_err(|error| error.to_string())?;
    let now = probe.get_heads();
    Ok(probe
        .diff(&was, &now)
        .into_iter()
        .map(|patch| {
            let mut keys: Vec<String> = patch.path.iter().map(|(_, prop)| key(prop)).collect();
            match &patch.action {
                PatchAction::PutMap { key: put, .. } | PatchAction::DeleteMap { key: put } => {
                    keys.push(put.clone());
                }
                PatchAction::Increment { prop, .. } | PatchAction::Conflict { prop } => {
                    keys.push(key(prop));
                }
                _ => {}
            }
            keys
        })
        .collect())
}

fn heads_of(document: &[u8]) -> Result<Vec<ui_core::automerge::ChangeHash>, String> {
    ui_core::automerge::AutoCommit::load(document)
        .map(|mut doc| doc.get_heads())
        .map_err(|error| error.to_string())
}

/// Refuse a view that wrote to the record `checkout` names, one its
/// participant joined at `joined_at`, as it opened ([`written_on_open`]).
fn refuse_written_on_open(
    definition: &ApplicationDefinition,
    records: &RecordStore,
    checkout: &RecordCheckout,
    joined_at: &[ui_core::automerge::ChangeHash],
) -> Result<(), String> {
    match records.with(&checkout.id, |doc| written_on_open(doc, joined_at)) {
        Some(Some(wrote)) => Err(format!(
            "{} wrote to a record it joined as it opened ({wrote}): only the record's \
             creator writes its initial content, and a participant that joins adds \
             nothing on open. What the record starts with belongs in the app's seed; \
             what a participant adds belongs behind a control they use",
            definition.app_id
        )),
        _ => Ok(()),
    }
}

/// What the app's view wrote to `doc` past `joined_at` while it opened, named
/// for the message that refuses it; `None` when it wrote nothing. A storage
/// migration, or the structure and identity every member derives the same
/// way, is the framework's and none of the app's.
///
/// Refused rather than kept: the participant's own changes are sent from
/// where it joined, so the view's write would be sent as if the participant
/// had made it, which no member did. Deferring it until
/// the record's content arrives would only move that fork later.
fn written_on_open(
    doc: &mut ui_core::automerge::AutoCommit,
    joined_at: &[ui_core::automerge::ChangeHash],
) -> Option<String> {
    use tilo_actions::{ActionContext, Did, FrameworkChange, HistoryOp};
    let wrote: Vec<String> = doc
        .get_changes(joined_at)
        .into_iter()
        .filter_map(|change| {
            let message = change.message()?;
            if message.starts_with(tilo_actions::CONTINUATION_PREFIX)
                || FrameworkChange::from_change_message(message) == Some(FrameworkChange::Native)
            {
                return Some("an edit".to_owned());
            }
            Some(match ActionContext::from_change_message(message)?.what? {
                Did::Verb(verb) => format!("`{}`", verb.action.action),
                Did::Edit(edit) => format!("an edit of `{}`", edit.field),
                Did::Group(_) => "a group of edits".to_owned(),
                Did::History(step) if matches!(step.op, HistoryOp::Migrate { .. }) => return None,
                Did::History(_) => "a history step".to_owned(),
            })
        })
        .collect();
    (!wrote.is_empty()).then(|| wrote.join(", "))
}
