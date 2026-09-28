//! The root widget of a framework app: keeps the resource state current,
//! publishes the resource and its actions, forwards edits to the host, and
//! indexes the resource for search.

use std::rc::Rc;
use std::sync::Arc;
use ui_core::widget::{AppModel, ModelPath};

use tilo_resource::{Resource, SearchEntry};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use ui_core::automerge::AutoCommit;
use ui_core::permissions::{LivePermissions, PermissionScope};
use ui_core::presence::PresenceCtx;
use ui_core::record::RecordSaveStatus;
use ui_core::search::{SearchProvider, SearchResult};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::widget::{AppKind, AppWidget, Decorator, Element, IconDescriptor, Widget};
use ui_core::{LocalState, Theme};

use super::dispatch::Dispatcher;
use super::live::Live;
use super::stage::Stages;
use super::view::Updaters;

pub(crate) struct ResourceRoot<R: Resource> {
    title: &'static str,
    icon: IconDescriptor,
    label: String,
    live: Live<R>,
    updaters: Updaters<R>,
    /// The live revision bound controls last followed.
    followed: std::cell::Cell<u64>,
    _alive: Rc<()>,
    dispatch: Dispatcher<R>,
    saved: LocalState<RecordSaveStatus>,
    /// The participant's permissions, re-checked every sync so a role change
    /// while the app runs takes effect at once. `None` for a read-only view
    /// of a past version.
    permissions: Option<LivePermissions>,
    write: Option<PermissionScope>,
    stages: Stages,
    content: Element,
    /// The resource's search entries at one live revision, indexed.
    search: std::cell::RefCell<Option<(u64, Arc<IndexedEntries>)>>,
}

impl<R: Resource> ResourceRoot<R> {
    #[allow(
        clippy::too_many_arguments,
        reason = "assembled once by the framework from parts it already owns"
    )]
    pub(crate) fn new(
        title: &'static str,
        icon: IconDescriptor,
        label: String,
        live: Live<R>,
        updaters: Updaters<R>,
        alive: Rc<()>,
        dispatch: Dispatcher<R>,
        saved: LocalState<RecordSaveStatus>,
        stages: Stages,
        content: Element,
    ) -> Self {
        Self {
            title,
            icon,
            label,
            live,
            updaters,
            followed: std::cell::Cell::new(0),
            _alive: alive,
            dispatch,
            saved,
            permissions: None,
            write: None,
            stages,
            content,
            search: std::cell::RefCell::new(None),
        }
    }

    /// Stage a screenshot scenario's view state: every view's
    /// [`on_stage`](super::ViewCx::on_stage) callback runs now, and every
    /// hosted surface stages on its next sync.
    pub(crate) fn stage(&self, scenario: Option<&str>) {
        self.stages.run(scenario);
    }

    /// Follow `permissions`: the resource is editable while they grant
    /// `write`.
    pub(crate) fn following(
        mut self,
        permissions: Option<LivePermissions>,
        write: PermissionScope,
    ) -> Self {
        self.permissions = permissions;
        self.write = Some(write);
        self
    }

    /// Apply a permission change made while the app runs: dispatchers stop
    /// (or resume) submitting, bound state and surfaces see the new value.
    fn refresh_access(&self) {
        if let (Some(permissions), Some(write)) = (&self.permissions, &self.write) {
            self.dispatch.set_read_only(!permissions.check(write));
        }
    }

    pub(crate) fn live(&self) -> &Live<R> {
        &self.live
    }

    /// Follow the document when it moved — a local action, a
    /// collaborator's change, an undo or a restore — reading only what the
    /// change touched.
    fn refresh(&self, doc: &mut AutoCommit) {
        if let Err(error) = self.live.sync(doc) {
            tracing::warn!(%error, kind = R::KIND, "resource unreadable");
        }
        // A surface may have synced the state itself after its own edit.
        let revision = self.live.revision();
        if revision != self.followed.get() {
            self.followed.set(revision);
            self.updaters.run(&self.live.get());
        }
    }
}

/// A resource's search entries and their index, keyed by position.
struct IndexedEntries {
    entries: Vec<SearchEntry>,
    index: SearchIndex,
}

impl IndexedEntries {
    fn new(entries: Vec<SearchEntry>) -> Self {
        // Entry titles are names a person half remembers: typos match too.
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(entries.iter().enumerate().map(|(at, entry)| {
            Document::new(at as u64)
                .field(entry.title.as_str(), FieldWeight::TITLE)
                .field(entry.text.as_str(), FieldWeight::BODY)
        }));
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index resource search entries");
        }
        Self { entries, index }
    }
}

