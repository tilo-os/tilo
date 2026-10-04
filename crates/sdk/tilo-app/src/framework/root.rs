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
use ui_core::application_view::{
    ApplicationFocusRestore, ApplicationLocalView, ApplicationViewState,
};
use ui_core::automerge::AutoCommit;
use ui_core::permissions::{LivePermissions, PermissionScope};
use ui_core::presence::{PresenceCtx, PresenceKey};
use ui_core::record::RecordSaveStatus;
use ui_core::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchCursor, SearchPage,
    SearchProvider, SearchResult,
};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::view::ThawError;
use ui_core::widget::capability::{capability, capability_mut};
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
    /// The app's view: the custom surface itself when the view is one.
    pub(crate) content: Element,
    /// View and effect operations of the app's presentation, attached to the
    /// published resource. Document specs are dropped.
    presentation: Vec<tilo_actions::ActionSpec>,
    /// The resource's and its surfaces' search entries, indexed.
    search: std::cell::RefCell<Option<SearchCache>>,
    /// The view operation types of the nested resources at one live revision.
    nested_views: std::cell::RefCell<Option<(u64, Rc<HashSet<String>>)>>,
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
            nested_views: std::cell::RefCell::new(None),
            drawn_theme: None,
            render_text: None,
            past: None,
        }
    }

    /// The person's own record of `P`, as the views open it.
    pub(crate) fn person<P: Resource>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.person::<P>()
    }

    /// The person's record of `P` that the app `owner` keeps.
    pub(crate) fn person_of<P: Resource>(
        &self,
        owner: &'static str,
    ) -> Option<super::PersonRecord<P>> {
        self.storage.person_of::<P>(owner)
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
    /// The first widget of type `T` in the view, whatever wraps it: a
    /// custom surface is found by its own type.
    pub(crate) fn find_mut<T: 'static>(&mut self) -> Option<&mut T> {
        fn find<T: 'static>(widget: &mut dyn Widget) -> Option<&mut T> {
            if widget.as_any_mut().is_some_and(|any| any.is::<T>()) {
                return widget.as_any_mut()?.downcast_mut::<T>();
            }
            widget.children_mut().into_iter().find_map(find::<T>)
        }
        find::<T>(self.content.as_mut())
    }

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
        if let Some(permissions) = &permissions {
            self.dispatch.follow(permissions.clone());
        }
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

    /// The view the root hosts, for a test reaching a custom surface in it.
    pub(crate) fn content_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    /// Open what a launch naming `arguments` opens in this window
    /// ([`ViewCx::open_here`](super::ViewCx::open_here)).
    pub(crate) fn open_here(
        &self,
        arguments: &[String],
    ) -> Result<ui_core::application::ReplacementOutcome, String> {
        self.run.open_here(arguments)
    }

    /// What the launch that opened the window named.
    pub(crate) fn launch(&self) -> super::launch::LaunchInput<'_> {
        self.run.launch()
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
        self.publication_of(&self.live.get())
    }

    /// [`Self::publication`] of the resource as `doc` holds it now, which a
    /// change from another window or participant may have moved past what
    /// this view last synced.
    fn current_publication(&self, doc: &AutoCommit) -> tilo_actions::Resource {
        if !self.live.follows(doc)
            && let Ok(current) = R::read(doc)
        {
            return self.publication_of(&current);
        }
        self.publication()
    }

    fn publication_of(&self, state: &R::State) -> tilo_actions::Resource {
        let mut resource =
            tilo_resource::publish::<R>(self.dispatch.resource_id(), &self.label, state);
        let mut bound = self.bindings.declarations(state);
        let mut rows = Vec::new();
        let mut availability = Vec::new();
        self.surfaces(&mut |surface| {
            bound.extend(surface.bound_declarations());
            availability.extend(surface.availability());
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
        for status in &availability {
            for spec in resource
                .actions
                .iter_mut()
                .filter(|spec| spec.id == status.id())
            {
                status.overlay(spec);
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
        resource.children.extend(self.storage.person_resources());
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

    /// The operation types of the views the resource's nested resources
    /// publish (the editor of a note's rich-text body): presentations of the
    /// resource this root has mounted, answered like its own.
    fn nested_views(&self) -> Rc<HashSet<String>> {
        fn collect(resources: &[tilo_actions::Resource], views: &mut HashSet<String>) {
            for resource in resources {
                views.extend(
                    resource
                        .actions
                        .iter()
                        .filter(|spec| spec.kind == tilo_actions::OperationKind::View)
                        .filter_map(|spec| spec.operation_type.clone()),
                );
                collect(&resource.children, views);
            }
        }
        let revision = self.live.revision();
        let mut cached = self.nested_views.borrow_mut();
        match cached.as_ref() {
            Some((at, views)) if *at == revision => Rc::clone(views),
            _ => {
                let mut views = HashSet::new();
                collect(&R::children(&self.live.get()), &mut views);
                let views = Rc::new(views);
                *cached = Some((revision, Rc::clone(&views)));
                views
            }
        }
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
        self.effects.poll_jobs();
    }
}

/// The index of the resource's search entries and its surfaces', as of one
/// revision of the resource and one set of surface entries.
struct SearchCache {
    revision: u64,
    shown: Vec<SearchEntry>,
    indexed: Arc<IndexedEntries>,
    /// An index of newer entries being built away from the UI thread: the
    /// revision and surface entries it is of, and where it arrives.
    building: Option<(
        u64,
        Vec<SearchEntry>,
        std::sync::mpsc::Receiver<IndexedEntries>,
    )>,
}

/// A [`SearchText`](tilo_resource::SearchText), as the index reads it.
struct BodyText(Arc<dyn tilo_resource::SearchText>);

impl tilo_search::BodySource for BodyText {
    fn segment_count(&self) -> usize {
        self.0.segment_count()
    }

    fn piece_count(&self, segment: usize) -> usize {
        self.0.piece_count(segment)
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<tilo_search::BodyPiece<'_>> {
        self.0
            .piece(segment, piece)
            .map(|piece| tilo_search::BodyPiece {
                text: piece.text,
                space_before: piece.space_before,
            })
    }

    fn locates(&self, segment: usize) -> bool {
        self.0.locates(segment)
    }
}

/// A resource's search entries and their index, keyed by position.
struct IndexedEntries {
    entries: Vec<SearchEntry>,
    index: SearchIndex,
    /// Which build of the entries this is: a page cursor from another build
    /// starts over rather than skipping or repeating.
    generation: u64,
}

impl IndexedEntries {
    fn new(entries: Vec<SearchEntry>) -> Self {
        Self::built(entries, 0)
    }

    fn built(entries: Vec<SearchEntry>, generation: u64) -> Self {
        // Entry titles are names a person half remembers: typos match too.
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(entries.iter().enumerate().map(|(at, entry)| {
            let document = Document::new(at as u64)
                .field(entry.title.as_str(), FieldWeight::TITLE)
                .field(entry.text.as_str(), FieldWeight::BODY)
                .field(
                    entry.subtitle.as_deref().unwrap_or_default(),
                    FieldWeight::BODY,
                );
            match &entry.body {
                Some(body) => document.body(Arc::new(BodyText(body.shared()))),
                None => document,
            }
        }));
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index resource search entries");
        }
        Self {
            entries,
            index,
            generation,
        }
    }
}

/// A search provider over one snapshot of the resource's entries.
pub(crate) struct ResourceSearch {
    name: &'static str,
    indexed: Arc<IndexedEntries>,
}

impl ResourceSearch {
    /// The search source over `entries`, listed under `name` unless an
    /// entry names its own group.
    pub(crate) fn over(name: &'static str, entries: Vec<SearchEntry>) -> Self {
        Self {
            name,
            indexed: Arc::new(IndexedEntries::new(entries)),
        }
    }
}

impl SearchProvider for ResourceSearch {
    fn name(&self) -> &str {
        self.name
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    // No caller materialises more than one interactive page in one turn;
    // complete discovery goes through `search_page`'s cursor.
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.results(query, Page::first(limit.min(DEFAULT_SEARCH_RESULT_LIMIT)))
            .0
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.indexed.generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        cursor.opaque_parts().0 == self.indexed.generation
    }

    // Every entry stays findable however many there are: a page continues
    // where the last one stopped, within the caller's budgets.
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let limit = work_budget
            .min(DEFAULT_SEARCH_CANDIDATE_LIMIT)
            .min(result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT));
        let folded = ui_core::search::normalize_search_text(query);
        let offset = cursor
            .map(SearchCursor::opaque_parts)
            .filter(|(generation, at_query, _, _)| {
                *generation == self.indexed.generation && *at_query == folded
            })
            .and_then(|(_, _, _, offset)| usize::try_from(offset?).ok())
            .unwrap_or(0);
        if limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let (results, more) = self.results(query, Page { offset, limit });
        let inspected = results.len();
        let next = more.then(|| {
            SearchCursor::opaque(
                self.indexed.generation,
                folded,
                None,
                u64::try_from(offset.saturating_add(inspected)).ok(),
            )
        });
        SearchPage {
            results,
            next,
            inspected,
        }
    }
}

impl ResourceSearch {
    /// One page of hits for `query`, and whether more follow it.
    fn results(&self, query: &str, page: Page) -> (Vec<SearchResult>, bool) {
        let found = self.indexed.index.search(query, page);
        let results = found
            .hits
            .into_iter()
            .filter_map(|hit| {
                let parent = self.indexed.entries.get(usize::try_from(hit.key).ok()?)?;
                // A match in a located segment of a body is that location's
                // hit, listed with its entry unless it names its own group.
                let located = hit.segment.and_then(|segment| {
                    let mut location = parent.body.as_ref()?.text().location(segment)?;
                    if location.group.is_none() {
                        location.group.clone_from(&parent.group);
                    }
                    Some(location)
                });
                let entry = located.as_ref().unwrap_or(parent);
                let group = entry.group.as_deref().unwrap_or(self.name);
                let mut result = match &entry.action {
                    Some(action) => SearchResult::for_action(group, entry.title.clone(), action),
                    None => SearchResult::new(
                        group,
                        entry.title.clone(),
                        entry.anchor.clone().unwrap_or_default(),
                    ),
                };
                if let Some(subtitle) = &entry.subtitle {
                    result = result.with_subtitle(subtitle.clone());
                }
                if let Some(icon) = entry
                    .icon
                    .as_deref()
                    .and_then(ui_core::icons::Icon::from_name)
                {
                    result = result.with_icon(icon);
                }
                if !entry.text.is_empty() {
                    result = result.with_preview(entry.text.clone());
                }
                Some(result.with_score(hit.rank.unwrap_or(0).saturating_add(entry.boost)))
            })
            .collect();
        (results, found.more)
    }
}

// The root is the app's view plus what the framework adds: it keeps the
// state current, publishes the resource, forwards edits and indexes search.
// Everything else — layout, input, traversal, operation admission — is the
// view's own, forwarded by the decorator.
impl<R: Resource> Decorator for ResourceRoot<R> {
    ui_core::provides!(dyn ui_core::undo::CommittedActions, dyn ui_core::widget::controls::ActionControls, dyn ui_core::resources::PublishesResources, dyn ui_core::search::capability::SearchSource, dyn ui_core::history_mode::capability::DocumentHistory; inner = content);

    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn record_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        let mut owners = self.storage.person_owners();
        owners.extend(self.content.record_owners());
        owners
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
        self.run.people.follow(presence);
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
        handlers.insert(super::person::HISTORY_TYPE.to_owned());
        handlers.extend(self.effects.operations().into_iter().map(str::to_owned));
        for spec in R::local_operations()
            .into_iter()
            .chain(self.presentation.iter().cloned())
        {
            if let Some(type_id) = spec.operation_type {
                handlers.insert(type_id);
            }
        }
        handlers.extend(self.nested_views().iter().cloned());
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
        if let Some(request) = op.unscoped()
            && request.type_id == super::person::HISTORY_TYPE
        {
            let output = self.storage.person_history(request.payload)?;
            return Ok(Some(ui_core::widget::ResourceOperationStart::Completed(
                output,
            )));
        }
        // An effect reads the resource as the document stands, though no
        // frame has run since a collaborator's change arrived.
        if let Err(error) = self.live.sync(doc) {
            tracing::warn!(%error, kind = R::KIND, "resource unreadable");
        }
        if let Some(started) = self.effects.start(op)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.inner_mut().start_operation(op, doc, path)? {
            return Ok(Some(started));
        }
        // View openers from the resource/presentation catalogue and from its
        // nested resources (e.g. a rich-text body's `tilo.rich-text.view`
        // editor/compact) are already mounted; accept them so agents and
        // Ctrl+K can switch without a missing-handler fault.
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind != tilo_actions::OperationKind::View {
            return Ok(None);
        }
        let known = R::local_operations()
            .into_iter()
            .chain(self.presentation.iter().cloned())
            .any(|spec| spec.operation_type.as_deref() == Some(request.type_id))
            || self.nested_views().contains(request.type_id);
        if known {
            Ok(Some(ui_core::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        } else {
            Ok(None)
        }
    }
}

impl<R: Resource> ResourceRoot<R> {
    /// The index of the resource's entries at `revision` and the surfaces'
    /// `shown` ones. Entries with prose bodies (whole books) are indexed
    /// away from the UI thread: until that index arrives, search answers
    /// from the one before it.
    fn current_search_index(&self, revision: u64, shown: Vec<SearchEntry>) -> Arc<IndexedEntries> {
        let mut cached = self.search.borrow_mut();
        if let Some(cache) = cached.as_mut()
            && let Some((at, entries_shown, arriving)) = cache.building.take()
        {
            match arriving.try_recv() {
                Ok(indexed) => {
                    cache.revision = at;
                    cache.shown = entries_shown;
                    cache.indexed = Arc::new(indexed);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    cache.building = Some((at, entries_shown, arriving));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if let Some(cache) = cached.as_ref() {
            let current = cache.revision == revision && cache.shown == shown;
            let arriving = cache
                .building
                .as_ref()
                .is_some_and(|(at, building, _)| *at == revision && *building == shown);
            if current || arriving {
                return Arc::clone(&cache.indexed);
            }
        }
        let generation = cached
            .as_ref()
            .map_or(0, |cache| cache.indexed.generation.wrapping_add(1));
        let mut entries = R::search(&self.live.get());
        entries.extend(shown.iter().cloned());
        #[cfg(not(target_arch = "wasm32"))]
        if entries.iter().any(|entry| entry.body.is_some()) {
            let (send, arriving) = std::sync::mpsc::channel();
            let building = entries.clone();
            let spawned = std::thread::Builder::new()
                .name("resource-search-index".into())
                .spawn(move || {
                    let _ = send.send(IndexedEntries::built(building, generation));
                });
            if spawned.is_ok() {
                let cache = cached.get_or_insert_with(|| SearchCache {
                    revision,
                    shown: Vec::new(),
                    indexed: Arc::new(IndexedEntries::built(Vec::new(), generation)),
                    building: None,
                });
                cache.building = Some((revision, shown, arriving));
                return Arc::clone(&cache.indexed);
            }
        }
        let indexed = Arc::new(IndexedEntries::built(entries, generation));
        *cached = Some(SearchCache {
            revision,
            shown,
            indexed: Arc::clone(&indexed),
            building: None,
        });
        indexed
    }
}

impl<R: Resource> ui_core::search::capability::SearchSource for ResourceRoot<R> {
    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        // The resource's entries change with its revision; what the surfaces
        // show (a page's text, live rows) changes on their own, so their
        // entries are read now and the index rebuilt only when either moved.
        let revision = self.live.revision();
        let mut shown = Vec::new();
        self.surfaces(&mut |surface| shown.extend(surface.search_entries()));
        let indexed = self.current_search_index(revision, shown);
        // The app's one search source: what its content widgets show beside
        // the resource reaches it as their surfaces' entries, never as a
        // provider of their own.
        vec![Box::new(ResourceSearch {
            name: R::LABEL,
            indexed,
        })]
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
        let mut resource = self.current_publication(doc);
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
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        // A change from another window or participant may not have reached
        // this view yet: an item it removed is gone, one it made is there.
        if let Some(published) = find(&[self.current_publication(doc)], id) {
            return Some(published);
        }
        let current = if self.live.follows(doc) {
            None
        } else {
            R::read(doc).ok()
        };
        let child = match &current {
            Some(current) => R::child(current, id),
            None => R::child(&self.live.get(), id),
        };
        if child.is_some() {
            return child;
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
        if address.resource == self.dispatch.resource_id() {
            if super::history::is_edit(&address.action) {
                return super::history::result(&address.action, doc, changes);
            }
            if let Some(answer) = R::answer(doc, &address.action, changes) {
                return Some(answer);
            }
        }
        ui_core::resources::document_action_result(self.content.as_ref(), doc, address, changes)
    }
}

impl<R: Resource> ui_core::widget::controls::ActionControls for ResourceRoot<R> {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }
    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<ui_core::widget::controls::AttributedControlInvocation> {
        self.dispatch.take_attributed().or_else(|| {
            ui_core::widget::controls::take_attributed_control_invocation(self.content.as_mut())
        })
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

// The root owns the file whose history it shows beside the resource's (the
// runner checks the source it registered is this root's); everything else
// about History is its content's, as before.
impl<R: Resource> ui_core::history_mode::capability::DocumentHistory for ResourceRoot<R> {
    fn history_registration(
        &self,
    ) -> Result<
        Option<ui_core::history_mode::HistoryRegistration>,
        ui_core::history_mode::HistoryRegistrationError,
    > {
        match &self.run.file_history {
            Some((source, definition)) => {
                let scopes = definition.scopes.iter().map(|scope| {
                    ui_core::history_mode::HistorySourceRegistration::CheckedFiles {
                        scope: scope.key,
                        source: source.clone(),
                    }
                });
                ui_core::history_mode::HistoryRegistration::new(definition, scopes.collect())
                    .map(Some)
            }
            None => ui_core::history_mode::capability::history_registration(self.content.as_ref()),
        }
    }

    fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
        ui_core::widget::capability::capability::<
            dyn ui_core::history_mode::capability::DocumentHistory,
        >(self.content.as_ref())
        .map_or(Ok(()), |content| content.history_restore_readiness(scope))
    }

    fn document_history_definition(
        &self,
    ) -> Option<&'static ui_core::history_mode::HistoryDefinition> {
        ui_core::history_mode::capability::document_history_definition(self.content.as_ref())
    }

    fn provides_document_history(&self) -> bool {
        ui_core::widget::capability::capability::<
            dyn ui_core::history_mode::capability::DocumentHistory,
        >(self.content.as_ref())
        .is_some_and(ui_core::history_mode::capability::DocumentHistory::provides_document_history)
    }

    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[ui_core::automerge::ChangeHash],
        doc: &ui_core::automerge::AutoCommit,
        path: &ui_core::automerge::ObjId,
    ) {
        if let Some(content) = ui_core::widget::capability::capability_mut::<
            dyn ui_core::history_mode::capability::DocumentHistory,
        >(self.content.as_mut())
        {
            content.document_action_applied(action, before, doc, path);
        }
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

    // A surface that keeps presentation of its own across restarts (the
    // open item, the caret) saves it as the window's.
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        capability::<dyn ApplicationViewState>(self.content.as_ref())
            .map_or(Ok(None), ApplicationViewState::capture_application_view)
    }

    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        doc: &AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        match capability_mut::<dyn ApplicationViewState>(self.content.as_mut()) {
            Some(view) => view.restore_application_view(local, doc),
            None => Err(ThawError::State(
                "application-local restoration unsupported".into(),
            )),
        }
    }

    fn finish_application_view_restore(&mut self) -> Result<(), ThawError> {
        capability_mut::<dyn ApplicationViewState>(self.content.as_mut()).map_or(
            Ok(()),
            ApplicationViewState::finish_application_view_restore,
        )
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

    #[test]
    fn a_search_entry_reads_as_its_group_subtitle_and_icon() {
        let reveal = tilo_actions::ActionAddress::new("video.marker:1", "reveal");
        let search = ResourceSearch {
            name: "Project",
            indexed: Arc::new(IndexedEntries::new(vec![
                SearchEntry::new("Sunset", "the last light")
                    .invoking(reveal)
                    .grouped("Timeline markers")
                    .subtitled("0:21 · Drone")
                    .with_icon("star")
                    .boosted(5),
                SearchEntry::new("Sunset take", ""),
            ])),
        };
        let hits = search.search("sunset");
        let marker = hits
            .iter()
            .find(|hit| hit.title == "Sunset")
            .expect("the marker is found");
        assert_eq!(marker.kind, "Timeline markers");
        assert_eq!(marker.subtitle.as_deref(), Some("0:21 · Drone"));
        assert_eq!(marker.icon, Some(ui_core::icons::Icon::Star));
        let plain = hits
            .iter()
            .find(|hit| hit.title == "Sunset take")
            .expect("the plain entry is found");
        assert_eq!(plain.kind, "Project", "an ungrouped hit is the resource's");
        assert!(plain.preview.is_none(), "no text, no preview");
        // Subtitles are searched too: a marker is found by the shot it is on.
        assert!(
            search
                .search("drone")
                .iter()
                .any(|hit| hit.title == "Sunset")
        );
    }

    #[test]
    fn every_entry_stays_findable_page_by_page() {
        let search = ResourceSearch::over(
            "Tracks",
            (0..257)
                .map(|n| SearchEntry::new(format!("Common track {n}"), ""))
                .collect(),
        );
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = search.search_page("common", cursor.as_ref(), 17, 5);
            assert!(page.results.len() <= 5);
            for hit in &page.results {
                assert!(seen.insert(hit.title.clone()), "{} came twice", hit.title);
            }
            let Some(next) = page.next else { break };
            assert!(search.action_directory_cursor_is_current(&next));
            cursor = Some(next);
        }
        assert_eq!(seen.len(), 257, "every match is reachable");
    }

    #[test]
    fn no_caller_materialises_more_than_one_page_at_once() {
        let search = ResourceSearch::over(
            "Tables",
            (0..300)
                .map(|n| SearchEntry::new(format!("table {n}"), ""))
                .collect(),
        );
        assert_eq!(
            search.search_limited("", usize::MAX).len(),
            DEFAULT_SEARCH_RESULT_LIMIT
        );
        assert_eq!(
            search
                .search_page("", None, usize::MAX, usize::MAX)
                .results
                .len(),
            DEFAULT_SEARCH_RESULT_LIMIT
        );
    }

    /// A chapter of a book: one paragraph per segment, each opening the
    /// book where it is.
    struct Chapter(Vec<&'static str>);

    impl tilo_resource::SearchText for Chapter {
        fn segment_count(&self) -> usize {
            self.0.len()
        }

        fn piece_count(&self, segment: usize) -> usize {
            usize::from(segment < self.0.len())
        }

        fn piece(&self, segment: usize, piece: usize) -> Option<tilo_resource::SearchPiece<'_>> {
            let text = self.0.get(segment)?;
            (piece == 0).then_some(tilo_resource::SearchPiece {
                text,
                space_before: segment > 0,
            })
        }

        fn locates(&self, _segment: usize) -> bool {
            true
        }

        fn location(&self, segment: usize) -> Option<SearchEntry> {
            Some(
                SearchEntry::new(format!("Paragraph {}", segment.saturating_add(1)), "").invoking(
                    tilo_actions::ActionAddress::new(format!("book.location:{segment}"), "open"),
                ),
            )
        }
    }

    #[test]
    fn a_match_in_a_body_is_the_place_it_is_in() {
        let body = tilo_resource::SearchBody::new(Arc::new(Chapter(vec![
            "It was a dark and stormy night.",
            "The lighthouse keeper counted the waves.",
        ])));
        let search = ResourceSearch::over(
            "Library",
            vec![
                SearchEntry::new("Storm Tales", "")
                    .invoking(tilo_actions::ActionAddress::new("book:1", "open"))
                    .grouped("Books")
                    .with_body(body.clone()),
            ],
        );
        let hits = search.search("lighthouse keeper");
        let place = hits
            .iter()
            .find(|hit| hit.title == "Paragraph 2")
            .expect("the paragraph the words are in is a hit");
        assert_eq!(
            place.action_address(),
            Some(tilo_actions::ActionAddress::new("book.location:1", "open"))
        );
        assert_eq!(place.kind, "Books", "a location is listed with its book");
        assert!(
            search
                .search("storm tales")
                .iter()
                .any(|hit| hit.title == "Storm Tales"),
            "the book itself is found by its title"
        );
        assert_eq!(body, body.clone(), "a body is the text it shares");
    }
}
