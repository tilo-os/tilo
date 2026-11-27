//! An app's typed settings, files and secrets as views and surfaces reach
//! them: from their context, scoped to the app and the settings document its
//! host gave it.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use settings_model::SettingsSchema;
use tilo_ui_base::LocalState;
use ui_core::storage::{AppStorage, Settings, SettingsRevision};

use super::resources::{Held, ResourceApi};
use super::section::{SectionApi, SectionKind};

/// One typed settings value, kept current while the view lives: a write
/// from this window, another window, the Settings app or another of the
/// person's devices (for [`Scope::Personal`](settings_model::Scope))
/// reaches [`Self::state`] on the next frame.
///
/// Bind controls to [`Self::state`] and write with [`Self::set`]. Settings
/// are not the document: they are not undone and never seen by
/// members.
///
/// A section that is a resource model (`#[derive(Model)]` and an empty
/// `#[resource]`) is also a record its views edit by path
/// ([`ViewCx::record`](super::ViewCx::record)): its field edits and verbs
/// are published as `settings:<kind>`, each change is checked by the
/// model's invariants and recorded in the settings document with what made
/// it and who.
pub struct Setting<T> {
    settings: Settings<T>,
    value: LocalState<T>,
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            value: self.value.clone(),
        }
    }
}

impl<T: SettingsSchema + Clone + PartialEq + Send + Sync> Setting<T> {
    /// The current value.
    #[must_use]
    pub fn get(&self) -> T {
        self.value.with(Clone::clone)
    }

    /// The value as state that follows every write.
    #[must_use]
    pub fn state(&self) -> LocalState<T> {
        self.value.clone()
    }

    /// Store `value`.
    ///
    /// # Errors
    ///
    /// When the document refuses it (see [`Settings::set`]); the value shown
    /// stays the stored one.
    pub fn set(&self, value: T) -> std::io::Result<()> {
        self.settings.set(&value)?;
        self.value.set(value);
        Ok(())
    }

    /// Change the stored value with `change`.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    pub fn update(&self, change: impl FnOnce(&mut T)) -> std::io::Result<()> {
        let value = self.settings.update(change)?;
        self.value.set(value);
        Ok(())
    }

    /// The underlying settings, for code that runs off the view (a job).
    #[must_use]
    pub fn settings(&self) -> &Settings<T> {
        &self.settings
    }
}

type Refresh = Box<dyn FnMut()>;

impl<T> super::resources::sealed::Hold<T> for Setting<T>
where
    T: tilo_resource::Resource<State = T>
        + tilo_resource::Fields
        + SettingsSchema
        + Clone
        + PartialEq
        + Send
        + Sync,
{
    fn held(&self, storage: super::resources::sealed::Storage<'_>) -> Option<Rc<Held<T>>> {
        storage.0.section_held(&self.settings)
    }

    fn api(
        &self,
        storage: super::resources::sealed::Storage<'_>,
    ) -> Option<super::resources::sealed::Api> {
        Some(super::resources::sealed::Api(
            storage.0.section(&self.settings) as Rc<dyn ResourceApi>,
        ))
    }
}

/// The app's storage and the settings its views follow. Shared by every
/// scope of one root; the root refreshes it each sync.
pub(crate) struct ViewStorage {
    storage: AppStorage,
    followed: RefCell<Vec<(Weak<()>, Refresh)>>,
    /// The person's own records, when the host keeps them: not in a view of
    /// a past version, which shows the record it is a version of.
    person: Option<super::person::PersonState>,
    /// This device, in a live window.
    device: Option<String>,
    /// The settings sections views and surfaces edit as records, by kind.
    sections: RefCell<HashMap<&'static str, Rc<SectionApi>>>,
    /// Those a view part edits through its type, by type.
    held_sections: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
    /// Those sections, as the API reaches them.
    section_apis: RefCell<Vec<Rc<dyn ResourceApi>>>,
    /// The records another provider keeps, by id, with that provider
    /// ([`Resources::kept_by`](super::Resources::kept_by)): views show them and
    /// bind to them, the window neither publishes nor runs their actions.
    kept_by: RefCell<HashMap<String, &'static str>>,
    /// The store of the app's records in the workspace, for the records it
    /// keeps beside its own: not in a view of a past version.
    records: Option<super::chain::WorkspaceResources>,
    /// The chains the views opened ([`super::Chain`]).
    chains: super::chain::Chains,
    /// The view operations the chains publish on what they hold, at the
    /// chains' revision they were collected at.
    chain_views: RefCell<Option<(u64, Rc<std::collections::HashSet<String>>)>>,
    /// The records every app of the workspace opens, when the host keeps them:
    /// not in a view of a past version.
    workspace: Option<Rc<super::workspace::WorkspaceState>>,
    /// The workspace's emoji, as the window's views draw them.
    emoji: Option<Rc<super::workspace_emoji::WindowEmoji>>,
    /// The window's own route to the session, where it runs in one: taken while the window is
    /// constructed, the only time it can be, and handed to views built any time after.
    session: Option<::collaboration::browser_service_sessions::Client>,
}

