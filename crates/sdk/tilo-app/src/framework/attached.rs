//! An app's view of one record, attached and never drawn: how a device
//! without a screen — an agent's, a server, a workspace service — performs
//! actions.
//!
//! Nothing is drawn if no one is looking, but no action is performed out of
//! sight. An [`AttachedView`] mounts the app's real component over the
//! record exactly as a window does, lays it out and never paints it; every
//! operation goes through the view's bound dispatcher, the API a person's
//! click takes, admitted for the member as invoker and checked against
//! its permissions. Where the action landed is where the member's
//! presence shows ([`AttachedView::landed`]), and the person's devices draw
//! the same record from synced state when someone looks.
//!
//! Sync is the caller's: [`AttachedView::take_changes`] returns this
//! member's own changes since the last call (the whole document the
//! first time, for a document it created) and [`AttachedView::merge`] takes a
//! peer's, through the record store's replication frontier as a browser
//! page's mounts do. The parts of a chain the app keeps beside the document
//! ([`Chain`](super::Chain)) sync the same way on routes of their own
//! ([`AttachedView::take_resource_changes`], [`AttachedView::merge_resource`]),
//! and the app's actions reach them through the chain the view publishes.

use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

use tilo_actions::api::{ActionAddress, ActionDescriptor, ApiError};
use tilo_actions::{ActionEnvelope, ActionOutput, ActionTransport};
use ui_core::application::{
    ApplicationDefinition, ComponentRunnerInputs, LaunchTarget, ProcessContextInputs, RestoreInput,
};
use ui_core::application_view::InstanceViewStore;
use ui_core::collaboration::DocumentMutation;
use ui_core::headless::FrameHarness;
use ui_core::permissions::{MemberId, PermissionContext, SharedPermissions};
use ui_core::portable_storage::{PortableStorage, StorageKey};
use ui_core::resource::{DocumentCheckout, DocumentSync, ResourceOwners, ResourceStore};

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

/// The items of a chain's level as its parts publish them
/// ([`AttachedView::chain_level`]), in the order a listing walks them: each
/// is made a resource only when a page reaches it
/// ([`AttachedView::chain_level_item`]).
#[derive(Clone)]
pub struct ChainLevel(ui_core::published::SharedResource);

impl ChainLevel {
    /// How many items the level holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.children().len()
    }

    /// The id of the item at `at`, read without building it.
    #[must_use]
    pub fn id(&self, at: usize) -> Option<&str> {
        self.0
            .children()
            .as_slice()
            .get(at)
            .map(ui_core::published::SharedResource::id)
    }

    /// Whether the level holds no item.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.children().is_empty()
    }

    /// Whether `other` is this level as it was published, nothing in it
    /// changed since: what a listing keeping its place from one page to the
    /// next compares.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

/// The tree an [`AttachedView`] published, and the state it was published
/// in: kept until that state moves.
struct Published {
    at: TreeInputs,
    /// The tree as the view's frame publishes it.
    tree: Rc<Vec<tilo_actions::ResourceSpec>>,
    /// The tree as the member may reach it, made from `tree` the first
    /// time it is asked for.
    permitted: Option<Rc<Vec<tilo_actions::ResourceSpec>>>,
}

/// What a published tree is read from: what the view was asked to do, the
/// record it holds, the input and models the toolkit was handed, and the
/// permissions in force. A tree published at one inputs is the tree at an
/// equal one.
#[derive(PartialEq, Eq)]
struct TreeInputs {
    moves: u64,
    heads: Vec<ui_core::automerge::ChangeHash>,
    input: u64,
    model: u64,
    permissions: u64,
}

/// What a view hands over of one record it holds beside its own.
pub struct BesideChange {
    /// The record that changed.
    pub document: ui_core::resource::DocumentId,
    /// This member's changes since the last take.
    pub changes: DocumentMutation,
    /// The record whole, when this member started it here: what a peer that
    /// holds none of it founds it from.
    pub whole: Option<Vec<u8>>,
}

impl BesideChange {
    /// What a peer that holds none of the record takes: the whole record when
    /// this member started it, the changes otherwise.
    #[must_use]
    pub fn mutation(&self) -> DocumentMutation {
        self.whole
            .clone()
            .map_or_else(|| self.changes.clone(), DocumentMutation::Snapshot)
    }
}

/// One record held in its app's view, attached and never drawn. See the
/// module docs.
pub struct AttachedView {
    definition: &'static ApplicationDefinition,
    checkout: DocumentCheckout,
    /// The app's view of the record: mounted, laid out, never drawn. Laid
    /// out when it is next read after a change ([`Self::frame`]).
    frame: RefCell<Box<FrameHarness>>,
    /// The view has not laid out what changed since its last frame: what was
    /// merged (a write's echo, each peer's change) and what the view's own
    /// actions did. One frame shows them all when the view is next read; an
    /// action on a resource the frame already finds does not wait for it.
    unsettled: Cell<bool>,
    /// What the view has not laid out includes what reached it from
    /// elsewhere and may move what it asks the workspace for (a peer's
    /// change to its own document, a part another device started, a new
    /// member's access, the workspace's answer): what it asks for is what
    /// a frame of that makes of it. A peer's change into a part the view
    /// already holds is not among them ([`Self::merge_resource`]).
    merged: Cell<bool>,
    /// What the view published as last asked, while nothing it reads moved
    /// ([`Published`]): one call resolves a path, finds its chains and checks
    /// what it offers against the same tree, built once.
    published: RefCell<Option<Published>>,
    /// Counts what the view was asked to do that may change what it
    /// publishes: a frame, an invocation, a merge, a new inputs.
    moves: Cell<u64>,
    /// The record store the view reads and writes.
    records: ResourceStore,
    permissions: SharedPermissions,
    /// Who acts through the view: the invoker its operations are admitted for.
    me: MemberId,
    /// The invoker as admitted: its kind and the person it acts for.
    invoker: tilo_actions::ActionInvoker,
    /// The task the invoker is carrying out, recorded with what it does.
    context: Option<tilo_actions::ActionContext>,
    landed: Landed,
    /// What this member sent of the record, and its own changes set
    /// aside for the next publication.
    sync: DocumentSync,
    /// Each document the view holds beside its own (the parts of a chain), by
    /// id: what was taken or merged of it so far.
    beside: std::collections::HashMap<ui_core::resource::DocumentId, DocumentSync>,
    /// Where the view keeps its records, and the workspace: what a record
    /// beside its own that a peer started is kept in before it opens.
    cache: PortableStorage,
    workspace: String,
    /// What the app keeps for its whole workspace beside its own document
    /// ([`collab_types::WORKSPACE_DOCUMENTS`]) that whoever holds this view has neither
    /// handed over ([`Self::merge_resource`]) nor said the workspace holds none of
    /// ([`Self::workspace_answered`]). The app's store opens for them only once this is
    /// empty: one founded here before that would give a document the workspace already
    /// holds a second beginning.
    awaited: Awaited,
    /// Whether the app keeps anything for its whole workspace.
    keeps_for_workspace: bool,
}

