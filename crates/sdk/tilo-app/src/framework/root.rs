//! The root widget of a framework app: keeps the resource state current,
//! publishes the resource and its actions, forwards edits to the host, and
//! indexes the resource for search.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use ui_core::widget::{AppModel, ModelPath};

use tilo_resource::{Resource, SearchEntry};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use ui_core::PresencePayload;
use ui_core::automerge::AutoCommit;
use ui_core::permissions::{LivePermissions, PermissionScope};
use ui_core::presence::{PresenceCtx, PresenceKey};
use ui_core::record::RecordSaveStatus;
use ui_core::search::{SearchProvider, SearchResult};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::widget::{AppKind, AppWidget, Decorator, Element, IconDescriptor, Widget};
use ui_core::{LocalState, Theme};

use super::dispatch::Dispatcher;
use super::effect::Effects;
use super::live::Live;
use super::rows::{find, join, merge, outside_document};
use super::scenario::Run;
use super::stage::Stages;
use super::surface::SurfacePublication;
use super::view::{Bindings, Updaters, ViewCx};

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
    feeds: super::history::HistoryFeeds,
    /// What the view was built for, and the fixtures a capture's views read.
    run: Run,
    /// The settings the view follows, re-read when their document changes.
    storage: Rc<super::settings::ViewStorage>,
    /// Edits the views bound to their local state.
    bindings: Bindings<R>,
    /// The effect handlers and projection drains the view registered.
    effects: Effects<R>,
    content: Element,
    /// View and effect operations of the app's presentation, attached to the
    /// published resource. Document specs are dropped.
    presentation: Vec<tilo_actions::ActionSpec>,
    /// The resource's search entries at one live revision, indexed.
    search: std::cell::RefCell<Option<(u64, Arc<IndexedEntries>)>>,
    /// The palette the view was last drawn in, which `render` draws it in.
    drawn_theme: Option<Theme>,
    /// Shapes text for `render`, which runs outside a frame.
    /// Kept across renders; unused on wasm where `render` is cfg'd out.
    #[allow(dead_code, reason = "read only by render, which is native-only")]
    render_text: Option<TextRenderer>,
    /// Draws the view over a past version, which `render` answers for a
    /// `version`. `None` in a view that is itself a past version.
    /// Used by native-only `render` / past-version draw; unused on wasm.
    #[allow(
        dead_code,
        reason = "used by native-only render / past-version draw; unused on wasm"
    )]
    past: Option<super::preview::DrawPast>,
}

/// The operation type of the framework's `render` view.
pub(crate) const RENDER_TYPE: &str = "tilo.app.render";