/// What the host gives a live window beside its record: where the person's own
/// records are and this device's id. Neither reaches a view of a past version.
#[derive(Default)]
pub(crate) struct Hosted {
    /// The person at the window and where their own records are; `None` for a private window,
    /// which keeps nothing of theirs.
    pub(crate) person: Option<(tilo_resource::MemberId, super::person::PersonRecords)>,
    pub(crate) device: Option<String>,
    /// The store of the app's records in the workspace, who edits there and
    /// what they may do (`None`: a past version, where nobody edits).
    pub(crate) records: Option<(
        ui_core::resource::ResourceStore,
        tilo_resource::MemberId,
        Option<tilo_permissions::LivePermissions>,
    )>,
    /// The records every app of the workspace opens, and the window's wake
    /// scope, against which the workspace's emoji are registered.
    pub(crate) workspace: Option<(
        ui_core::resource::workspace::WorkspaceResources,
        ui_core::wake::WakeHandle,
    )>,
    /// The window's wake scope: what wakes it when another replica's change
    /// reaches a record the person keeps ([`super::PersonResource`]), which
    /// nothing else tells it of.
    pub(crate) wake: Option<ui_core::wake::WakeHandle>,
}

/// [`Hosted`], with the person's records opened for one app.
pub(crate) struct Attached {
    person: Option<super::person::PersonState>,
    device: Option<String>,
    records: Option<super::chain::WorkspaceResources>,
    workspace: Option<Rc<super::workspace::WorkspaceState>>,
    emoji: Option<Rc<super::workspace_emoji::WindowEmoji>>,
}

impl Hosted {
    pub(crate) fn attach(self, app_id: &'static str, app_title: &'static str) -> Attached {
        let permissions = self
            .records
            .as_ref()
            .and_then(|(_, _, permissions)| permissions.clone());
        // The person's records the window opens are their own: one the
        // workspace keeps is the window's workspace record, opened from
        // `workspace` instead.
        let workspace = self.workspace.map(|(records, wake)| {
            let state = Rc::new(super::workspace::WorkspaceState::new(
                records,
                app_id,
                app_title,
                permissions,
                Some(wake.clone()),
            ));
            let emoji = super::workspace_emoji::register(&wake, Rc::clone(&state));
            (state, emoji)
        });
        let (workspace, emoji) = workspace.unzip();
        let wake = self.wake.clone();
        Attached {
            workspace,
            emoji,
            person: self.person.map(|(me, records)| {
                super::person::PersonState::new(records, me, app_id, app_title, wake)
            }),
            device: self.device,
            records: self.records.map(|(store, me, permissions)| {
                super::chain::WorkspaceResources {
                    store,
                    me,
                    permissions,
                    app_id,
                    app_title,
                    device_queue: None,
                }
            }),
        }
    }
}

impl ViewStorage {
    pub(crate) fn new(storage: AppStorage, mut hosted: Attached) -> Rc<Self> {
        if let Some(records) = &mut hosted.records {
            records.device_queue = Some(storage.settings::<super::chain::QueuedWrites>());
        }
        let session = ::collaboration::browser_service_sessions::Client::capture_kept(
            hosted
                .records
                .as_ref()
                .map(|records| records.store.home())
                .as_deref(),
        );
        Rc::new(Self {
            storage,
            followed: RefCell::default(),
            person: hosted.person,
            device: hosted.device,
            sections: RefCell::default(),
            held_sections: RefCell::default(),
            section_apis: RefCell::default(),
            kept_by: RefCell::default(),
            records: hosted.records,
            chains: super::chain::Chains::default(),
            chain_views: RefCell::default(),
            workspace: hosted.workspace,
            emoji: hosted.emoji,
            session,
        })
    }

