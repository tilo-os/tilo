//! The root widget of a framework app: keeps the resource state current,
//! publishes the resource and its actions, forwards edits to the host, and
//! indexes the resource for search.

use std::rc::Rc;

use tilo_resource::{Resource, SearchEntry};
use ui_core::automerge::{AutoCommit, ChangeHash, ObjId};
use ui_core::permissions::{LivePermissions, PermissionScope};
use ui_core::presence::PresenceCtx;
use ui_core::record::RecordSaveStatus;
use ui_core::search::{SearchProvider, SearchResult, fuzzy_score};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::widget::{AppKind, AppWidget, Decorator, Element, IconDescriptor, Widget};
use ui_core::{LocalState, Theme};

use super::dispatch::Dispatcher;
use super::live::Live;
use super::view::Updaters;

pub(crate) struct ResourceRoot<R: Resource> {
    title: &'static str,
    icon: IconDescriptor,
    label: String,
    live: Live<R>,
    updaters: Updaters<R>,
    _alive: Rc<()>,
    dispatch: Dispatcher<R>,
    saved: LocalState<RecordSaveStatus>,
    heads: Vec<ChangeHash>,
    /// The participant's permissions, re-checked every sync so a role change
    /// while the app runs takes effect at once. `None` for a read-only view
    /// of a past version.
    permissions: Option<LivePermissions>,
    write: Option<PermissionScope>,
    content: Element,
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
        content: Element,
    ) -> Self {
        Self {
            title,
            icon,
            label,
            live,
            updaters,
            _alive: alive,
            dispatch,
            saved,
            heads: Vec::new(),
            permissions: None,
            write: None,
            content,
        }
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
    fn refresh_access(&mut self) {
        if let (Some(permissions), Some(write)) = (&self.permissions, &self.write) {
            self.dispatch.set_read_only(!permissions.check(write));
        }
    }

    /// Re-read the resource when the document moved: a local action, a
    /// collaborator's change, an undo or a restore.
    fn refresh(&mut self, doc: &mut AutoCommit) {
        let heads = doc.get_heads();
        if heads == self.heads {
            return;
        }
        self.heads = heads;
        match R::read(doc) {
            Ok(state) => {
                self.live.replace(state);
                self.updaters.run(&self.live.get());
            }
            Err(error) => tracing::warn!(%error, kind = R::KIND, "resource unreadable"),
        }
    }
}

/// A search provider over one snapshot of the resource's entries.
struct ResourceSearch {
    name: &'static str,
    entries: Vec<SearchEntry>,
}

impl SearchProvider for ResourceSearch {
    fn name(&self) -> &str {
        self.name
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, usize::MAX)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let mut hits: Vec<SearchResult> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let score =
                    fuzzy_score(query, &entry.title).or_else(|| fuzzy_score(query, &entry.text))?;
                let mut hit = SearchResult::new(
                    self.name,
                    entry.title.clone(),
                    entry.anchor.clone().unwrap_or_default(),
                )
                .with_preview(entry.text.clone());
                hit.score = score;
                Some(hit)
            })
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        hits.truncate(limit);
        hits
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

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
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

    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut resources = vec![tilo_resource::publish::<R>(
            self.dispatch.resource_id(),
            &self.label,
            &self.live.get(),
        )];
        resources.extend(self.content.resources(doc));
        resources
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

    fn has_pending_control_invocation(&self) -> bool {
        self.dispatch.has_pending() || self.content.has_pending_control_invocation()
    }

    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        let mut providers: Vec<Box<dyn SearchProvider>> = vec![Box::new(ResourceSearch {
            name: R::LABEL,
            entries: R::search(&self.live.get()),
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