/// A search provider over one snapshot of the resource's entries.
struct ResourceSearch {
    name: &'static str,
    indexed: Arc<IndexedEntries>,
}

impl SearchProvider for ResourceSearch {
    fn name(&self) -> &str {
        self.name
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, usize::MAX)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.indexed
            .index
            .search(query, Page::first(limit))
            .hits
            .into_iter()
            .filter_map(|hit| {
                let entry = self.indexed.entries.get(usize::try_from(hit.key).ok()?)?;
                Some(
                    SearchResult::new(
                        self.name,
                        entry.title.clone(),
                        entry.anchor.clone().unwrap_or_default(),
                    )
                    .with_preview(entry.text.clone())
                    .with_score(hit.rank.unwrap_or(0)),
                )
            })
            .collect()
    }
}

// The root is the app's view plus what the framework adds: it keeps the
// state current, publishes the resource, forwards edits and indexes search.
// Everything else — layout, input, traversal, operation admission — is the
// view's own, forwarded by the decorator.
impl<R: Resource> Decorator for ResourceRoot<R> {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui_core::doc::document(state, path);
        self.refresh_access();
        self.refresh(state);
        self.content.sync(state, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            ui_core::vello::peniko::Fill::NonZero,
            transform,
            theme.bg_app,
            None,
            &self.content.get_rect(),
        );
        self.content.draw(scene, transform, text, theme, presence);
    }

    // The host paints through this entry point; paint the background too.
    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui_core::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.content.get_rect()) {
            return;
        }
        Decorator::draw(self, scene, transform, text, theme, presence);
    }

    fn name(&self) -> &'static str {
        "ResourceApp"
    }

    // A screenshot harness reaches the root to stage a scenario's view state
    // (`framework::stage`).
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        Decorator::resources_for(self, None, doc)
    }

    // The resource is the same for every participant (an agent task sees and
    // invokes the same actions); only the content's tree is per participant.
    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        let mut resources = vec![tilo_resource::publish::<R>(
            self.dispatch.resource_id(),
            &self.label,
            &self.live.get(),
        )];
        resources.extend(self.content.resources_for(participant, doc));
        resources
    }

    // The undo boundary of a surface's immediate actions this event; the
    // host records one step per event and author.
    fn take_committed_action(&mut self) -> Option<Vec<ui_core::automerge::ChangeHash>> {
        self.dispatch.take_committed()
    }

    // A surface's step stays open for the actions that continue it (the
    // letters of a word), which report no boundary of their own.
    fn keeps_undo_step_open(&self) -> bool {
        self.dispatch.keeps_step_open()
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.dispatch
            .take()
            .or_else(|| self.content.take_control_invocation())
    }

    /// An edit this resource's dispatcher submitted was refused, or is held
    /// for the user's confirmation. Nothing changed in the document, so a
    /// surface that previewed the edit is re-presented the stored state; a
    /// confirmed edit then arrives as an ordinary document change.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui_core::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        if result.is_err() && address.resource == self.dispatch.resource_id() {
            self.live.represent();
        }
        self.content.on_control_invocation_result(address, result);
    }

    fn on_workspace_request_finished(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<(), String>,
    ) {
        self.content.on_workspace_request_finished(address, result);
    }

    fn joined_records(&self) -> Option<&ui_core::record::JoinedRecords> {
        self.content.joined_records()
    }

    fn room_index_changed(&mut self, index: &AutoCommit) {
        self.content.room_index_changed(index);
    }

    fn permissions_changed(&mut self, permissions: &ui_core::permissions::PermissionContext) {
        self.content.permissions_changed(permissions);
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.dispatch.has_pending() || self.content.has_pending_control_invocation()
    }

    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        let revision = self.live.revision();
        let indexed = {
            let mut cached = self.search.borrow_mut();
            match cached.as_ref() {
                Some((at, indexed)) if *at == revision => Arc::clone(indexed),
                _ => {
                    let indexed = Arc::new(IndexedEntries::new(R::search(&self.live.get())));
                    *cached = Some((revision, Arc::clone(&indexed)));
                    indexed
                }
            }
        };
        let mut providers: Vec<Box<dyn SearchProvider>> = vec![Box::new(ResourceSearch {
            name: R::LABEL,
            indexed,
        })];
        providers.extend(self.content.search_providers());
        providers
    }
}

impl<R: Resource> AppWidget for ResourceRoot<R> {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &str {
        self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        self.icon.clone()
    }

    fn permission_hint(&self) -> Option<AppKind> {
        None
    }

    fn record_save_status(&mut self, status: RecordSaveStatus) {
        self.saved.set(status);
    }
}