    /// The window's own route to the session, where it runs in one.
    pub(crate) fn session(&self) -> Option<::collaboration::browser_service_sessions::Client> {
        self.session
            .as_ref()
            .map(::collaboration::browser_service_sessions::Client::another)
    }

    /// The chain of `P` the app keeps beside its record, opened on first
    /// use, its actions reading the app's record from `base`. `None` where
    /// the window keeps no records in a workspace (a view of a past version).
    pub(crate) fn chain<P: super::Chained>(
        &self,
        record: Option<&ui_core::resource::DocumentId>,
        base: impl Fn() -> Rc<<P::Base as tilo_resource::Resource>::State> + 'static,
    ) -> Option<Rc<super::chain::ChainState>> {
        super::controls::opened_resource::<P>();
        self.chains.open::<P>(self.records.as_ref(), record, base)
    }

    /// The chain of `P` that lives beside the record `base` rather than
    /// beside the app's own ([`super::Chained::chain_beside`]), opened on
    /// first use, its actions reading that record from `base_state`.
    pub(crate) fn chain_beside<P: super::Chained>(
        &self,
        base: &ui_core::resource::DocumentId,
        base_state: impl Fn() -> Rc<<P::Base as tilo_resource::Resource>::State> + 'static,
    ) -> Option<Rc<super::chain::ChainState>> {
        super::controls::opened_resource::<P>();
        self.chains
            .open_beside::<P>(self.records.as_ref(), base, base_state)
    }

    /// The children of `C` the parent lists, held on first use, the parent
    /// read from `parent`. `None` where the window keeps no records in a
    /// workspace (a view of a past version).
    pub(crate) fn children<C: super::children::ChildResource>(
        &self,
        parent: impl Fn() -> Rc<<C::Parent as tilo_resource::Resource>::State> + 'static,
    ) -> Option<Rc<super::children::ChildrenState<C>>> {
        super::controls::opened_resource::<C>();
        self.chains
            .open_children::<C>(self.records.as_ref(), parent)
    }

    /// Answer one published action of a chain the views opened, invoked by
    /// someone the window knows as `me`.
    pub(crate) fn chain_action(
        &self,
        me: Option<&tilo_resource::MemberId>,
        payload: &[u8],
        live: &mut tilo_ui_base::widget::Edits<'_>,
    ) -> Result<tilo_actions::ActionOutput, tilo_resource::Refusal> {
        self.chains.answer(me, payload, live)
    }