/// `render`: the app's whole view as a picture, which every framework app
/// answers, so an agent can see what a person sees through the same door it
/// acts through.
fn render_spec() -> tilo_actions::ActionSpec {
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "render",
        "See the app",
        "See the app",
        tilo_actions::OperationKind::View,
        RENDER_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "scale",
                tilo_actions::ParamType::Number,
                "Pixels per point, 0.5 to 2 (default 1)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "version",
                tilo_actions::ParamType::Text,
                "A past version: change id from history, or checkpoint name",
            )
            .nullable(),
        ],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                RENDER_TYPE,
                &serde_json::json!({ "scale": args.number("scale"), "version": args.text("version") }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: RENDER_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .requiring(tilo_actions::PermissionRequirement::Read)
    .with_description(
        "See the app's window as a picture: answers with the image and the revision of the document it shows.",
    );
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

/// The revision a picture shows: the document's heads.
#[allow(dead_code, reason = "called only by render, which is native-only")]
fn revision(doc: &mut AutoCommit) -> String {
    let mut heads: Vec<String> = doc
        .get_heads()
        .iter()
        .map(|head| head.to_string().chars().take(12).collect())
        .collect();
    heads.sort();
    heads.join("+")
}

impl<R: Resource> ResourceRoot<R> {
    /// A root over `content`, the view built in `cx`, whose handles the root
    /// keeps; `alive` is the scope `cx`'s bindings are tied to.
    pub(crate) fn new(
        title: &'static str,
        icon: IconDescriptor,
        label: String,
        cx: ViewCx<R>,
        alive: Rc<()>,
        content: Element,
    ) -> Self {
        Self {
            title,
            icon,
            label,
            live: cx.live,
            updaters: cx.updaters,
            followed: std::cell::Cell::new(0),
            _alive: alive,
            dispatch: cx.dispatch,
            saved: cx.saved,
            permissions: None,
            write: None,
            stages: cx.stages,
            feeds: cx.feeds,
            run: cx.run,
            storage: cx.storage,
            bindings: cx.bindings,
            effects: cx.effects,
            content,
            presentation: Vec::new(),
            search: std::cell::RefCell::new(None),
            drawn_theme: None,
            render_text: None,
            past: None,
        }
    }

    pub(crate) fn drawing_past(mut self, past: super::preview::DrawPast) -> Self {
        self.past = Some(past);
        self
    }

    pub(crate) fn with_presentation(mut self, operations: Vec<tilo_actions::ActionSpec>) -> Self {
        self.presentation = operations;
        self
    }

    /// Stage a screenshot scenario's view state: every view's
    /// [`on_stage`](super::ViewCx::on_stage) callback runs now, and every
    /// hosted surface stages on its next sync.
    /// Stage the views' state for `scenario`, and answer the collaborators
    /// the capture shows as the presence frames their windows would send.
    pub(crate) fn stage(
        &self,
        scenario: Option<&str>,
    ) -> Vec<(String, PresenceKey, PresencePayload)> {
        self.stages.run(scenario);
        self.run
            .collaborators()
            .iter()
            .map(|(name, at)| {
                let (key, payload) = at.presence::<R>(self.dispatch.app_id());
                (name.clone(), key, payload)
            })
            .collect()
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

    /// Every hosted surface of this resource in the view, in tree order.
    fn surfaces(&self, visit: &mut dyn FnMut(&dyn SurfacePublication<R>)) {
        fn walk<R: Resource>(
            widget: &dyn Widget,
            visit: &mut dyn FnMut(&dyn SurfacePublication<R>),
        ) {
            if let Some(surface) =
                ui_core::widget::capability::capability::<dyn SurfacePublication<R>>(widget)
            {
                visit(surface);
            }
            widget.for_each_child(&mut |child| walk(child, visit));
        }
        walk(self.content.as_ref(), visit);
    }

    /// The resource as published: its own actions and children, the edits
    /// views and surfaces bound to where the user is, the live rows surfaces
    /// show, and the framework's operations every app answers.
    fn publication(&self) -> tilo_actions::Resource {
        let state = self.live.get();
        let mut resource =
            tilo_resource::publish::<R>(self.dispatch.resource_id(), &self.label, &state);
        let mut bound = self.bindings.declarations(&state);
        let mut rows = Vec::new();
        self.surfaces(&mut |surface| {
            bound.extend(surface.bound_declarations());
            rows.extend(
                surface
                    .live_rows()
                    .into_iter()
                    .map(super::LiveRow::into_resource),
            );
        });
        for decl in bound {
            if !resource
                .actions
                .iter()
                .any(|existing| existing.id == decl.id)
            {
                resource = resource.with_action(tilo_resource::declaration_spec(&decl));
            }
        }
        merge(&mut resource.children, rows);
        for spec in &self.presentation {
            if spec.kind == tilo_actions::OperationKind::Document {
                continue;
            }
            if resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
            {
                continue;
            }
            resource = resource.with_action(spec.clone());
        }
        if cfg!(not(target_family = "wasm"))
            && !resource
                .actions
                .iter()
                .any(|existing| existing.id == "render")
        {
            resource = resource.with_action(render_spec());
        }
        for spec in super::history::specs() {
            if !resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
            {
                resource = resource.with_action(spec);
            }
        }
        for spec in self.effects.specs() {
            let shadowed = resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id);
            debug_assert!(!shadowed, "effect `{}` shadows an action", spec.id);
            if shadowed {
                tracing::warn!(effect = spec.id, "effect shadows an action; not published");
                continue;
            }
            resource = resource.with_action(spec);
        }
        resource
    }

    /// The dispatcher the root's views submit through, which the host drains.
    pub(crate) fn dispatcher(&self) -> &Dispatcher<R> {
        &self.dispatch
    }

    /// The view as a stored PNG, at `scale` pixels per point.
    #[cfg(not(target_family = "wasm"))]
    fn render(
        &mut self,
        payload: &[u8],
        doc: &mut AutoCommit,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let args: serde_json::Value =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let scale = args
            .get("scale")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        if !(0.5..=2.0).contains(&scale) {
            return Err("scale must be within 0.5..=2".into());
        }
        let version = args.get("version").and_then(serde_json::Value::as_str);
        let rect = self.content.get_rect();
        let side = |points: f64| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 1..=8192 pixels before the cast"
            )]
            let pixels = (points * scale).round().clamp(1.0, 8192.0) as u32;
            pixels
        };
        let (width, height) = (side(rect.width()), side(rect.height()));
        let theme = self.drawn_theme.clone().unwrap_or_else(Theme::dark);
        let text = self.render_text.get_or_insert_with(TextRenderer::new);
        let mut scene = Scene::new();
        let shown = if let Some(version) = version {
            let draw = self
                .past
                .ok_or_else(|| "this view cannot show another version".to_owned())?;
            let heads = ui_core::history_source::steps::resolve(doc, version)?;
            let past = doc
                .fork_at(&heads)
                .map_err(|error| format!("version {version} is not in this document: {error}"))?;
            let bounds = ui_core::vello::kurbo::Rect::new(0.0, 0.0, rect.width(), rect.height());
            let transform = Affine::scale(scale);
            scene.fill(
                ui_core::vello::peniko::Fill::NonZero,
                transform,
                theme.bg_app,
                None,
                &bounds,
            );
            draw(past, bounds, &mut scene, transform, &theme)?;
            super::history::version_name(&heads)
        } else {
            let transform = Affine::translate((-rect.x0, -rect.y0)).then_scale(scale);
            scene.fill(
                ui_core::vello::peniko::Fill::NonZero,
                transform,
                theme.bg_app,
                None,
                &rect,
            );
            self.content
                .draw(&mut scene, transform, text, &theme, &PresenceCtx::empty());
            revision(doc)
        };
        let mut offscreen = ui_core::offscreen::OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("no renderer for a {width}x{height} picture: {error}"))?;
        // A new renderer reserves room for a modest scene; a denser view
        // overflows its first frame, which then comes back blank. Answer
        // only a frame that kept all of its geometry.
        let rgba = offscreen.try_render_resilient(&scene, theme.bg_app)?;
        let png =
            ui_core::png::encode_rgba8(width, height, &rgba).map_err(|error| error.to_string())?;
        let hash = tilo_content::ContentStore::for_app()
            .put(&png)
            .map_err(|error| format!("the picture could not be stored: {error}"))?;
        let address = tilo_actions::content_resource(&hash)
            .ok_or_else(|| "the picture has no content address".to_owned())?;
        let mut value = serde_json::json!({
            "width": width,
            "height": height,
            "theme": theme.name,
            "revision": shown,
            "content": address,
        });
        // Where the live view sits in the app's window, in window pixels: a
        // point of the picture plus this is where a person (or the computer
        // tool, aimed at this app) clicks it.
        if version.is_none()
            && let Some(fields) = value.as_object_mut()
        {
            fields.insert(
                "window_origin".into(),
                serde_json::json!([rect.x0.round(), rect.y0.round()]),
            );
        }
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: vec![address],
        })
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
        self.feeds.refresh(doc);
        self.live
            .with(|state| self.effects.drain_projections(state, revision));
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
                let result = match &entry.action {
                    Some(action) => {
                        SearchResult::for_action(self.name, entry.title.clone(), action)
                    }
                    None => SearchResult::new(
                        self.name,
                        entry.title.clone(),
                        entry.anchor.clone().unwrap_or_default(),
                    ),
                };
                Some(
                    result
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
    ui_core::provides!(dyn ui_core::undo::CommittedActions, dyn ui_core::widget::controls::ActionControls, dyn ui_core::resources::PublishesResources, dyn ui_core::search::capability::SearchSource; inner = content);

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
        self.storage.refresh();
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
        if self
            .drawn_theme
            .as_ref()
            .is_none_or(|drawn| drawn.name != theme.name)
        {
            self.drawn_theme = Some(theme.clone());
        }
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

    // The resource is the same for every participant (an agent task sees and
    // invokes the same actions); only the content's tree is per participant.

    // The undo boundary of a surface's immediate actions this event; the
    // host records one step per event and author.

    fn take_action_revert(
        &mut self,
    ) -> Option<(tilo_actions::EncodedAction, tilo_actions::EncodedAction)> {
        self.dispatch.take_action_revert()
    }

    // A surface's step stays open for the actions that continue it (the
    // letters of a word), which report no boundary of their own.

    /// Resource and app presentation doors publish effect/view actions; their
    /// operation types must be in the retained handler table or publication
    /// withdraws the whole component surface.
    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = self.inner().resource_operation_handlers();
        if cfg!(not(target_family = "wasm")) {
            handlers.insert(RENDER_TYPE.to_owned());
        }
        handlers.insert(super::history::VIEW_TYPE.to_owned());
        handlers.extend(self.effects.operations().into_iter().map(str::to_owned));
        for spec in R::local_operations()
            .into_iter()
            .chain(self.presentation.iter().cloned())
        {
            if let Some(type_id) = spec.operation_type {
                handlers.insert(type_id);
            }
        }
        handlers
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ui_core::automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.effects
            .poll(invocation, cancellation_requested)
            .or_else(|| {
                self.content
                    .poll_resource_operation(invocation, cancellation_requested, doc, path)
            })
    }

    fn start_operation(
        &mut self,
        op: &ui_core::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ui_core::widget::ResourceOperationStart>, String> {
        #[cfg(not(target_family = "wasm"))]
        if let Some(request) = op.unscoped()
            && request.type_id == RENDER_TYPE
        {
            let output = self.render(request.payload, doc)?;
            return Ok(Some(ui_core::widget::ResourceOperationStart::Completed(
                output,
            )));
        }
        if let Some(request) = op.unscoped()
            && request.type_id == super::history::VIEW_TYPE
        {
            let output = super::history::answer(doc, request.payload)?;
            return Ok(Some(ui_core::widget::ResourceOperationStart::Completed(
                output,
            )));
        }
        if let Some(started) = self.effects.start(op)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.inner_mut().start_operation(op, doc, path)? {
            return Ok(Some(started));
        }
        // View openers from the resource/presentation catalogue (e.g.
        // `tilo.rich-text.view` editor/compact) are already mounted; accept
        // them so agents and Ctrl+K can switch without a missing-handler fault.
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind != tilo_actions::OperationKind::View {
            return Ok(None);
        }
        let known = R::local_operations()
            .into_iter()
            .chain(self.presentation.iter().cloned())
            .any(|spec| spec.operation_type.as_deref() == Some(request.type_id));
        if known {
            Ok(Some(ui_core::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        } else {
            Ok(None)
        }
    }
}

impl<R: Resource> ui_core::search::capability::SearchSource for ResourceRoot<R> {
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
        providers.extend(ui_core::search::capability::search_providers(
            self.content.as_ref(),
        ));
        providers
    }
}

impl<R: Resource> ui_core::resources::PublishesResources for ResourceRoot<R> {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui_core::resources::PublishesResources::resources_for(self, None, doc)
    }
    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        let mut resource = self.publication();
        // The content may add live external entities beside the resource,
        // never a document action: those are the resource's own. A hosted
        // surface has already admitted what it adds. One naming the resource
        // joins it rather than publishing it twice.
        let surface = ui_core::widget::capability::capability::<dyn SurfacePublication<R>>(
            self.content.as_ref(),
        )
        .is_some();
        let mut extras = Vec::new();
        for extra in ui_core::resources::resources_for(self.content.as_ref(), participant, doc) {
            let extra = if surface {
                extra
            } else {
                outside_document(extra)
            };
            if extra.id == resource.id {
                join(&mut resource, extra);
            } else {
                extras.push(extra);
            }
        }
        let mut resources = vec![resource];
        resources.append(&mut extras);
        resources
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        ui_core::resources::PublishesResources::resource_for(self, None, doc, id)
    }
    /// Resource `id` in the published tree, or one the tree leaves out: a
    /// child the resource resolves by id, or a live row a surface resolves.
    fn resource_for(
        &self,
        _participant: Option<&tilo_actions::ParticipantId>,
        _doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        if let Some(published) = find(&[self.publication()], id) {
            return Some(published);
        }
        if let Some(child) = R::child(&self.live.get(), id) {
            return Some(child);
        }
        let mut row = None;
        self.surfaces(&mut |surface| {
            if row.is_none() {
                row = surface.live_row(id).map(super::LiveRow::into_resource);
            }
        });
        row
    }
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value> {
        if address.resource == self.dispatch.resource_id()
            && super::history::is_edit(&address.action)
        {
            return super::history::result(&address.action, doc, changes);
        }
        ui_core::resources::document_action_result(self.content.as_ref(), doc, address, changes)
    }
}