/// What an app keeps for its whole workspace that its view was not told of yet, by id.
type Awaited = Rc<RefCell<std::collections::BTreeSet<ui_core::resource::DocumentId>>>;

/// An [`AttachedView`] about to open ([`AttachedView::opening`]): whom it
/// is for, what they may do, and what the host lends the view.
pub struct OpenView<'a> {
    definition: &'static ApplicationDefinition,
    workspace: &'a str,
    checkout: DocumentCheckout,
    document: Vec<u8>,
    invoker: tilo_actions::ActionInvoker,
    permissions: PermissionContext,
    created: bool,
    content: Option<tilo_content::ContentStore>,
    /// The view's store asks its workspace for each document it opens and does not hold
    /// ([`ResourceStore::ask_workspace_for_missing`]): what whoever holds the view answers, as
    /// a page's workspace does for its windows.
    asks_workspace: bool,
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

    /// Have the view's store ask its workspace for the documents it opens and does not hold:
    /// one not here is then on its way rather than founded, until the workspace hands it over
    /// or answers it holds none of it
    /// ([`ResourceStore::ask_workspace_for_missing`]).
    #[must_use]
    pub fn asking_the_workspace(mut self) -> Self {
        self.asks_workspace = true;
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
        checkout: DocumentCheckout,
        document: Vec<u8>,
        me: &MemberId,
        permissions: PermissionContext,
        created: bool,
    ) -> Result<Self, String> {
        Self::open_for(
            definition,
            workspace,
            checkout,
            document,
            &tilo_actions::ActionInvoker::member(me.clone()),
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
        checkout: DocumentCheckout,
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
        checkout: DocumentCheckout,
        document: Vec<u8>,
    ) -> OpenView<'a> {
        OpenView {
            definition,
            workspace,
            checkout,
            document,
            invoker: tilo_actions::ActionInvoker::person("owner"),
            permissions: PermissionContext::owner(),
            created: false,
            content: None,
            asks_workspace: false,
        }
    }

    /// Give `context` what whoever holds the view lends it: the workspace's content, and its own
    /// `store` for the documents the view opens from its workspace, which for an app that keeps
    /// documents for its whole workspace ([`collab_types::WORKSPACE_DOCUMENTS`]) opens each
    /// only once the workspace answered for that one: until then it is on its way, which asks
    /// nothing and founds nothing. The rest of the app's own documents (the workspace's folder,
    /// which a Files window opens) are opened from it as a window does. With those still
    /// awaited, and whether the app keeps any. Another app's are not handed to this view at
    /// all.
    fn lent(
        context: &mut ui_core::application::ProcessContext,
        lends: Option<tilo_content::ContentStore>,
        definition: &'static ApplicationDefinition,
        store: &ResourceStore,
        me: &MemberId,
        asks_workspace: bool,
    ) -> (Awaited, bool) {
        if let Some(lends) = lends {
            context.provide_content(lends);
        }
        let awaited: Awaited = Rc::new(RefCell::new(
            collab_types::WORKSPACE_DOCUMENTS
                .iter()
                .filter(|kept| kept.app == definition.app_id())
                .filter_map(|kept| {
                    ui_core::resource::DocumentId::parse(&collab_types::workspace_document_id(
                        kept.kind,
                    ))
                })
                .collect(),
        ));
        let keeps = !awaited.borrow().is_empty();
        // A view whose documents come from its workspace is lent the store it keeps them in: an
        // app that keeps none for the workspace, and whose store asks its workspace for what
        // its window opens, still opens the workspace's own documents beside its own.
        if keeps || asks_workspace {
            let (asks, own, store) = (Rc::clone(&awaited), definition.app_id(), store.clone());
            context.provide_workspace_resources(
                ui_core::resource::workspace::WorkspaceResources::new(me.clone(), move |owner| {
                    if owner == own {
                        Ok(store.clone())
                    } else {
                        Err(ui_core::resource::ResourceOpenError::Arriving)
                    }
                })
                .awaiting(move |owner, id| {
                    // Each record opens on the workspace's word about it alone: one answered
                    // while another of the same app's is still on its way is already its own.
                    owner == own && asks.borrow().iter().any(|asked| asked.as_str() == id)
                }),
            );
        }
        (awaited, keeps)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one view opened: its store, its context and its first frame, in the order they \
                  depend on each other"
    )]
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
            asks_workspace,
        } = view;
        let invoker = &invoker;
        let me = member_of(invoker)?;
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
        let records = ResourceOwners::portable(cache.clone())
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
        let mut context = ui_core::application::ProcessContext::prepare(
            definition,
            ProcessContextInputs {
                records: records.clone(),
                views,
                device_id: DEVICE.into(),
                instance_id: INSTANCE,
                permissions: permissions.clone(),
                author: me.clone(),
                action_context: tilo_actions::ActionContext::default()
                    .with_invoker(invoker.clone()),
            },
            &target,
        )
        .map_err(|error| error.to_string())?;
        // Asked before the view's first frame opens anything, so a document the workspace
        // holds is on its way rather than founded here.
        if asks_workspace {
            records.ask_workspace_for_missing();
        }
        let (awaited, keeps_for_workspace) = Self::lent(
            &mut context,
            content,
            definition,
            &records,
            me,
            asks_workspace,
        );
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
        // with the member's first changes, as a window's is, so the
        // workspace and its other holders take it rather than diverge.
        let started = joined_at.or_else(|| {
            records
                .with(&checkout.id, ui_core::automerge::AutoCommit::get_heads)
                .filter(|_| !created)
        });
        let sync = started.map_or_else(DocumentSync::new, DocumentSync::delivered);
        Ok(Self {
            definition,
            checkout,
            frame: RefCell::new(Box::new(frame)),
            unsettled: Cell::new(false),
            merged: Cell::new(false),
            published: RefCell::new(None),
            moves: Cell::new(0),
            records,
            permissions,
            me: me.clone(),
            invoker: invoker.clone(),
            context: None,
            landed,
            sync,
            beside: std::collections::HashMap::new(),
            cache: cache_kept,
            workspace: workspace.to_owned(),
            awaited,
            keeps_for_workspace,
        })
    }

    /// The app whose view this is.
    #[must_use]
    pub const fn definition(&self) -> &'static ApplicationDefinition {
        self.definition
    }

    #[must_use]
    pub const fn checkout(&self) -> &DocumentCheckout {
        &self.checkout
    }

    /// Who acts through the view.
    #[must_use]
    pub fn me(&self) -> &MemberId {
        &self.me
    }

    /// The member's permissions changed: a role granted or taken away.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        self.permissions.set(permissions);
        self.unsettle_merged();
    }

    /// The view's frame, laid out first when a change it has not shown was
    /// merged or made since its last frame. A host merges every change a
    /// record takes, its own write's echo among them, acts through the view
    /// and reads it far less often: the view lays out once for all of them,
    /// when it is read. What that frame asks a worker for is taken up in it
    /// ([`FrameHarness::settle_owed`]): whoever reads the view reads all it
    /// makes of the record.
    fn frame(&self) -> Ref<'_, FrameHarness> {
        if self.unsettled.replace(false) {
            self.merged.set(false);
            self.moved();
            lay_out(&mut self.frame.borrow_mut());
        }
        Ref::map(self.frame.borrow(), |frame| &**frame)
    }

    /// The view's frame as it stands, without laying out what changed since
    /// its last one. The record's resource reads its document as it is now
    /// (an item a change made is there, one it removed is gone), so what an
    /// address names is found here at the cost of that resource, however
    /// much the view would lay out.
    fn frame_as_is(&self) -> Ref<'_, FrameHarness> {
        Ref::map(self.frame.borrow(), |frame| &**frame)
    }

    /// What `find` finds in the frame as it stands, or else in the frame laid
    /// out, when it has anything left to lay out: something only a frame
    /// shows (an item a surface publishes once it has synced) is found then.
    fn found<T>(&self, find: impl Fn(&FrameHarness) -> Option<T>) -> Option<T> {
        let found_now = find(&self.frame_as_is());
        if found_now.is_some() || !self.unsettled.get() {
            return found_now;
        }
        find(&self.frame())
    }

    /// [`Self::unsettle`] for what reached the view from elsewhere.
    fn unsettle_merged(&self) {
        self.merged.set(true);
        self.unsettle();
    }

    /// The view holds what it has not laid out yet: its next read lays it out.
    fn unsettle(&self) {
        self.moved();
        self.unsettled.set(true);
    }

    fn moved(&self) {
        self.moves.set(self.moves.get().wrapping_add(1));
    }

    /// Where the view stands now ([`TreeInputs`]).
    fn inputs(&self) -> TreeInputs {
        TreeInputs {
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
        self.tree_of(self.frame())
    }

    /// The tree `frame` publishes, kept as [`Self::frame_tree`] keeps it.
    fn tree_of(&self, frame: Ref<'_, FrameHarness>) -> Rc<Vec<tilo_actions::ResourceSpec>> {
        if let Some(published) = self.published.borrow().as_ref()
            && published.at == self.inputs()
        {
            return Rc::clone(&published.tree);
        }
        // Each build walks everything the view publishes: what a lookup that
        // costs one resource never does, as an observation of it says.
        let building = tilo_resource::profiling::Phase::start("framework.published_tree");
        let tree = Rc::new(frame.resources());
        drop(frame);
        building.end();
        // Read after building: what building took in (a worker's reply that
        // had landed) is part of what the tree was published at.
        *self.published.borrow_mut() = Some(Published {
            at: self.inputs(),
            tree: Rc::clone(&tree),
            permitted: None,
        });
        tree
    }

    /// The tree the view publishes, as the member may reach it, shared:
    /// [`Self::resources`] without a copy of it.
    #[must_use]
    pub fn published(&self) -> Rc<Vec<tilo_actions::ResourceSpec>> {
        self.permitted_tree(&self.frame_tree())
    }

    /// [`Self::published`] from the view as it stands, without laying out
    /// what changed since its last frame: what a lookup of one resource or
    /// action reads first, at the cost of the tree and not of a frame. The
    /// record's own resource reads its document as it is now; only what a
    /// frame of the view would add is not there yet ([`Self::laid_out`]).
    #[must_use]
    pub fn published_without_layout(&self) -> Rc<Vec<tilo_actions::ResourceSpec>> {
        self.permitted_tree(&self.tree_of(self.frame_as_is()))
    }

    /// Whether the view has laid out everything that changed: nothing it
    /// would publish waits for a frame.
    #[must_use]
    pub fn laid_out(&self) -> bool {
        !self.unsettled.get()
    }

    /// `tree` as the member may reach it, kept with it.
    fn permitted_tree(
        &self,
        tree: &[tilo_actions::ResourceSpec],
    ) -> Rc<Vec<tilo_actions::ResourceSpec>> {
        if let Some(permitted) = self
            .published
            .borrow()
            .as_ref()
            .and_then(|published| published.permitted.clone())
        {
            return permitted;
        }
        let permitted = Rc::new(
            tree.iter()
                .cloned()
                .filter_map(|resource| self.permitted(resource))
                .collect::<Vec<_>>(),
        );
        if let Some(published) = self.published.borrow_mut().as_mut() {
            published.permitted = Some(Rc::clone(&permitted));
        }
        permitted
    }

    /// The tree the view publishes, as the member may reach it.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.published().as_ref().clone()
    }

    /// The items the view's tree leaves out past each collection's published
    /// page, as the member may reach them: children of the record's
    /// resource.
    #[must_use]
    pub fn resources_past_page(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.frame()
            .resources_past_page()
            .into_iter()
            .filter_map(|resource| self.permitted(resource))
            .collect()
    }

    /// How many items the view's tree leaves out past each collection's
    /// published page: counted without building them.
    #[must_use]
    pub fn past_page_len(&self) -> usize {
        self.frame().past_page_len()
    }

    /// The items the view's tree leaves out past each collection's published
    /// page whose id or label is `name`, built alone and only if the member
    /// may reach them: what a path naming one of them costs, however many
    /// items the record holds.
    #[must_use]
    pub fn past_page_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        // Named alone from the view as it stands; laid out only when that
        // names none.
        self.found(|frame| {
            let past = frame.past_page_labels();
            // An id names the item itself, before any label does: its
            // resource id, or the item part of it (`<kind>/<field>/<item>`,
            // the id a path and an answer write).
            let by_id = |(id, _): &(String, String)| {
                id == name || id.rsplit_once('/').is_some_and(|(_, item)| item == name)
            };
            let listed: Vec<(String, String)> = {
                let ids: Vec<_> = past.iter().filter(|entry| by_id(entry)).cloned().collect();
                if ids.is_empty() {
                    past.iter()
                        .filter(|(_, label)| label.eq_ignore_ascii_case(name))
                        .cloned()
                        .collect()
                } else {
                    ids
                }
            };
            let named: Vec<_> = listed
                .into_iter()
                .filter_map(|(id, _)| frame.resource(&id))
                .filter_map(|resource| self.permitted(resource))
                .collect();
            (!named.is_empty()).then_some(named)
        })
        .unwrap_or_default()
    }

    /// The items `name` names among what the chain `parent` holds — by its
    /// id, whole or the item part of it, first, then by its label — each
    /// built alone from the part keeping it, and only those the member may
    /// reach: what a path naming one costs, however many items the chain
    /// holds, and whether or not the page it publishes carries them.
    #[must_use]
    pub fn chain_items_named(&self, parent: &str, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        let lookup = |frame: &mut FrameHarness| {
            frame
                .widget_mut::<super::root::ResourceRoot>()
                .map(|root| root.storage.chain_items_named(parent, name))
                .unwrap_or_default()
        };
        let mut found = lookup(&mut self.frame.borrow_mut());
        if found.is_empty() && self.unsettled.get() {
            // A chain only a view opens may hold it: lay the view out once.
            drop(self.frame());
            found = lookup(&mut self.frame.borrow_mut());
        }
        // Each is the item as the view publishes it, wherever it stands:
        // the frame resolves an item of a chain the same way
        // ([`super::root::ResourceRoot`]'s `resource_for`).
        found
            .into_iter()
            .filter_map(|resource| self.permitted(resource))
            .collect()
    }

    /// The level the chain `parent` keeps, as its parts publish it, without
    /// a frame of the view or the tree the view publishes: after a write, a
    /// page of the chain's listing builds the part that changed and the items
    /// it shows ([`Self::chain_level_item`]), not the whole chain, however
    /// many items it holds. `None` when no chain the view opened is
    /// `parent`, or another provider keeps it.
    #[must_use]
    pub fn chain_level(&self, parent: &str) -> Option<ChainLevel> {
        super::chain_of_resource_id(parent)?;
        let lookup = |frame: &mut FrameHarness| {
            frame
                .widget_mut::<super::root::ResourceRoot>()
                .and_then(|root| root.storage.chain_published(parent))
        };
        let mut found = lookup(&mut self.frame.borrow_mut());
        if found.is_none() && self.unsettled.get() {
            // A chain only a view opens: lay the view out once.
            drop(self.frame());
            found = lookup(&mut self.frame.borrow_mut());
        }
        found.map(ChainLevel)
    }

    /// The item at `at` of `level`, with what it holds, as the member may
    /// reach it: `None` past the level's end or when nothing of it is theirs.
    #[must_use]
    pub fn chain_level_item(
        &self,
        level: &ChainLevel,
        at: usize,
    ) -> Option<tilo_actions::ResourceSpec> {
        let item = level.0.children().as_slice().get(at)?.to_resource();
        self.permitted(item)
    }

    /// The chains the view opened that `name` names at the app's own level,
    /// by their id, kind or label, each as its head part publishes it, and
    /// only those the member may reach: what a path's first segment costs,
    /// however many items the chains hold, and without a frame of the view.
    #[must_use]
    pub fn chains_named_at_top(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        let found = {
            let mut frame = self.frame.borrow_mut();
            frame
                .widget_mut::<super::root::ResourceRoot>()
                .map(|root| root.storage.chains_named(name))
                .unwrap_or_default()
        };
        found
            .into_iter()
            .filter_map(|resource| self.permitted(resource))
            .collect()
    }

    /// Whether `id` is an item a chain the view opened keeps, found in the
    /// part keeping it without a frame of the view: what an action on it is
    /// offered as.
    #[must_use]
    pub fn in_chain(&self, id: &str) -> bool {
        self.frame
            .borrow_mut()
            .widget_mut::<super::root::ResourceRoot>()
            .is_some_and(|root| root.storage.chain_item(id).is_some())
    }

    /// [`Self::past_page_len`] of the items the level `parent` leaves out
    /// past the page its publication carries: the app's own collections, or,
    /// for a chain, the items past the page each of its parts publishes.
    #[must_use]
    pub fn past_page_len_under(&self, parent: &str) -> usize {
        if super::chain_of_resource_id(parent).is_none() {
            return self.past_page_len();
        }
        self.frame
            .borrow_mut()
            .widget_mut::<super::root::ResourceRoot>()
            .map_or(0, |root| root.storage.chain_past_page_len(parent))
    }

    /// Whether a page of the listing of the level `parent` reads what is not here yet, `filled`
    /// when what the view holds filled the page: a chain listed whose newest parts are not all
    /// in, the one the page reaches asked of the workspace's hosts
    /// ([`ResourceStore::wanted_resources`]). A page that reads none is what it will be once
    /// every part is in, and is answered now.
    #[must_use]
    pub fn listing_arriving(&self, parent: &str, filled: bool) -> bool {
        self.frame
            .borrow_mut()
            .widget_mut::<super::root::ResourceRoot>()
            .is_some_and(|root| root.storage.listing_arriving(parent, filled))
    }

    /// [`Self::past_page_range`] of the level `parent`: the app's own items,
    /// or a chain's items past the page each of its parts publishes, and only
    /// those the member may reach.
    #[must_use]
    pub fn past_page_range_under(
        &self,
        parent: &str,
        offset: usize,
        take: usize,
    ) -> Vec<tilo_actions::ResourceSpec> {
        if super::chain_of_resource_id(parent).is_none() {
            return self.past_page_range(offset, take);
        }
        let built = self
            .frame
            .borrow_mut()
            .widget_mut::<super::root::ResourceRoot>()
            .map(|root| root.storage.chain_past_page_window(parent, offset, take))
            .unwrap_or_default();
        built
            .into_iter()
            .filter_map(|resource| self.permitted(resource))
            .collect()
    }

    /// The items the view's tree leaves out past each collection's published
    /// page from `offset` on, built alone as [`Self::resources_past_page`]
    /// builds them, and only those the member may reach: what a page
    /// costs.
    #[must_use]
    pub fn past_page_range(&self, offset: usize, take: usize) -> Vec<tilo_actions::ResourceSpec> {
        self.frame()
            .past_page_range(offset, take)
            .into_iter()
            .filter_map(|resource| self.permitted(resource))
            .collect()
    }

    /// The resource `id` the view publishes for this member, resolved
    /// without building every other one: what an invocation on an address
    /// already known asks for.
    #[must_use]
    pub fn resource(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        self.found(|frame| self.permitted(frame.resource(id)?))
    }

    /// `resource` as the member may reach it.
    fn permitted(
        &self,
        resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        self.permissions
            .borrow()
            .shown_resource(self.definition.app_id(), resource)
    }

    /// The resource `id` with its own actions, as the member may reach
    /// them, and none of its children: what an address on it is answered
    /// from, which costs that resource and not the items it holds.
    fn resource_alone(&self, frame: &FrameHarness, id: &str) -> Option<tilo_actions::ResourceSpec> {
        let mut resource = frame.resource(id)?;
        resource.children.clear();
        self.permitted(resource)
    }

    /// The contract of the action `address` names, as the view publishes it
    /// now: `None` when the view publishes no such action. Answered from the
    /// one resource it names, built alone. An item's address
    /// (`<kind>/<field>/<item>`) is answered from that item alone: it costs
    /// the item however many there are; a frame the item only publishes once
    /// it is laid out lays that out; one a collection keeps past its page's
    /// items is built from that collection's labels; and one the view
    /// publishes in its tree is found there, as a listing finds it. None of
    /// them costs the tree while the item answers by itself.
    #[must_use]
    pub fn descriptor(&self, address: &ActionAddress) -> Option<ActionDescriptor> {
        // The one resource's own actions, never its children's: an action
        // elsewhere in the tree is not this address's.
        let own = |frame: &FrameHarness| {
            let alone = self.resource_alone(frame, &address.resource)?;
            if !alone
                .actions
                .iter()
                .any(|action| action.id == address.action)
            {
                return None;
            }
            tilo_actions::api::survey(&[alone])
                .into_iter()
                .find(|descriptor| descriptor.address == *address)
        };
        let in_tree = |resources: &[tilo_actions::ResourceSpec]| {
            tilo_actions::api::survey(resources)
                .into_iter()
                .find(|descriptor| descriptor.address == *address)
        };
        if tilo_resource::items::item_kind(address.resource.as_str()).is_some() {
            // An item a listing has just offered must be runnable: the frame
            // as it stands answers while it holds the item, and one a change
            // made since is published by a layout, as any other resource a
            // frame publishes only when it is laid out.
            if let Some(found) = self.found(own) {
                return Some(found);
            }
            // An item a collection keeps past the page its frame shows is
            // built alone, as the listing that named it built it.
            let frame = self.frame();
            let labels = frame.past_page_labels();
            if let Some(at) = labels.iter().position(|(id, _)| id == &address.resource)
                && let Some(mut child) = frame.past_page_range(at, 1).into_iter().next()
            {
                child.children.clear();
                if let Some(alone) = self.permitted(child)
                    && alone
                        .actions
                        .iter()
                        .any(|action| action.id == address.action)
                    && let Some(found) = tilo_actions::api::survey(&[alone])
                        .into_iter()
                        .find(|descriptor| descriptor.address == *address)
                {
                    return Some(found);
                }
            }
            drop(frame);
            // The view publishes the item in its tree although no one widget
            // hands it out by itself: the tree it publishes has it.
            let resources = self.published_without_layout();
            return in_tree(resources.as_ref());
        }
        // As the view stands: naming one operation costs the tree, not a frame of everything the
        // document shows. A resource only a frame publishes yet is found by laying it out, as
        // before: something a change just made, named before anyone looked.
        let as_is = own(&self.frame_as_is()).or_else(|| {
            let resources = self.published_without_layout();
            in_tree(resources.as_ref())
        });
        if as_is.is_some() || !self.unsettled.get() {
            return as_is;
        }
        self.found(own)
            .or_else(|| in_tree(self.published().as_ref()))
    }

    /// Every document operation the member may perform through the
    /// view now, with its typed parameters. What its permissions deny it is
    /// not among them: an agent is listed that marked not allowed
    /// ([`tilo_actions::Availability::not_allowed`]), so that it hears a
    /// refusal, and an offer names only what may be done.
    #[must_use]
    pub fn operations(&self) -> Vec<ActionDescriptor> {
        tilo_actions::api::survey(&self.published())
            .into_iter()
            .filter(|descriptor| {
                descriptor.kind == tilo_actions::OperationKind::Document
                    && descriptor.contract.availability.refusal().is_none()
            })
            .collect()
    }

    /// Perform the operation `address` names through the view, for the
    /// member: permission, confirmation and strict argument binding
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
    /// member, or the person the task says it acts for now, showing
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
        // It answers once what it submitted is applied to the document; what
        // the view makes of that is made when the view is next read. A job
        // it started answers by itself.
        self.settle_acted();
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
            self.settle_acted();
            return Ok(output);
        }
        // The job takes its input from the resource and its arguments: its
        // end is waited for without laying the view out, and what it caused
        // is laid out when someone reads the view.
        self.moved();
        let frame = &mut **self.frame.get_mut();
        let ended = frame.await_job(invocation, within);
        self.unsettle();
        match ended {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(output),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(ApiError::Failed(error.message))
            }
            Some(_) => Err(ApiError::Failed(format!(
                "{} was cancelled",
                address.action
            ))),
            None => {
                let _ = self.frame.get_mut().cancel_job(invocation);
                Err(ApiError::Failed(format!(
                    "{} did not finish within {} seconds",
                    address.action,
                    within.as_secs()
                )))
            }
        }
    }

    /// After an action: dispatch what it submitted, laying nothing out
    /// ([`FrameHarness::take_up_acted`]), and leave the view to lay out what
    /// it did when it is next read. An action never waits for a frame of
    /// everything the view shows; whoever looks lays that out.
    fn settle_acted(&mut self) {
        self.moved();
        self.frame.get_mut().take_up_acted();
        self.unsettle();
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
        // whatever task the member carries out.
        let ambient = self.context.clone().unwrap_or_default().enriched_by(
            &tilo_actions::ActionContext::default()
                .with_app(self.definition.app_id(), self.definition.title()),
        );
        // An operation acts on the resource as it is and on the arguments it
        // was given, through the frame as it stands: a document operation,
        // an effect and its job alike. A view operation moves what the view
        // shows, which whoever next reads the view lays out.
        self.moved();
        let frame = &mut **self.frame.get_mut();
        frame.start_bound_job_within(
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
        let ours = |resource: &tilo_actions::ResourceSpec| {
            resource.kind == R::KIND && resource.app().is_none_or(|of| of == app)
        };
        // The record's own resource is published under its kind: found
        // alone, as an address is, before the whole tree is read for it.
        let id = match self.resource(R::KIND).filter(ours) {
            Some(resource) => resource.id,
            None => {
                self.resources()
                    .into_iter()
                    .find(ours)
                    .ok_or_else(|| ApiError::Unknown(ActionAddress::new(R::KIND, invoke.id)))?
                    .id
            }
        };
        self.invoke(
            &ActionAddress::new(id.as_str(), invoke.id),
            &invoke.arguments,
            confirmed,
        )
    }

    /// [`Self::invoke`] acting at `place` instead of at the member's
    /// caret: `place` is given as the operation's place parameter
    /// (`#[param(place)]`), and the member lands there.
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

    /// The task the member carries out from now on (`None`: none),
    /// recorded with each thing it does.
    pub fn set_context(&mut self, context: Option<tilo_actions::ActionContext>) {
        self.context = context;
    }

    /// The task the member carries out, when it has one.
    #[must_use]
    pub const fn context(&self) -> Option<&tilo_actions::ActionContext> {
        self.context.as_ref()
    }

    /// Record one instruction that changed several records as one run in
    /// this record's history: a marker, no content. Leaves with
    /// [`Self::take_changes`]; whether anything was recorded.
    pub fn record_invocation(
        &mut self,
        label: &str,
        issued_by: Option<&str>,
        decision: Option<&str>,
        steps: &[ui_core::InvocationStep],
    ) -> bool {
        self.records
            .with(&self.checkout.id, |doc| {
                let by = doc.get_actor().to_hex_string();
                let recorded = ui_core::record_invocation_with_details(
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

    /// The last action the view applied for the member's latest
    /// invocation: where its presence shows.
    #[must_use]
    pub fn landed(&self) -> Option<ActionEnvelope> {
        self.landed.0.borrow().last().cloned()
    }

    /// Read the record's document, as it is: reading the record lays out
    /// nothing of the view.
    pub fn read<R>(&self, read: impl FnOnce(&ui_core::automerge::AutoCommit) -> R) -> Option<R> {
        self.records.read(&self.checkout.id, read)
    }

    /// The member's changes since the last call, or `None` when it
    /// changed nothing. The first call on a record the member created
    /// returns it whole. What the view writes as it lays a change out goes
    /// with the call after the frame that wrote it: taking a write lays out
    /// nothing.
    ///
    /// # Errors
    /// The record's changes could not be collected (it is not open here or
    /// its store is busy). Distinct from `Ok(None)`: what the member
    /// changed is still in the record, unsent, and goes with the next call.
    pub fn take_changes(&mut self) -> Result<Option<DocumentMutation>, String> {
        publish(&mut self.sync, &self.records, &self.checkout.id)
    }

    /// Run `during` while the view's records are held for reading, as a
    /// frame's read holds them: collecting this member's changes, or
    /// setting them aside, finds the store busy. `None` when the record is
    /// not open. For tests of what a host answers then.
    #[cfg(any(test, feature = "test-support"))]
    pub fn while_resources_busy<R>(&mut self, during: impl FnOnce(&mut Self) -> R) -> Option<R> {
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
                .with(&self.checkout.id, |doc| DocumentSync::builds_on(doc, bytes))
                .unwrap_or(false)
        {
            return Ok(false);
        }
        // What this member did and has not taken stays its own: when
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
            self.unsettle_merged();
        }
        Ok(changed)
    }

    /// Take the view's records to come from its workspace's hosts, which may
    /// send a record only once it is asked for (a public workspace):
    /// [`ResourceStore::ask_workspace_for_missing`]. A chain the view holds no
    /// part of asks for its head then, and writes to it wait for the answer.
    pub fn ask_workspace_for_missing(&self) {
        self.records.ask_workspace_for_missing();
    }

    /// What the view's records ask the workspace's hosts for
    /// ([`ResourceStore::wanted_resources`]). Each chain with a part here reads
    /// first: a chain follows what arrived only when it is read, so the part
    /// just handed to it is taken up, and a chain read whole asks for the
    /// parts before its head. Only the chains are read, not the view's tree.
    ///
    /// What the app keeps for its whole workspace that the view was told nothing of yet is
    /// wanted too: it opens only on the workspace's word ([`Self::workspace_answered`]), and
    /// the view waits for what its app declared of it, so a host that answers what the view
    /// wants answers that as well.
    #[must_use]
    pub fn wanted_resources(&self) -> Vec<ui_core::resource::DocumentId> {
        // What reached the view from elsewhere asks for what a frame of it
        // asks for (a chain a merged change starts); what the view's own
        // actions did asks for nothing a frame would add.
        if self.merged.get() {
            drop(self.frame());
        }
        let chains: std::collections::BTreeSet<ui_core::resource::DocumentId> = self
            .records
            .open_resources()
            .into_iter()
            .filter_map(|checkout| checkout.id.chain_part().map(|(chain, _)| chain))
            .collect();
        for chain in &chains {
            let _ = self
                .frame_as_is()
                .resource(&super::chain_resource_id(chain));
        }
        let mut wanted = self.records.wanted_resources();
        for kept in self.awaited.borrow().iter() {
            if !wanted.contains(kept) {
                wanted.push(kept.clone());
            }
        }
        wanted
    }

    /// The workspace's hosts sent all they hold of `record`, none of it
    /// ([`ResourceStore::note_workspace_answered`]); the view takes it up at its
    /// next frame.
    pub fn workspace_answered(
        &mut self,
        record: ui_core::resource::DocumentId,
        answer: ui_core::resource::WorkspaceAnswer,
    ) {
        self.awaited.borrow_mut().remove(&record);
        self.records.note_workspace_answer(record, answer);
        self.unsettle_merged();
        self.take_up_kept();
    }

    /// The chains of records the app keeps beside its own
    /// ([`Chain`](super::Chain)), as the view publishes them: each chain's
    /// id and the kind of its parts.
    #[must_use]
    pub fn chains(&self) -> Vec<DocumentCheckout> {
        // Published beside the app's resource, at the top of its tree.
        self.frame_tree()
            .iter()
            .filter_map(|resource| {
                let chain = super::chain_of_resource_id(resource.id.as_str())?;
                Some(DocumentCheckout::new(
                    chain,
                    ui_core::resource::ResourceType::new(resource.kind.clone()),
                ))
            })
            .collect()
    }

    /// `record` as the view holds it beside its own: a part of one of the
    /// app's chains ([`Self::chains`]), of that chain's kind, or what the app
    /// keeps for its whole workspace ([`Self::kept_for_workspace`]). `None`
    /// for any other document.
    #[must_use]
    pub fn beside(&self, record: &ui_core::resource::DocumentId) -> Option<DocumentCheckout> {
        if let Some(kept) = self.kept_for_workspace(record) {
            return Some(kept);
        }
        let (chain, _) = record.chain_part()?;
        let kind = self.chain_kind(&chain)?;
        Some(DocumentCheckout::new(record.clone(), kind))
    }

    /// The kind of the parts of `chain`, when the view publishes it: found
    /// by the chain's own resource, without the rest of the tree.
    fn chain_kind(
        &self,
        chain: &ui_core::resource::DocumentId,
    ) -> Option<ui_core::resource::ResourceType> {
        let id = super::chain_resource_id(chain);
        self.found(|frame| frame.resource(&id))
            .map(|resource| ui_core::resource::ResourceType::new(resource.kind))
    }

    /// `id` as what the app keeps for its whole workspace beside its own document
    /// ([`collab_types::WORKSPACE_DOCUMENTS`]), of that document's kind, when it is one of
    /// those: held here like a chain's parts, on a route of its own.
    fn kept_for_workspace(&self, id: &ui_core::resource::DocumentId) -> Option<DocumentCheckout> {
        if !self.keeps_for_workspace {
            return None;
        }
        collab_types::workspace_document(self.definition.app_id(), id.as_str()).map(|kept| {
            DocumentCheckout::new(id.clone(), ui_core::resource::ResourceType::new(kept.kind))
        })
    }

    /// Have the app open each document it asked its workspace for that the workspace has
    /// answered for. Its declaration asked for each once, when the view opened and they were
    /// still awaited, and a view no window shows is laid out and never refreshed, so nothing
    /// else asks again. A document opened here is new to the view, which lays it out when next
    /// read; laying the view out here would do that for every change merged into a part.
    fn take_up_kept(&mut self) {
        let opened = self
            .frame
            .get_mut()
            .widget_mut::<super::root::ResourceRoot>()
            .is_some_and(|root| root.storage.open_arrived());
        if opened {
            self.unsettle();
        }
    }

    /// The documents the view opened from its workspace beside its own — what the app keeps for
    /// its whole workspace ([`Self::kept_for_workspace`]) and the app's own documents a window
    /// opens from it (the workspace's folder a Files window shows) — that hold something of
    /// this member's to send: one a peer's change was merged into, or one founded here that
    /// holds more than its founding. One only founded is nobody's change yet, and sending it
    /// would be a write by a member who made none.
    fn kept_changed(&self) -> Vec<DocumentCheckout> {
        let founding = tilo_actions::FrameworkChange::Genesis.change_message();
        self.records
            .open_resources()
            .into_iter()
            .filter(|checkout| {
                checkout.id != self.checkout.id && checkout.id.chain_part().is_none()
            })
            .filter(|checkout| {
                self.beside.contains_key(&checkout.id)
                    || self
                        .records
                        .with(&checkout.id, |doc| {
                            doc.get_changes(&[])
                                .iter()
                                .any(|change| change.message() != Some(founding.as_str()))
                        })
                        .unwrap_or(false)
            })
            .collect()
    }

    /// The records the view holds beside its own: the parts of the app's
    /// chains ([`Self::chains`]) open here, as they sync on routes of their
    /// own.
    #[must_use]
    pub fn resources_beside(&self) -> Vec<DocumentCheckout> {
        let mut chains = std::collections::HashMap::new();
        self.records
            .open_resources()
            .into_iter()
            .filter(|checkout| {
                checkout.id.chain_part().is_some_and(|(chain, _)| {
                    *chains
                        .entry(chain)
                        .or_insert_with_key(|chain| self.chain_kind(chain).is_some())
                })
            })
            .collect()
    }

    pub fn read_resource<R>(
        &self,
        record: &ui_core::resource::DocumentId,
        read: impl FnOnce(&ui_core::automerge::AutoCommit) -> R,
    ) -> Option<R> {
        self.beside(record)?;
        self.records.read(record, read)
    }

    /// The member's changes to `record`, one the view holds beside
    /// its own ([`Self::resources_beside`]), since the last call: the changes
    /// it made, and, for a part the member started, the part whole, which a
    /// peer that holds none of it founds it from.
    ///
    /// # Errors
    /// As [`Self::take_changes`].
    pub fn take_resource_changes(
        &mut self,
        record: &ui_core::resource::DocumentId,
    ) -> Result<Option<BesideChange>, String> {
        let sync = self.beside.entry(record.clone()).or_default();
        publish_beside(sync, &self.records, record)
    }

    /// The member's changes to every document the view holds beside its
    /// own ([`Self::take_resource_changes`]), by record; all or none of them.
    ///
    /// # Errors
    /// As [`Self::take_changes`], for any one of them: those taken before it
    /// are kept to go with the next call, so a write is never half reported.
    pub fn take_resources_beside_changes(&mut self) -> Result<Vec<BesideChange>, String> {
        let mut taken = Vec::new();
        let mut held = self.resources_beside();
        held.extend(self.kept_changed());
        for checkout in held {
            match self.take_resource_changes(&checkout.id) {
                Ok(Some(handed)) => taken.push(handed),
                Ok(None) => {}
                Err(error) => {
                    for handed in taken {
                        if let Some(sync) = self.beside.get_mut(&handed.document) {
                            sync.put_back(handed.mutation());
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(taken)
    }

    /// `document` as one the view opened from its workspace: a document of its own that its
    /// store asked its workspace for (a Files window's tree and its chunks), which the store
    /// now holds or was opened as. `None` for anything else, which no document here may take.
    fn opened_from_workspace(
        &self,
        document: &ui_core::resource::DocumentId,
    ) -> Option<DocumentCheckout> {
        if let Some(opened) = self
            .records
            .open_resources()
            .into_iter()
            .find(|checkout| checkout.id == *document)
        {
            return Some(opened);
        }
        let kind = self.records.wanted_kind(document)?;
        Some(DocumentCheckout::new(document.clone(), kind))
    }

    /// Merge a peer's change into `document`, a part of one of the app's
    /// chains ([`Self::beside`]), a document the app keeps for its workspace, or one its window
    /// opened from it ([`Self::opened_from_workspace`]), opening it first when the view holds
    /// it not yet (a part another device started). Changes that build on nothing the part
    /// holds are left alone: `Ok(false)`. Whether it changed.
    pub fn merge_resource(
        &mut self,
        document: &ui_core::resource::DocumentId,
        mutation: &DocumentMutation,
    ) -> Result<bool, String> {
        let held = self.beside(document);
        let from_workspace = held.is_none();
        let checkout = held
            .or_else(|| self.opened_from_workspace(document))
            .ok_or_else(|| {
                format!(
                    "{} is no document {} holds beside its own",
                    document.as_str(),
                    self.definition.app_id()
                )
            })?;
        let opened = !self.records.is_ready(document);
        // Opened here from the peer's whole document: what the part holds is that document.
        let mut opened_from_mutation = false;
        if opened && self.records.try_open_resource(&checkout).is_err() {
            // Kept as the peer sent it, then opened as the view's own are.
            let DocumentMutation::Snapshot(bytes) = mutation else {
                return Ok(false);
            };
            self.cache.hydrate(
                StorageKey::Record {
                    package: self.definition.app_id().into(),
                    workspace: self.workspace.clone(),
                    record: document.as_str().into(),
                },
                bytes.clone(),
                None,
            )?;
            self.records
                .try_open_resource(&checkout)
                .map_err(|error| error.to_string())?;
            opened_from_mutation = true;
        }
        if let DocumentMutation::Incremental(bytes) = mutation
            && !self
                .records
                .with(document, |doc| DocumentSync::builds_on(doc, bytes))
                .unwrap_or(false)
        {
            return Ok(false);
        }
        // What this member did and has not taken stays its own. A part
        // first met through a peer's change is not this member's to
        // publish whole: it starts from what the record already holds.
        let heads = self
            .records
            .with(document, ui_core::automerge::AutoCommit::get_heads)
            .unwrap_or_default();
        let delivered_now = !self.beside.contains_key(document);
        let sync = self
            .beside
            .entry(document.clone())
            .or_insert_with(move || DocumentSync::delivered(heads));
        if sync.snapshotted() {
            sync.set_aside(&self.records, document)
                .map_err(|error| set_aside_failed(document, &error))?;
        }
        // A part opened from the very document handed here, and delivered at its heads just
        // now, holds all of it: merging it again loaded it a second time, a second of a large
        // tracker's part, for every part a replica takes in after a start.
        let changed = if opened_from_mutation && delivered_now {
            true
        } else {
            sync.merge(&self.records, &checkout, mutation.clone())?
        };
        // What the workspace holds of it is here now: nothing founds another. A document the
        // view awaited for its workspace opened here is an arrival too: its declaration waited
        // for it, and the record is not opened through the store's own open-arrived path. A
        // chain's part opened here was awaited by nothing, and laying the view out for it would
        // build the tree for every part a replica takes in.
        let was_awaited = self.awaited.borrow_mut().remove(document);
        if from_workspace || (opened && was_awaited) {
            // The window that asked its workspace for it opens it at its next frame, as it
            // does one the workspace delivered: what it waited for arrived.
            self.records.note_kept();
        }
        self.take_up_kept();
        // A part opened here is new to the view even when the change merged
        // into it is what it opened from, and what the view asks the
        // workspace for may move with it (a chain read whole asks for the
        // parts before it): a frame says so before the view is asked. A
        // change into a part the view already holds moves what that part
        // holds and nothing the view asks for: it is laid out when the view
        // is next read, as the view's own changes are, however often the
        // view is asked what it wants meanwhile.
        if opened {
            self.unsettle_merged();
        } else if changed {
            self.unsettle();
        }
        Ok(changed)
    }
}

/// Lay `frame` out, and take up what that frame asked for: the invocations
/// it submitted, the replies it asked a worker for. One frame when it asked
/// for nothing; when what it submitted changed the record, the frames that
/// shows ([`FrameHarness::settle_owed`]). Laid out as a still frame
/// ([`ui_core::arriving::still_frames`]): a view no window shows has no later frame.
fn lay_out(frame: &mut FrameHarness) {
    let _still = ui_core::arriving::still_frames();
    frame.relayout();
    if frame.settle_if_owed() == (ui_core::headless::OwedSettle::Unframed { moved: true }) {
        frame.settle_owed();
    }
}

/// What to publish next of `record` as `sync` stands: the record whole the
/// first time, then this member's own changes set aside since; `None`
/// when it changed nothing.
///
/// A failure to collect them is an error, never `None`: a host answers a
/// write by what this returns, and "nothing changed" would answer one that
/// was never saved as done.
fn publish(
    sync: &mut DocumentSync,
    records: &ResourceStore,
    id: &ui_core::resource::DocumentId,
) -> Result<Option<DocumentMutation>, String> {
    let failed = |error: String| {
        format!(
            "this member's change to {} could not be collected: {error}",
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

/// [`publish`] of a record held beside the view's own: the changes this member
/// made, and, for a record it started, the record whole. A peer that holds
/// none of the record founds it from the whole; the changes are what its
/// host admits without loading the record again.
///
/// # Errors
/// As [`publish`].
fn publish_beside(
    sync: &mut DocumentSync,
    records: &ResourceStore,
    id: &ui_core::resource::DocumentId,
) -> Result<Option<BesideChange>, String> {
    let failed = |error: String| {
        format!(
            "this member's change to {} could not be collected: {error}",
            id.as_str()
        )
    };
    let (changes, whole) = if sync.snapshotted() {
        sync.set_aside(records, id).map_err(failed)?;
        let Some(changes) = sync.take_set_aside() else {
            return Ok(None);
        };
        (changes, None)
    } else {
        let (whole, changes, heads) = records
            .replication_whole_and_changes(id)
            .map_err(|error| failed(error.to_string()))?;
        sync.advance(heads);
        (DocumentMutation::Incremental(changes), Some(whole))
    };
    Ok(Some(BesideChange {
        document: id.clone(),
        changes,
        whole,
    }))
}

/// Why a peer's change to `record` was not merged: this member's own
/// changes to it could not be set aside first, so the record is left as it
/// was, its own changes still to go out, rather than merged around them.
fn set_aside_failed(record: &ui_core::resource::DocumentId, error: &str) -> String {
    format!(
        "a peer's change to {} was not merged: this member's own changes to it \
         could not be set aside first ({error})",
        record.as_str()
    )
}

/// The storage keys `changes` write, top down to what each put or deleted
/// (each object's keys once per change), read from where their operations write
/// ([`ui_core::automerge::Automerge::change_touches`]): an object they make is
/// placed where they make it, any other where `doc` holds it. Nothing of `doc`
/// is copied or applied to and no value is decoded, so the answer costs the
/// changes, not the document: the replica that admitted them names what they
/// touched. `None` when an operation is on an object neither `doc` nor the
/// changes hold.
#[must_use]
pub fn keys_written<'a>(
    doc: &ui_core::automerge::AutoCommit,
    changes: impl IntoIterator<Item = &'a ui_core::automerge::Change>,
) -> Option<Vec<Vec<String>>> {
    use std::collections::HashMap;
    use ui_core::automerge::{ObjId, Prop, ReadDoc};
    let held = doc.committed_document();
    // Each object the changes make, by its id: where it is made.
    let mut made: HashMap<ObjId, (ObjId, Option<String>)> = HashMap::new();
    // What the operations touch: a map's key, or a list or text as a whole.
    let mut touched: Vec<(ObjId, Option<String>)> = Vec::new();
    for change in changes {
        let writes = held.change_touches(change)?;
        for (object, parent, key) in writes.made {
            made.insert(object, (parent, key));
        }
        touched.extend(writes.writes);
    }
    let mut paths: HashMap<ObjId, Vec<String>> = HashMap::new();
    let mut path_of = |obj: &ObjId| -> Option<Vec<String>> {
        // Up through what the changes make, to the root or an object `doc`
        // holds, whose path `doc` names.
        let mut above: Vec<Option<String>> = Vec::new();
        let mut at = obj.clone();
        let base = loop {
            if at == ui_core::automerge::ROOT {
                break Vec::new();
            }
            if let Some(known) = paths.get(&at) {
                break known.clone();
            }
            if let Some((parent, key)) = made.get(&at) {
                above.push(key.clone());
                at = parent.clone();
                if above.len() > made.len() {
                    return None;
                }
                continue;
            }
            let mut keys: Vec<String> = held
                .parents(&at)
                .ok()?
                .map(|parent| match parent.prop {
                    Prop::Map(key) => key,
                    Prop::Seq(index) => index.to_string(),
                })
                .collect();
            keys.reverse();
            paths.insert(at, keys.clone());
            break keys;
        };
        let mut path = base;
        path.extend(above.into_iter().rev().flatten());
        paths.insert(obj.clone(), path.clone());
        Some(path)
    };
    let mut written: Vec<Vec<String>> = Vec::with_capacity(touched.len());
    for (obj, key) in touched {
        let mut path = path_of(&obj)?;
        path.extend(key);
        written.push(path);
    }
    Some(written)
}

/// The member `invoker` is: an automation opens no view of its own.
fn member_of(invoker: &tilo_actions::ActionInvoker) -> Result<&MemberId, String> {
    invoker
        .acting
        .by
        .member_id()
        .ok_or_else(|| "an automation opens no view of its own".to_owned())
}

/// The heads of the record `document` holds: what a member joining it
/// was given, past which anything its view writes while it opens is refused
/// ([`refuse_written_on_open`]).
fn heads_of(document: &[u8]) -> Result<Vec<ui_core::automerge::ChangeHash>, String> {
    ui_core::automerge::AutoCommit::load(document)
        .map(|mut doc| doc.get_heads())
        .map_err(|error| error.to_string())
}

/// Refuse a view that wrote to the record `checkout` names, one its
/// member joined at `joined_at`, as it opened ([`written_on_open`]).
fn refuse_written_on_open(
    definition: &ApplicationDefinition,
    records: &ResourceStore,
    checkout: &DocumentCheckout,
    joined_at: &[ui_core::automerge::ChangeHash],
) -> Result<(), String> {
    match records.with(&checkout.id, |doc| written_on_open(doc, joined_at)) {
        Some(Some(wrote)) => Err(format!(
            "{} wrote to a record it joined as it opened ({wrote}): only the record's \
             creator writes its initial content, and a member that joins adds \
             nothing on open. What the record starts with belongs in the app's seed; \
             what a member adds belongs behind a control they use",
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
/// Refused rather than kept: the member's own changes are sent from
/// where it joined, so the view's write would be sent as if the member
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

mod page;

#[cfg(test)]
mod tests;