    /// Answer a read of a chain the views opened
    /// ([`super::chain::CHAIN_READ_TYPE`]): what its parts hold, read here.
    pub(crate) fn chain_read(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        let chain = serde_json::from_slice::<serde_json::Value>(payload)
            .ok()
            .and_then(|args| {
                args.get("chain")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or("a chain read names the chain it reads")?;
        let value = self.chains.read(&chain, payload)?;
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    pub(crate) fn storage(&self) -> &AppStorage {
        &self.storage
    }

    /// The store of the app's records in the workspace; `None` in a view of
    /// a past version.
    pub(crate) fn workspace_store(&self) -> Option<&ui_core::resource::ResourceStore> {
        self.records.as_ref().map(|records| &records.store)
    }

    /// The records every app of the window's workspace opens, when the host keeps them: what
    /// History reads the workspace's tree and its chunks from after the window launched.
    pub(crate) fn workspace_state(&self) -> Option<Rc<super::workspace::WorkspaceState>> {
        self.workspace.clone()
    }

    /// This device's id, in a live window.
    pub(crate) fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// Open the record `P` the person keeps for themselves, in their own workspace
    /// rather than the window's: presentation data only they use.
    pub(crate) fn profile_resource<P: tilo_resource::Resource>(
        &self,
    ) -> Option<super::person::PersonResource<P>> {
        // It is the person's own, as a settings section is: a surface report lists it as the
        // person's.
        super::controls::opened_section::<P>();
        self.person.as_ref()?.open_profile::<P>()
    }

    /// Open the record `P` every app of the workspace opens that this app
    /// keeps.
    pub(crate) fn workspace_resource<P: tilo_resource::Resource>(&self) {
        if let Some(workspace) = &self.workspace {
            let _ = workspace.open::<P>(workspace.app_id());
        }
    }

    /// Open an additional workspace record owned by this app.
    pub(crate) fn workspace_record<P: tilo_resource::Resource>(&self, id: &str) {
        if let Some(workspace) = &self.workspace {
            let _ = workspace.open_at::<P>(workspace.app_id(), id);
        }
    }

    /// The record `P` every app of the workspace opens that `owner` keeps,
    /// opened on first use.
    pub(crate) fn shared_workspace<P: tilo_resource::Resource>(
        &self,
        owner: &'static str,
    ) -> Option<super::workspace::WorkspaceResource<P>> {
        self.workspace.as_ref()?.open::<P>(owner)
    }

    /// The record `id` that `owner` keeps, opened on first use.
    pub(crate) fn shared_resource<P: tilo_resource::Resource>(
        &self,
        owner: &'static str,
        id: &str,
    ) -> Option<super::workspace::WorkspaceResource<P>> {
        self.workspace.as_ref()?.open_at::<P>(owner, id)
    }

    /// The records other than the app's own the views opened — the
    /// person's records, settings sections — as published resources.
    /// What [`Self::resource_resources`] publishes depends on: each record's
    /// id and revision, in publication order.
    pub(crate) fn resources_key(&self) -> Vec<(String, u64)> {
        let mut apis = self
            .person
            .as_ref()
            .map(super::person::PersonState::apis)
            .unwrap_or_default();
        apis.extend(self.section_apis.borrow().iter().cloned());
        apis.extend(self.chains.apis());
        apis.extend(self.workspace_apis());
        apis.iter()
            .map(|api| (api.id().to_owned(), api.revision()))
            .collect()
    }

    /// The person's records a view asked for and could not open, as they
    /// are published: their declared actions, unavailable for why.
    pub(crate) fn unavailable_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.person
            .as_ref()
            .map(super::person::PersonState::unavailable)
            .unwrap_or_default()
    }

    /// `read` of every part the chains the views opened hold.
    pub(crate) fn read_chain_parts<T>(
        &self,
        read: &dyn Fn(&ui_core::automerge::AutoCommit) -> T,
    ) -> Vec<(ui_core::history_mode::HistoryPart, T)> {
        self.chains.read_parts(read)
    }

    /// A part's version beside the other parts of its chain ([`super::chain::Chains::past_part`]).
    pub(crate) fn past_part(
        &self,
        version: &ui_core::history_mode::PartVersion<'_>,
    ) -> Result<super::chain::PastPart, String> {
        self.chains.past_part(version)
    }

    /// Write a part back as it was at `heads` ([`super::chain::Chains::restore_part`]).
    pub(crate) fn restore_chain_part(
        &self,
        part: &ui_core::history_mode::HistoryPart,
        heads: &[ui_core::automerge::ChangeHash],
        author: &tilo_resource::MemberId,
    ) -> Result<(), String> {
        self.chains.restore_part(part, heads, author)
    }

    /// What search finds in the chains the views opened.
    pub(crate) fn chains_search(&self) -> Vec<tilo_resource::SearchEntry> {
        self.chains
            .apis()
            .iter()
            .flat_map(|api| api.search())
            .collect()
    }

    /// Answer a read of `item` that names nothing the app's own resource
    /// holds but something a chain it keeps derives over its parts (a mail
    /// folder, read as the messages every part holds): what a read of the
    /// app's resource falls back to, and `None` when no chain derives
    /// anything of it.
    pub(crate) fn read_over(&self, item: &str) -> Option<Result<serde_json::Value, String>> {
        self.chains
            .apis()
            .into_iter()
            .find_map(|api| api.read_over(item))
    }

    /// The items `name` names among what the chain `parent` holds, each built
    /// alone by the part keeping it ([`ResourceApi::items_named`]): what a
    /// path naming one costs, however many items the chain holds.
    pub(crate) fn chain_items_named(
        &self,
        parent: &str,
        name: &str,
    ) -> Vec<tilo_actions::ResourceSpec> {
        self.chains
            .apis()
            .into_iter()
            .find(|api| api.id() == parent)
            .map(|api| api.items_named(name))
            .unwrap_or_default()
    }

    /// The item `id` of a chain the views opened, built alone by the part
    /// keeping it ([`ResourceApi::item`]) and carried out where the chain is:
    /// what an action addressed to it resolves, however many items the chain
    /// holds.
    pub(crate) fn chain_item(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        self.chains
            .apis()
            .into_iter()
            .find_map(|api| Some(self.kept_where(api.id(), api.item(id)?)))
    }

    /// The chains the app declared that `name` names by their id, kind or
    /// label, each as its head part publishes it ([`ResourceApi::publish_head`]):
    /// what a path's first segment costs, however many items the chains hold.
    pub(crate) fn chains_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        let same = |text: &str| text.trim().eq_ignore_ascii_case(name);
        self.chain_heads()
            .filter(|head| same(head.id.as_str()) || same(&head.kind) || same(&head.label))
            .collect()
    }

    /// How many items the chain `parent` keeps past the page each of its
    /// parts publishes ([`super::chain::ChainState::past_page_len`]): what a
    /// listing of the chain reaches past those pages.
    pub(crate) fn chain_past_page_len(&self, parent: &str) -> usize {
        self.chain_api(parent).map_or(0, |api| api.past_page_len())
    }

    /// [`Self::chain_past_page_len`] from `offset` on, at most `take`, each
    /// built alone: what a page of a listing past the parts' pages costs.
    pub(crate) fn chain_past_page_window(
        &self,
        parent: &str,
        offset: usize,
        take: usize,
    ) -> Vec<tilo_actions::ResourceSpec> {
        self.chain_api(parent)
            .map(|api| api.past_page_window(offset, take))
            .unwrap_or_default()
    }

    /// The chain `id` as it publishes its items, shared: the nodes of the parts it holds,
    /// built again only for a part that changed, without the frame of the view or the rest of
    /// what it publishes. `None` for a chain the views did not open, or one another provider
    /// keeps, which is built whole for that provider to name on every action.
    pub(crate) fn chain_published(&self, id: &str) -> Option<ui_core::published::SharedResource> {
        if self.kept_by.borrow().contains_key(id) {
            return None;
        }
        self.chain_api(id).map(|api| api.publish_shared())
    }

    /// Whether a page listing the chain `parent` reads a part not here yet
    /// ([`super::chain::ChainState::listing_arriving`]), `filled` when the parts held filled
    /// it; for any other level, whether a chain the views opened holds no part yet, or misses
    /// one between those it holds.
    pub(crate) fn listing_arriving(&self, parent: &str, filled: bool) -> bool {
        match self.chain_api(parent) {
            Some(api) => api.listing_arriving(filled),
            None => self
                .chains
                .apis()
                .iter()
                .any(|api| api.listing_arriving(true)),
        }
    }

    /// The chain `id`, among the chains the views opened.
    fn chain_api(&self, id: &str) -> Option<Rc<dyn super::resources::ResourceApi>> {
        self.chains.apis().into_iter().find(|api| api.id() == id)
    }

    /// The chain `id` as its head part publishes it, without the items it
    /// holds: what an address on the chain itself (its `create`, the kind of
    /// its parts) is answered from, at the head's cost.
    pub(crate) fn chain_head(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        super::chain_of_resource_id(id)?;
        self.chain_heads().find(|head| head.id.as_str() == id)
    }

    /// Every chain the app declared, each as its head part publishes it.
    fn chain_heads(&self) -> impl Iterator<Item = tilo_actions::ResourceSpec> + '_ {
        self.chains
            .apis()
            .into_iter()
            .filter(|api| super::chain_of_resource_id(api.id()).is_some())
            .map(|api| self.kept_where(api.id(), api.publish_head()))
    }

