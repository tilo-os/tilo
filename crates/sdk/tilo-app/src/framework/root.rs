//! The root widget of a framework app: keeps the resource state current,
//! publishes the resource and its actions, forwards edits to the host, and
//! indexes the resource for search.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_resource::{Resource, SearchEntry};
use ui_core::automerge::{AutoCommit, ChangeHash, ObjId};
use ui_core::presence::PresenceCtx;
use ui_core::record::RecordSaveStatus;
use ui_core::search::{SearchProvider, SearchResult, fuzzy_score};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::widget::{AppKind, AppWidget, Element, IconDescriptor, Widget};
use ui_core::winit::event::{Modifiers, WindowEvent};
use ui_core::{LocalState, Theme, taffy};

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
            content,
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

impl<R: Resource> Widget for ResourceRoot<R> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.refresh(state);
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.content.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.content.apply_layout(taffy, node, origin);
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
            &self.get_rect(),
        );
        self.content.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.content.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, taffy,
        );
    }

    // A transparent container forwards document-capable operation admission
    // to the handlers it hosts (see the toolkit's `Widget` docs).
    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui_core::widget::ResourceOperationStart>, String> {
        for child in self.children_mut() {
            if let Some(started) = child.start_resource_operation_with_document(
                invocation, kind, type_id, payload, doc, path,
            )? {
                return Ok(Some(started));
            }
        }
        Ok(None)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.content.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.content.as_mut()]
    }

    fn name(&self) -> &'static str {
        "ResourceApp"
    }

    fn get_rect(&self) -> Rect {
        self.content.get_rect()
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![tilo_resource::publish::<R>(
            self.dispatch.resource_id(),
            &self.label,
            &self.live.get(),
        )]
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

    fn native_input_snapshot(&self, doc: &AutoCommit, path: &ObjId) -> Option<serde_json::Value> {
        self.content.native_input_snapshot(doc, path)
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