impl<R: Resource> ui_core::widget::controls::ActionControls for ResourceRoot<R> {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.dispatch
            .take()
            .or_else(|| ui_core::widget::controls::take_control_invocation(self.content.as_mut()))
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
        // The resource itself, or a child its dispatcher edits through it.
        let ours = address.resource == self.dispatch.resource_id()
            || address
                .resource
                .strip_prefix(R::KIND)
                .is_some_and(|rest| rest.starts_with('/'));
        if let Err(error) = result
            && ours
        {
            if !matches!(error, tilo_actions::DoorError::ConfirmationRequired) {
                self.dispatch.refuse(&address.action, error.to_string());
            }
            self.live.represent();
        }
        ui_core::widget::controls::on_control_invocation_result(
            self.content.as_mut(),
            address,
            result,
        );
    }
    fn has_pending_control_invocation(&self) -> bool {
        self.dispatch.has_pending()
            || ui_core::widget::controls::has_pending_control_invocation(self.content.as_ref())
    }
}

impl<R: Resource> ui_core::undo::CommittedActions for ResourceRoot<R> {
    fn take_committed_action(&mut self) -> Option<Vec<ui_core::automerge::ChangeHash>> {
        self.dispatch.take_committed()
    }
    fn keeps_undo_step_open(&self) -> bool {
        self.dispatch.keeps_step_open()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_entry_naming_an_action_is_a_hit_that_invokes_it() {
        let open = tilo_actions::ActionAddress::new("notes.page:1", "open");
        let search = ResourceSearch {
            name: "Notes",
            indexed: Arc::new(IndexedEntries::new(vec![
                SearchEntry::new("Groceries", "milk and eggs").invoking(open.clone()),
                SearchEntry::new("Garden", "roses").anchored("page:2"),
            ])),
        };
        let groceries = search.search("groceries");
        assert_eq!(groceries.len(), 1);
        assert_eq!(groceries[0].action_address(), Some(open));
        let garden = search.search("garden");
        assert_eq!(garden.len(), 1);
        assert_eq!(garden[0].action_address(), None);
        assert_eq!(garden[0].action_id, "page:2");
    }
}