    /// `resource`, of the one `owner` names, with its actions carried out by
    /// the provider keeping `owner` when another one does.
    fn kept_where(
        &self,
        owner: &str,
        mut resource: tilo_actions::ResourceSpec,
    ) -> tilo_actions::ResourceSpec {
        if let Some(provider) = self.kept_provider(owner) {
            run_by(&mut resource, provider);
        }
        resource
    }

    /// A revision that moves whenever a chain the views opened changed.
    pub(crate) fn chains_revision(&self) -> u64 {
        self.chains.apis().iter().fold(0u64, |revision, api| {
            revision.wrapping_mul(31).wrapping_add(api.revision())
        })
    }

    /// The view operations the records kept beside the app's own publish,
    /// on themselves or on what they hold (a note's body opened in an
    /// editor): the window handles them as it does its own record's.
    pub(crate) fn view_types_beside(&self) -> std::collections::HashSet<String> {
        fn collect(
            resource: &tilo_actions::ResourceSpec,
            views: &mut std::collections::HashSet<String>,
        ) {
            views.extend(
                resource
                    .actions
                    .iter()
                    .filter(|spec| spec.kind == tilo_actions::OperationKind::View)
                    .filter_map(|spec| spec.operation_type.clone()),
            );
            for child in &resource.children {
                collect(child, views);
            }
        }
        let mut views = std::collections::HashSet::new();
        let mut beside = self
            .person
            .as_ref()
            .map(super::person::PersonState::resources)
            .unwrap_or_default();
        beside.extend(self.section_apis.borrow().iter().map(|api| api.publish()));
        for resource in &beside {
            collect(resource, &mut views);
        }
        // A chain holds every part's items: collected once per change.
        let revision = self.chains_revision();
        let mut kept = self.chain_views.borrow_mut();
        let chains = match kept.as_ref() {
            Some((at, chains)) if *at == revision => Rc::clone(chains),
            _ => {
                let mut chains = std::collections::HashSet::new();
                for api in self.chains.apis() {
                    collect(&api.publish(), &mut chains);
                }
                let chains = Rc::new(chains);
                *kept = Some((revision, Rc::clone(&chains)));
                chains
            }
        };
        views.extend(chains.iter().cloned());
        views
    }

    pub(crate) fn resource_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let mut resources = self.own_resources();
        resources.extend(self.chains.apis().iter().map(|api| api.publish()));
        resources.extend(self.workspace_apis().iter().map(|api| api.publish()));
        let kept_by = self.kept_by.borrow();
        if !kept_by.is_empty() {
            for resource in &mut resources {
                if let Some(provider) = kept_by.get(resource.id.as_str()) {
                    run_by(resource, provider);
                }
            }
        }
        resources
    }

    /// The person's records and the settings sections the views opened, published: sections
    /// of two types the settings keep in one place are one resource there.
    fn own_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let mut resources = self
            .person
            .as_ref()
            .map(super::person::PersonState::resources)
            .unwrap_or_default();
        resources.extend(self.section_apis.borrow().iter().map(|api| api.publish()));
        let sections = self.sections.borrow();
        // The same kind whichever order the sections were opened in.
        super::section::one_per_place(resources, |id| {
            sections
                .values()
                .filter(|api| api.id() == id)
                .map(|api| api.place_kind())
                .min()
        })
    }

    /// [`Self::resource_resources`] as shared trees: a chain's items are the nodes its last
    /// publication carried wherever its parts are unchanged.
    pub(crate) fn resource_resources_shared(&self) -> Vec<ui_core::published::SharedResource> {
        let kept_by = self.kept_by.borrow();
        let owned = |resource: tilo_actions::ResourceSpec| {
            let owner = resource.id.as_str().to_owned();
            ui_core::published::SharedResource::from_resource(self.kept_where(&owner, resource))
        };
        let mut resources: Vec<ui_core::published::SharedResource> =
            self.own_resources().into_iter().map(owned).collect();
        resources.extend(self.chains.apis().iter().map(|api| {
            // A chain run by another provider names it on every action, so it is built whole.
            if kept_by.contains_key(api.id()) {
                owned(api.publish())
            } else {
                api.publish_shared()
            }
        }));
        resources.extend(self.workspace_apis().iter().map(|api| owned(api.publish())));
        resources
    }

    /// Mark the record `id` as kept by `provider`
    /// ([`Resources::kept_by`](super::Resources::kept_by)).
    pub(crate) fn keep_by(&self, id: &str, provider: &'static str) {
        self.kept_by.borrow_mut().insert(id.to_owned(), provider);
    }

    /// The provider keeping the record `id`, when another one does.
    pub(crate) fn kept_provider(&self, id: &str) -> Option<&'static str> {
        self.kept_by.borrow().get(id).copied()
    }

    /// Answer one published action of those records, invoked by someone
    /// the window knows as `me`.
    pub(crate) fn resource_action(
        &self,
        me: Option<&tilo_resource::MemberId>,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, String> {
        let me = self
            .person
            .as_ref()
            .map(super::person::PersonState::me)
            .or(me);
        super::resources::answer(&self.answering_apis(), me, payload)
    }

    /// Answer a `read` of one of those records, named `record`
    /// ([`tilo_resource::read::resource_spec`]).
    pub(crate) fn resource_read(
        &self,
        record: &str,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, String> {
        let api = self
            .answering_apis()
            .into_iter()
            .find(|api| api.id() == record)
            .ok_or_else(|| format!("no record `{record}` is open here"))?;
        Ok(tilo_actions::ActionOutput {
            value: Some(api.read(payload)?),
            resources: Vec::new(),
        })
    }

    /// The person's records and settings sections the window answers for. A record of theirs
    /// kept in their own workspace is answered here too, which sends each of its changes to the
    /// app there.
    fn answering_apis(&self) -> Vec<Rc<dyn ResourceApi>> {
        let mut apis = self
            .person
            .as_ref()
            .map(super::person::PersonState::apis)
            .unwrap_or_default();
        apis.extend(self.section_apis.borrow().iter().cloned());
        apis.extend(self.workspace_apis());
        let kept_by = self.kept_by.borrow();
        apis.retain(|api| !kept_by.contains_key(api.id()));
        apis
    }

    /// Open what was on its way from the workspace when it was asked for and has arrived
    /// since ([`super::workspace::WorkspaceState::open_arrived`]): whether anything opened.
    pub(crate) fn open_arrived(&self) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|workspace| workspace.open_arrived())
    }

    /// Whether a document the views asked for is still on its way: the workspace had not
    /// delivered it, or its store was in use, when it was asked for.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn opening(&self) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|workspace| workspace.opening())
    }

    /// What the window's view waits for: the documents its app's declarations asked for that
    /// are still on their way ([`super::workspace::WorkspaceState::declared_arriving`]).
    pub(crate) fn declared_arriving(
        &self,
    ) -> Option<tilo_ui_base::arriving::Arriving<(), tilo_ui_base::workspace_sync::RecordNotHere>>
    {
        self.workspace.as_ref()?.declared_arriving()
    }

    /// When the window's declared wait ends unanswered, unless what it waits for opens first
    /// ([`super::workspace::WorkspaceState::gives_up_at`]).
    pub(crate) fn gives_up_at(&self) -> Option<ui_core::time::Instant> {
        self.workspace.as_ref()?.gives_up_at()
    }

    /// The part of a view kept alive by `part` is being built, until what this returns is
    /// dropped ([`super::workspace::WorkspaceState::building`]). `None` in a window that
    /// opens no documents of a workspace.
    pub(crate) fn building(&self, part: &Rc<()>) -> Option<super::workspace::Building> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.building(part))
    }

    /// Whether the part kept alive by `part` is to be built again: a document of the
    /// workspace it asked for while it was built, and that was on its way, has opened since
    /// ([`super::workspace::WorkspaceState::take_arrived`]).
    pub(crate) fn take_arrived(&self, part: &Rc<()>) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|workspace| workspace.take_arrived(part))
    }

    /// What the app keeps for its whole workspace beside its own resource, as the API reaches
    /// it: offered by the keeping app's window alone, and decided by the workspace's grants.
    fn workspace_apis(&self) -> Vec<Rc<dyn ResourceApi>> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.apis())
            .unwrap_or_default()
    }

    /// The settings section `T` held as a record, opened on first use: with
    /// the undo and redo of the person's steps on it, unless the window
    /// keeps nothing of theirs (a private one).
    pub(crate) fn section<T>(&self, settings: &Settings<T>) -> Rc<SectionApi>
    where
        T: tilo_resource::Resource<State = T> + tilo_resource::Fields + SettingsSchema,
    {
        super::controls::opened_section::<T>();
        if let Some(open) = self.sections.borrow().get(T::KIND) {
            return Rc::clone(open);
        }
        self.open_section(SectionKind::of(settings))
    }

    /// [`Self::section`] of the section `kind` describes.
    fn open_section(&self, kind: Rc<SectionKind>) -> Rc<SectionApi> {
        let name = kind.kind;
        let api = if let Some(person) = &self.person {
            person.open_section(kind)
        } else {
            let api = Rc::new(SectionApi::new(kind, None, Vec::new()));
            self.section_apis
                .borrow_mut()
                .push(Rc::clone(&api) as Rc<dyn ResourceApi>);
            api
        };
        self.sections.borrow_mut().insert(name, Rc::clone(&api));
        api
    }

    /// The settings section `T` as a record a view part edits through its
    /// type ([`ViewCx::record`](super::ViewCx::record)).
    pub(crate) fn section_held<T>(&self, settings: &Settings<T>) -> Option<Rc<Held<T>>>
    where
        T: tilo_resource::Resource<State = T> + tilo_resource::Fields + SettingsSchema,
    {
        if let Some(held) = self.held_sections.borrow().get(&TypeId::of::<T>()) {
            return Rc::clone(held).downcast::<Held<T>>().ok();
        }
        let held = Rc::new(self.section(settings).held::<T>());
        self.held_sections
            .borrow_mut()
            .insert(TypeId::of::<T>(), Rc::clone(&held) as Rc<dyn Any>);
        Some(held)
    }

    /// Run `refresh` with every refresh while `alive` lives.
    pub(crate) fn follow_with(&self, alive: Weak<()>, refresh: impl FnMut() + 'static) {
        self.followed.borrow_mut().push((alive, Box::new(refresh)));
    }

    /// Answer an undo or redo of one of the person's records.
    pub(crate) fn person_history(
        &self,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.person
            .as_ref()
            .ok_or_else(|| "this window keeps no records of yours".to_owned())?
            .answer(payload)
    }

    /// The person's records the views opened, and the routes of the
    /// records every app of the workspace opens.
    pub(crate) fn person_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        let mut owners = self
            .person
            .as_ref()
            .map(super::person::PersonState::owners)
            .unwrap_or_default();
        if let Some(workspace) = &self.workspace {
            owners.extend(workspace.owners());
        }
        owners
    }

    /// `T`, followed for as long as `alive` is.
    pub(crate) fn follow<T>(&self, alive: Weak<()>) -> Setting<T>
    where
        T: SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        let settings = self.storage.settings::<T>();
        let mut seen: SettingsRevision = settings.revision();
        let value = LocalState::new(settings.get());
        let setting = Setting {
            settings: settings.clone(),
            value: value.clone(),
        };
        self.followed.borrow_mut().push((
            alive,
            Box::new(move || {
                let revision = settings.revision();
                if revision == seen {
                    return;
                }
                seen = revision;
                let stored = settings.get();
                if value.with(|shown| *shown != stored) {
                    value.set(stored);
                }
            }),
        ));
        setting
    }

    /// Take up a newer head of each chain that arrived on this device, and open the workspace's
    /// records that arrived: in a frame's settle, where the store takes the writer opening it
    /// needs, so the views' next sync reads it.
    pub(crate) fn settle(&self) {
        // A workspace record shares the app's store, which the view's sync reads: what arrived
        // for one, or was asked for while that store was in use, opens here, where it is free.
        if let Some(workspace) = &self.workspace {
            workspace.open_arrived();
        }
        for chain in self.chains.apis() {
            let _ = chain.revision();
        }
    }

    /// Read every followed setting whose document changed, dropping those
    /// of views that are gone.
    pub(crate) fn refresh(&self) {
        let _profile = tilo_resource::profiling::Phase::start("framework.view_storage_refresh");
        if let Some(person) = &self.person {
            person.refresh();
        }
        if let Some(workspace) = &self.workspace {
            workspace.refresh();
        }
        if let Some(emoji) = &self.emoji {
            emoji.settle();
        }
        for chain in self.chains.apis() {
            let _ = chain.revision();
        }
        // Take the list so a refresh that builds a view (a switch reacting
        // to the new value) may follow more without a reentrant borrow.
        let mut current = std::mem::take(&mut *self.followed.borrow_mut());
        current.retain_mut(|(alive, refresh)| {
            if alive.strong_count() == 0 {
                return false;
            }
            refresh();
            true
        });
        let mut list = self.followed.borrow_mut();
        current.append(&mut list);
        *list = current;
    }
}

/// `resource`'s actions as `provider` runs them, its parts' too.
fn run_by(resource: &mut tilo_actions::ResourceSpec, provider: &'static str) {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .map(|spec| spec.run_by(provider))
        .collect();
    for child in &mut resource.children {
        run_by(child, provider);
    }
}
