//! The root widget of a framework app: keeps the resource state current,
//! publishes the resource and its actions, forwards edits to the host, and
//! indexes the resource for search.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use ui_core::widget::{AppModel, ModelPath};

use crate::runner::with_native_presentations;
use tilo_resource::{Resource, ScopeDecl, SearchEntry};
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
    SearchProvider, SearchQuery, SearchResult,
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
use super::reactive::{Change, Updaters};
use super::rows::{find, join, merge, outside_document};
use super::scenario::Run;
use super::stage::Stages;
use super::surface::SurfacePublication;
use super::view::{Bindings, ViewCx};

mod shared_tree;
mod widget;
use shared_tree::{ItemChildren, NestedViews, PublishedChildren, SharedTreeCache};
pub(crate) use widget::{RootApp, RootControls, RootModel, RootPublishes, RootSearch, RootWidget};

pub(crate) struct ResourceRoot<R: Resource> {
    title: &'static str,
    icon: IconDescriptor,
    label: String,
    live: Live<R>,
    updaters: Updaters<R>,
    action_bindings: super::controls::Actions<R>,
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
    pub(crate) run: Run,
    /// The settings the view follows, re-read when their document changes.
    pub(crate) storage: Rc<super::settings::ViewStorage>,
    /// Edits the views bound to their local state.
    bindings: Bindings<R>,
    /// The effect handlers and projection drains the view registered.
    pub(super) effects: Effects<R>,
    /// The app's view: the custom surface itself when the view is one.
    pub(crate) content: Element,
    /// What is shown instead of the view while the record does not read as
    /// the resource, and why.
    unreadable: Option<(String, Element)>,
    /// The view was swapped for the unreadable state or back since layout.
    swapped: bool,
    /// View and effect operations of the app's presentation, attached to the
    /// published resource. Document specs are dropped.
    presentation: Vec<tilo_actions::ActionSpec>,
    /// The resource's and its surfaces' search entries, indexed.
    search: std::cell::RefCell<Option<SearchCache>>,
    /// The document-owned tree at one live and translation revision, shared
    /// with the last one wherever the change left it as it was. Surface
    /// bindings, local rows and effect availability are decorated afresh
    /// for every caller.
    publication_tree: std::cell::RefCell<Option<SharedTreeCache>>,
    /// The item children last published, kept so the next tree builds only
    /// the items whose label changed.
    item_children: std::cell::RefCell<Option<ItemChildren>>,
    /// The document children as the shared publication last carried them.
    published_children: std::cell::RefCell<Option<PublishedChildren>>,
    /// The head bound controls were last refreshed from, kept between syncs
    /// for a resource whose publication may be reused
    /// ([`Resource::publication_cache_epoch`]).
    binding_head: std::cell::RefCell<Option<BindingHead>>,
    /// The view operation types of the nested resources at one live revision.
    nested_views: std::cell::RefCell<Option<NestedViews>>,
    /// Whether the resource keeps document state History shows.
    history: bool,
    /// Platform presentation actions; the browser implementation has no state.
    native_presentation: crate::runner::Presentation,
}

impl<R: Resource> ui_core::component_lifecycle::WindowActivation for ResourceRoot<R> {
    fn take_window_activation_intent(&self) -> bool {
        self.native_presentation.take_window_activation_intent()
    }
}

/// The operation type of the framework's `open` view: show the item at an
/// anchor, as picking a search hit does.
pub(crate) const OPEN_TYPE: &str = "tilo.app.open";

/// The parameter of `open` naming what to show.
const OPEN_ANCHOR: &str = "anchor";

/// `open`: show the item at an anchor (a search entry's anchor). Published
/// when a view shows items ([`ViewCx::on_open`](super::ViewCx::on_open)), so
/// an agent opens the app where a person's search hit would.
fn open_spec() -> tilo_actions::ActionSpec {
    tilo_actions::ActionSpec::typed_operation(
        "open",
        "Open",
        "Item to open",
        tilo_actions::OperationKind::View,
        OPEN_TYPE,
        vec![tilo_actions::ActionParam::required(
            OPEN_ANCHOR,
            tilo_actions::ParamType::Text,
            "Item to open: the anchor of a search hit",
        )],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                OPEN_TYPE,
                &serde_json::json!({ OPEN_ANCHOR: args.text(OPEN_ANCHOR) }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: OPEN_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description("Open the app at one item: the anchor of a search hit.")
}

/// `open` on one item resource: show that item, as its search hit would.
fn item_open_spec(anchor: &str) -> tilo_actions::ActionSpec {
    let encoded = tilo_actions::EncodedAction::encode_operation(
        OPEN_TYPE,
        &serde_json::json!({ OPEN_ANCHOR: anchor }),
    )
    .unwrap_or_else(|_| tilo_actions::EncodedAction {
        type_id: OPEN_TYPE.into(),
        payload: Vec::new(),
    });
    tilo_actions::ActionSpec::typed_operation(
        "open",
        "Open",
        "Open",
        tilo_actions::OperationKind::View,
        OPEN_TYPE,
        Vec::new(),
        move |_| encoded.clone(),
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description("Show this item.")
}

/// Offer `open` on every item resource under `resource` (an item of `R`'s
/// collections), bound to that item.
fn offer_item_open<R: Resource>(resource: &mut tilo_actions::Resource) {
    if tilo_resource::items::item_of_resource::<R>(resource.id.as_str()).is_some()
        && !resource
            .actions
            .iter()
            .any(|existing| existing.id == "open")
    {
        let spec = item_open_spec(resource.id.as_str());
        resource.actions.push(spec);
    }
    for child in &mut resource.children {
        offer_item_open::<R>(child);
    }
}

/// The ids of the actions the framework publishes on every app's resource
/// beside the app's own: the generic `apply`, `group` and `field-edit`, the
/// views `open` and `read`, history, and the presentation operations. An app
/// verb, effect or view under one of these ids shadows the framework's or is
/// shadowed by it, so `tilo-app check` refuses it.
#[must_use]
pub fn reserved_action_ids() -> Vec<String> {
    let mut ids: Vec<String> = [
        tilo_resource::APPLY,
        tilo_resource::GROUP,
        tilo_resource::FIELD_EDIT,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ids.push(open_spec().id);
    ids.push(read_spec().id);
    ids.extend(super::history::specs().into_iter().map(|spec| spec.id));
    ids.extend(
        with_native_presentations(tilo_actions::Resource::new("", "", ""))
            .actions
            .into_iter()
            .map(|spec| spec.id),
    );
    ids.sort();
    ids.dedup();
    ids
}

/// The operation type of the framework's `read` view.
pub(crate) const READ_TYPE: &str = "tilo.resource.read";

/// `read`: what a [readable](Resource::READABLE) resource holds, as typed
/// data. With no `item` it answers the resource itself; with an `item` id, the
/// nested resource of that id. It is a view: it changes nothing, needs only
/// read permission and is checked like every other action.
fn read_spec() -> tilo_actions::ActionSpec {
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "read",
        "Read the content",
        "Read the content",
        tilo_actions::OperationKind::View,
        READ_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "item",
                tilo_actions::ParamType::Text,
                "A nested resource's id, as the first read lists it (default: the whole resource)",
            )
            .nullable(),
        ],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                READ_TYPE,
                &serde_json::json!({ "item": args.text("item") }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: READ_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "What this holds as typed data: with no item, the resource and the ids of what it contains; with an item id, that item's content.",
    );
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.reads_document()
}

/// Answer a `read` request against `R`'s committed document.
fn read<R: Resource>(
    state: &R::State,
    doc: &AutoCommit,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    let args: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
    };
    let item = args.get("item").and_then(serde_json::Value::as_str);
    Ok(tilo_actions::ActionOutput {
        value: Some(R::read_content(state, doc, item)?),
        resources: Vec::new(),
    })
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
        let native_presentation = crate::runner::Presentation::new(&cx);
        cx.dispatch.name_resource(&label);
        let ViewCx {
            env,
            updaters,
            action_bindings,
            stages,
            feeds,
            bindings,
            ..
        } = cx;
        Self {
            title,
            icon,
            label,
            live: env.live,
            updaters,
            action_bindings,
            followed: std::cell::Cell::new(0),
            _alive: alive,
            dispatch: env.dispatch,
            saved: env.saved,
            permissions: None,
            write: None,
            stages,
            feeds,
            run: env.run,
            storage: env.storage,
            bindings,
            effects: env.effects,
            content,
            unreadable: None,
            swapped: false,
            presentation: Vec::new(),
            search: std::cell::RefCell::new(None),
            publication_tree: std::cell::RefCell::new(None),
            item_children: std::cell::RefCell::new(None),
            published_children: std::cell::RefCell::new(None),
            binding_head: std::cell::RefCell::new(None),
            nested_views: std::cell::RefCell::new(None),
            history: true,
            native_presentation,
        }
    }

    /// A root whose resource keeps no document state: it publishes no
    /// History actions and its window offers no History.
    pub(crate) fn without_history(mut self) -> Self {
        self.history = false;
        self
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn drawing_past(mut self, past: crate::runner::DrawPast) -> Self {
        self.native_presentation.drawing_past(past);
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
        #[cfg(feature = "test-support")]
        let _profile = super::profiling::Scope::start("framework.access_refresh");
        if let (Some(permissions), Some(write)) = (&self.permissions, &self.write) {
            self.dispatch.set_read_only(!permissions.check(write));
        }
        self.dispatch.refresh_permission_states();
    }

    /// The root as the widget a host mounts.
    pub(crate) fn into_widget(self) -> Box<dyn AppWidget> {
        Box::new(RootWidget::new(self))
    }

    pub(crate) fn live(&self) -> &Live<R> {
        &self.live
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

    /// Bind the edits surfaces offer on the rows `rows` name
    /// ([`ResourceSurface::row_declarations`](super::ResourceSurface::row_declarations)).
    ///
    /// One walk of the view asks every surface about every row, so binding
    /// costs the view plus surfaces × rows, never the view once per row. Each
    /// row binds its edits in tree order of the surfaces offering them.
    #[inline(never)]
    fn bind_row_declarations(&self, rows: &mut [tilo_actions::Resource]) {
        // Every row, nested ones too (a playlist's tracks), asked of the
        // surfaces in one walk.
        fn ids(rows: &[tilo_actions::Resource], out: &mut Vec<String>) {
            for row in rows {
                out.push(row.id.as_str().to_owned());
                ids(&row.children, out);
            }
        }
        fn bind<R: Resource>(
            rows: &mut [tilo_actions::Resource],
            offered: &mut std::collections::HashMap<String, Vec<tilo_resource::ActionDecl<R>>>,
        ) {
            for row in rows {
                for decl in offered.remove(row.id.as_str()).unwrap_or_default() {
                    let bound = std::mem::replace(row, tilo_actions::Resource::new("", "", ""));
                    *row = tilo_resource::bind_declaration(bound, &decl);
                }
                bind(&mut row.children, offered);
            }
        }
        if rows.is_empty() {
            return;
        }
        let mut all = Vec::new();
        ids(rows, &mut all);
        let mut offered: std::collections::HashMap<String, Vec<tilo_resource::ActionDecl<R>>> =
            std::collections::HashMap::new();
        self.surfaces(&mut |surface| {
            for id in &all {
                let decls = surface.row_declarations(id);
                if !decls.is_empty() {
                    offered.entry(id.clone()).or_default().extend(decls);
                }
            }
        });
        bind(rows, &mut offered);
    }

    /// The resource as published: its own actions and children, the edits
    /// views and surfaces bound to where the user is, the live rows surfaces
    /// show, and the framework's operations every app answers.
    #[inline(never)]
    fn publication(&self) -> tilo_actions::Resource {
        #[cfg(feature = "test-support")]
        let _profile = super::profiling::Scope::start("framework.publication");
        let state = self.live.get();
        let tree = self.shared_document_tree(&state).to_resource();
        self.decorate_publication(&state, tree, Decoration::Tree)
    }

    /// The document-owned part of what controls bind to: the live
    /// resource's identity and own actions, without its children. Taken
    /// from the cached tree when it holds this change; else published
    /// without building a child, so it costs the same however many items
    /// the resource holds.
    #[inline(never)]
    fn binding_head(&self) -> BindingHead {
        let state = self.live.get();
        let revision = (self.live.revision(), ui_core::i18n::I18nManager::revision());
        let epoch = R::publication_cache_epoch(&state);
        let cached = epoch.as_ref().and_then(|epoch| {
            self.publication_tree
                .borrow()
                .as_ref()
                .filter(|cached| cached.revision == (revision.0, revision.1, epoch.clone()))
                .map(|cached| cached.tree.head().clone())
        });
        let resource = cached.unwrap_or_else(|| {
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.resource_head");
            tilo_resource::publish_head::<R>(self.dispatch.resource_id(), &self.label, &state)
        });
        BindingHead {
            revision,
            epoch,
            resource,
        }
    }

    /// The head bound controls were last refreshed from, while it still
    /// holds: the resource may reuse its publication
    /// ([`Resource::publication_cache_epoch`]) and nothing it depends on
    /// moved since. A resource that may not is published afresh each sync.
    fn retained_head(&self) -> Option<BindingHead> {
        let head = self.binding_head.borrow_mut().take()?;
        let current = self.live.with(R::publication_cache_epoch);
        (head.epoch.is_some() && head.epoch == current).then_some(head)
    }

    /// What bound controls look up ([`super::controls::Actions`]): the
    /// published resource's identity and actions over `head`, exactly as
    /// [`Self::publication`] publishes them, without building or decorating
    /// its children.
    #[inline(never)]
    fn binding_publication(&self, head: &BindingHead) -> tilo_actions::Resource {
        self.decorate_publication(&self.live.get(), head.resource.clone(), Decoration::Actions)
    }

    /// Refresh the actions bound controls look up, reusing `head` while the
    /// live state and locale are those it was built at. Answers the head
    /// they were refreshed from.
    #[inline(never)]
    fn refresh_bindings(&self, head: Option<BindingHead>) -> BindingHead {
        #[cfg(feature = "test-support")]
        let _profile = super::profiling::Scope::start("framework.action_bindings");
        let revision = (self.live.revision(), ui_core::i18n::I18nManager::revision());
        let head = match head {
            Some(head) if head.revision == revision => head,
            _ => self.binding_head(),
        };
        self.action_bindings.update(self.binding_publication(&head));
        head
    }

    /// [`Self::publication`] of the resource as `doc` holds it now, which a
    /// change from another window or participant may have moved past what
    /// this view last synced.
    #[inline(never)]
    fn current_publication(&self, doc: &AutoCommit) -> tilo_actions::Resource {
        if !self.live.follows(doc)
            && let Ok(current) = R::read(doc)
        {
            // Publication can run after an edit and before the view syncs.
            // Validate its nested views against this same state, rather than
            // withdrawing the new resource tree using the previous frame's
            // handler table. The next live revision replaces this cache.
            self.pin_nested_views(&current);
            return self.publication_of(&current);
        }
        self.publication()
    }

    #[inline(never)]
    fn publication_of(&self, state: &R::State) -> tilo_actions::Resource {
        #[cfg(feature = "test-support")]
        let _profile = super::profiling::Scope::start("framework.publication");
        let tree = self.shared_document_tree(state).to_resource();
        self.decorate_publication(state, tree, Decoration::Tree)
    }

    /// Add to `resource`, the document-owned tree, what the view and the
    /// framework publish on it. [`Decoration::Actions`] decorates only the
    /// resource's own actions; its children stay as they are.
    #[inline(never)]
    fn decorate_publication(
        &self,
        state: &R::State,
        mut resource: tilo_actions::Resource,
        decoration: Decoration,
    ) -> tilo_actions::Resource {
        let children = decoration == Decoration::Tree;
        let mut bound = {
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.bound_declarations");
            self.bindings.declarations(state)
        };
        let mut rows = Vec::new();
        let mut availability = Vec::new();
        self.surfaces(&mut |surface| {
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.surface_publication");
            bound.extend(surface.bound_declarations());
            availability.extend(surface.availability());
            if children {
                rows.extend(
                    surface
                        .live_rows()
                        .into_iter()
                        .map(super::LiveRow::into_resource),
                );
            }
        });
        for decl in bound {
            resource = tilo_resource::bind_declaration(resource, &decl);
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
        if children {
            merge(&mut resource.children, rows);
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.row_declarations");
            self.bind_row_declarations(&mut resource.children);
        }
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
        // The app's own effects come before the framework's history and
        // render operations: an app answering `diff` its own way is asked.
        let effects = {
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.effect_specs");
            self.effects.specs()
        };
        for spec in effects {
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
        resource = with_native_presentations(resource);
        if R::READABLE
            && !resource
                .actions
                .iter()
                .any(|existing| existing.id == "read")
        {
            resource = resource.with_action(read_spec());
        }
        for spec in super::history::specs().into_iter().filter(|_| self.history) {
            if !resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
            {
                resource = resource.with_action(spec);
            }
        }
        if self.effects.has_causal_drains() {
            mark_document_jobs(&mut resource);
        }
        if let Some(unreadable) = self.live.unreadable() {
            withhold_document_actions(&mut resource, &unreadable.reason);
        }
        if children {
            resource.children.extend(self.storage.record_resources());
        }
        resource
    }

    /// Show why the record does not read as the resource instead of the
    /// view while it does not, never the last state as if it were current,
    /// and refuse submissions with the same reason.
    #[inline(never)]
    fn follow_readability(&mut self) {
        let reason = self.live.unreadable().map(|unreadable| unreadable.reason);
        if self.unreadable.as_ref().map(|(shown, _)| shown) == reason.as_ref() {
            return;
        }
        self.dispatch.set_unreadable(reason.clone());
        self.unreadable = reason.map(|reason| {
            let shown: Element = Box::new(ui_core::widget::EmptyState::error(
                ui_core::i18n::I18nManager::lookup_plain("framework-unreadable"),
                reason.clone(),
            ));
            (reason, shown)
        });
        self.swapped = true;
    }

    /// The dispatcher the root's views submit through, which the host drains.
    pub(crate) fn dispatcher(&self) -> &Dispatcher<R> {
        &self.dispatch
    }

    /// Follow the document when it moved — a local action, a
    /// collaborator's change, an undo or a restore — reading only what the
    /// change touched.
    ///
    /// Answers the document-owned head the bindings were refreshed from, for
    /// the refresh after the view synced to reuse.
    fn refresh(&self, doc: &AutoCommit) -> BindingHead {
        if let Err(error) = self.live.sync(doc) {
            tracing::warn!(%error, kind = R::KIND, "resource unreadable");
        }
        // Refresh the same declarations the host publishes before rebuilt
        // controls look up their bindings. Retained surface handles share it.
        let head = self.refresh_bindings(self.retained_head());
        // A surface may have synced the state itself after its own edit.
        let revision = self.live.revision();
        let change = self.live.take_change();
        if revision != self.followed.get() || change.is_some() {
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.updaters");
            self.followed.set(revision);
            let change = match &change {
                Some(delta) => Change::Delta { delta, doc },
                None => Change::Everything,
            };
            self.updaters.run(&self.live.get(), &change);
        }
        self.live.place_landings();
        self.feeds.refresh(doc);
        self.live
            .with(|state| self.effects.drain_projections(state, revision));
        self.effects.poll_jobs();
        head
    }
}

/// The resource's own actions `encoded` applies: itself, or each member of
/// a group of them, last landing last.
fn own_actions<R: Resource>(encoded: &tilo_actions::EncodedAction) -> Vec<R::Action> {
    if encoded.type_id == R::ACTION_TYPE {
        return tilo_actions::decode_json(&encoded.payload)
            .map(|action| vec![action])
            .unwrap_or_default();
    }
    if encoded.type_id == tilo_resource::group::group_type::<R>() {
        return tilo_actions::decode_json::<tilo_resource::ActionGroup<R::Action>>(
            &encoded.payload,
        )
        .map(|group| group.actions)
        .unwrap_or_default();
    }
    Vec::new()
}

/// How much of a publication [`ResourceRoot::decorate_publication`] adds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decoration {
    /// The whole published tree: the resource's actions and its children.
    Tree,
    /// Only the resource's identity and own actions, which is all bound
    /// controls read.
    Actions,
}

/// The document-owned head of the publication, at the live and locale
/// revision it was built at.
#[derive(Clone)]
struct BindingHead {
    revision: (u64, u64),
    /// [`Resource::publication_cache_epoch`] at the time.
    epoch: Option<String>,
    resource: tilo_actions::Resource,
}

/// `resource` without its children, cloning nothing of them.
#[cfg(all(test, not(target_arch = "wasm32")))]
fn without_children(resource: &tilo_actions::Resource) -> tilo_actions::Resource {
    let tilo_actions::Resource {
        id,
        kind,
        label,
        presentation,
        semantic_identity,
        children: _,
        actions,
        views,
    } = resource;
    tilo_actions::Resource {
        id: id.clone(),
        kind: kind.clone(),
        label: label.clone(),
        presentation: presentation.clone(),
        semantic_identity: semantic_identity.clone(),
        children: Vec::new(),
        actions: actions.clone(),
        views: views.clone(),
    }
}

/// Every document action of `resource` and its children, unavailable for
/// `reason`: the record does not read as the resource.
fn withhold_document_actions(resource: &mut tilo_actions::Resource, reason: &str) {
    for spec in &mut resource.actions {
        if spec.kind == tilo_actions::OperationKind::Document {
            spec.contract.availability = tilo_actions::Availability::Unavailable {
                code: "unreadable".into(),
                reason: reason.to_owned(),
            };
        }
    }
    for child in &mut resource.children {
        withhold_document_actions(child, reason);
    }
}

/// Visit the surface of type `S` the root of a resource app of `R` shows: for
/// a test, or a host that mounted the app, reaching the surface between
/// frames (`App::with_component_root` lends the root of a mounted one).
/// `None` when `root` is not such a root or shows no `S`.
pub fn with_surface<R: Resource, S: 'static, T>(
    root: &mut dyn std::any::Any,
    visit: impl FnOnce(&mut S) -> T,
) -> Option<T> {
    fn find<S: 'static>(widget: &mut dyn ui_core::widget::Widget) -> Option<&mut S> {
        if widget.as_any_mut().is_some_and(|any| any.is::<S>()) {
            return widget.as_any_mut()?.downcast_mut::<S>();
        }
        widget.children_mut().into_iter().find_map(find::<S>)
    }
    let root = root.downcast_mut::<ResourceRoot<R>>()?;
    find::<S>(root.view_mut()).map(visit)
}

/// The document actions of `resource` and its children that can queue an
/// outside consequence (a verb taking an outbox, every action of a record
/// with a `queue`, and undo, redo and restore, which queue the compensation
/// of one), published as jobs: the invocation answers when what it queued
/// has settled. Every other document action is deterministic and applies
/// where it is invoked, at once.
fn mark_document_jobs(resource: &mut tilo_actions::Resource) {
    for spec in &mut resource.actions {
        let queues = spec.contract.kind == tilo_actions::ActionKind::Outside
            || matches!(spec.id.as_str(), "undo" | "redo" | "restore");
        if spec.kind == tilo_actions::OperationKind::Document && queues {
            spec.contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
            if matches!(spec.contract.result, tilo_actions::ResultSpec::None) {
                spec.contract.result = tilo_actions::ResultSpec::Job;
            }
        }
    }
    for child in &mut resource.children {
        mark_document_jobs(child);
    }
}

impl<R: Resource> ui_core::widget::document_effects::DocumentEffects for ResourceRoot<R> {
    fn start_document_effects(
        &mut self,
        operation: &ui_core::host_operation::ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        self.effects.start_document_effects(operation, applied)
    }
}

/// How many search entries are indexed away from the UI thread rather than
/// in the frame that asks for them.
const OFF_THREAD_ENTRIES: usize = 1024;

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
pub(crate) struct IndexedEntries {
    entries: Vec<SearchEntry>,
    facets: Vec<ui_core::search::Facet>,
    index: SearchIndex,
    /// Which build of the entries this is: a page cursor from another build
    /// starts over rather than skipping or repeating.
    generation: u64,
}

impl IndexedEntries {
    pub(crate) fn built(entries: Vec<SearchEntry>, generation: u64) -> Self {
        // Entry titles are names a person half remembers: typos match too.
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(entries.iter().enumerate().map(|(at, entry)| {
            let mut document = Document::new(at as u64)
                .field(entry.title.as_str(), FieldWeight::TITLE)
                .field(entry.text.as_str(), FieldWeight::BODY)
                .field(
                    entry.subtitle.as_deref().unwrap_or_default(),
                    FieldWeight::SUBTITLE,
                );
            for (text, weight) in &entry.fields {
                document = document.field(text.as_str(), field_weight(*weight));
            }
            match &entry.body {
                Some(body) => document.body(Arc::new(BodyText(body.shared()))),
                None => document,
            }
        }));
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index resource search entries");
        }
        let mut groups: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
        for entry in &entries {
            for (key, label, value) in &entry.facets {
                groups
                    .entry(key.clone())
                    .or_insert_with(|| (label.clone(), BTreeSet::new()))
                    .1
                    .insert(value.clone());
            }
        }
        let facets = groups
            .into_iter()
            .map(|(key, (label, values))| {
                values
                    .into_iter()
                    .fold(ui_core::search::Facet::new(key, label), |facet, value| {
                        facet.value(value.clone(), value)
                    })
            })
            .collect();
        Self {
            entries,
            facets,
            index,
            generation,
        }
    }
}

/// The index's weight for a field an entry ranks as `weight`.
fn field_weight(weight: tilo_resource::SearchWeight) -> FieldWeight {
    use tilo_resource::SearchWeight;
    match weight {
        SearchWeight::Title => FieldWeight::TITLE,
        SearchWeight::Identity => FieldWeight::IDENTITY,
        SearchWeight::Subtitle => FieldWeight::SUBTITLE,
        SearchWeight::Metadata => FieldWeight::METADATA,
        SearchWeight::Body => FieldWeight::BODY,
        SearchWeight::Unranked => FieldWeight::UNRANKED,
    }
}

/// A search provider over one snapshot of the resource's entries.
pub(crate) struct ResourceSearch {
    name: &'static str,
    indexed: Arc<IndexedEntries>,
    scopes: Vec<ScopeDecl>,
    queries: Vec<super::surface::SearchQueries>,
}

/// `result` read as the entry's row: its subtitle, its icon and its shape.
fn with_row(mut result: SearchResult, title: &str, row: &tilo_resource::SearchRow) -> SearchResult {
    use tilo_resource::SearchRowShape;
    use ui_core::search::SearchRow;
    if let Some(subtitle) = &row.subtitle {
        result = result.with_subtitle(subtitle.clone());
    }
    if let Some(icon) = row
        .icon
        .as_deref()
        .and_then(ui_core::icons::Icon::from_name)
    {
        result = result.with_icon(icon);
    }
    let shaped = match &row.shape {
        SearchRowShape::Plain => None,
        SearchRowShape::Message {
            sender,
            snippet,
            date,
            unread,
        } => {
            let mut shaped = SearchRow::message(sender.clone(), title).unread(*unread);
            if let Some(snippet) = snippet {
                shaped = shaped.snippet(snippet.clone());
            }
            if let Some(date) = date {
                shaped = shaped.date(date.clone());
            }
            Some(shaped)
        }
        SearchRowShape::Media { byline, duration } => {
            let mut shaped = SearchRow::media(title);
            if let Some(byline) = byline {
                shaped = shaped.byline(byline.clone());
            }
            if let Some(duration) = duration {
                shaped = shaped.duration(duration.clone());
            }
            Some(shaped)
        }
        SearchRowShape::FileHit {
            path,
            line,
            snippet,
        } => {
            let mut shaped = SearchRow::file_hit(path.clone(), snippet.clone());
            if let Some(line) = line {
                shaped = shaped.line(*line);
            }
            Some(shaped)
        }
    };
    match shaped {
        Some(shaped) => result.with_row(shaped),
        None => result,
    }
}

impl ResourceSearch {
    /// The search source over `indexed`, listed under `name` unless an
    /// entry names its own group.
    pub(crate) fn new(name: &'static str, indexed: Arc<IndexedEntries>) -> Self {
        Self {
            name,
            indexed,
            scopes: Vec::new(),
            queries: Vec::new(),
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
        let typed = SearchQuery::new(query);
        for queries in &self.queries {
            queries.submit(&typed);
        }
        self.results(&typed, Page::first(limit.min(DEFAULT_SEARCH_RESULT_LIMIT)))
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
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        for queries in &self.queries {
            queries.submit(query);
        }
        let limit = work_budget
            .min(DEFAULT_SEARCH_CANDIDATE_LIMIT)
            .min(result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT));
        let folded = format!(
            "{}\0{:?}",
            ui_core::search::normalize_search_text(query.text()),
            query.filters()
        );
        let offset = cursor
            .map(SearchCursor::opaque_parts)
            .filter(|(generation, at_query, at_scope, _)| {
                *generation == self.indexed.generation
                    && *at_query == folded
                    && *at_scope == query.scope()
            })
            .and_then(|(_, _, _, offset)| usize::try_from(offset?).ok())
            .unwrap_or(0);
        if limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
                pending: None,
            };
        }
        let (results, more, inspected) = self.results(query, Page { offset, limit });
        let next = more.then(|| {
            SearchCursor::opaque(
                self.indexed.generation,
                folded,
                query.scope().map(str::to_owned),
                u64::try_from(offset.saturating_add(inspected)).ok(),
            )
        });
        SearchPage {
            results,
            next,
            inspected,
            pending: None,
        }
    }

    fn facets(&self) -> Vec<ui_core::search::Facet> {
        self.indexed.facets.clone()
    }

    fn search_scopes(&self) -> Vec<ScopeDecl> {
        self.scopes.clone()
    }
}

impl ResourceSearch {
    /// One page of hits for `query`, and whether more follow it.
    fn results(&self, query: &SearchQuery, page: Page) -> (Vec<SearchResult>, bool, usize) {
        let found = self.indexed.index.search(query.text(), page);
        let inspected = found.hits.len();
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
                if query.scope().is_some_and(|scope| !entry.in_scope(scope))
                    || query.filters().iter().any(|filter| {
                        !parent.facets.iter().any(|(key, _, value)| {
                            key.eq_ignore_ascii_case(&filter.key)
                                && query.filter_values(&filter.key).any(|selected| {
                                    ui_core::search::normalize_search_text(value)
                                        == ui_core::search::normalize_search_text(selected)
                                })
                        })
                    })
                {
                    return None;
                }
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
                if let Some(within) = &entry.within {
                    result = result.within(within.clone());
                }
                if let Some(row) = &entry.row {
                    result = with_row(result, &entry.title, row);
                }
                Some(result.with_score(hit.rank.unwrap_or(0).saturating_add(entry.boost)))
            })
            .collect();
        (results, found.more, inspected)
    }
}

// The root is the app's view plus what the framework adds: it keeps the
// state current, publishes the resource, forwards edits and indexes search.
// Everything else — layout, input, traversal, operation admission — is the
// view's own, forwarded by the decorator.
impl<R: Resource> RootModel for ResourceRoot<R> {
    ui_core::provides!(dyn ui_core::undo::CommittedActions, dyn ui_core::history_mode::capability::DocumentHistory, dyn ui_core::widget::document_effects::DocumentEffects, #[cfg(not(target_family = "wasm"))] dyn ui_core::component_lifecycle::WindowActivation; inner = content);

    fn view(&self) -> &(dyn Widget + 'static) {
        match &self.unreadable {
            Some((_, shown)) => shown.as_ref(),
            None => self.content.as_ref(),
        }
    }

    fn view_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.unreadable {
            Some((_, shown)) => shown.as_mut(),
            None => self.content.as_mut(),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let swapped = std::mem::take(&mut self.swapped);
        self.view_mut().take_structure_dirty() || swapped
    }

    // Input is forwarded unchanged; model, storage and policy wakes remain
    // owned by the runner and still request their normal synchronization.
    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool {
        self.content.view_only_scroll(event)
    }

    fn record_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        let mut owners = self.storage.person_owners();
        owners.extend(self.content.record_owners());
        owners
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui_core::doc::document_ref(state, path);
        self.refresh_access();
        let head = self.refresh(state);
        self.follow_readability();
        self.storage.refresh();
        {
            #[cfg(feature = "test-support")]
            let _profile = super::profiling::Scope::start("framework.content_sync");
            self.content.sync(state, path);
        }
        // What the view's sync changed (a surface's bound edits, its
        // availability) reaches the bindings; the document-owned head is
        // rebuilt only when the sync moved the state.
        let head = self.refresh_bindings(Some(head));
        *self.binding_head.borrow_mut() = Some(head);
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId) {
        self.effects.apply_settlements(edits);
        self.content.settle(edits, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let ground = theme.tokens.colors.surfaces.content;
        self.native_presentation.track_theme(theme);
        // The window ground is the framework's: an app never fills it, and
        // one that paints nothing still stands on it.
        scene.fill(
            ui_core::vello::peniko::Fill::NonZero,
            transform,
            ground,
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
        RootModel::draw(self, scene, transform, text, theme, presence);
    }

    fn needs_redraw(&self) -> bool {
        self.native_presentation.needs_redraw() || self.content.needs_redraw()
    }

    // A screenshot harness reaches the root to stage a scenario's view state
    // (`framework::stage`).
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    // The resource is the same for every participant (an agent task sees and
    // invokes the same actions); only the content's tree is per participant.

    // The undo boundary of a surface's immediate actions this event; the
    // host records one step per event and author.

    // A surface's step stays open for the actions that continue it (the
    // letters of a word), which report no boundary of their own.

    /// Resource and app presentation doors publish effect/view actions; their
    /// operation types must be in the retained handler table or publication
    /// withdraws the whole component surface.
    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = self.view().resource_operation_handlers();
        handlers.extend(
            crate::runner::operation_types()
                .iter()
                .map(|op| (*op).to_owned()),
        );
        handlers.insert(super::history::VIEW_TYPE.to_owned());
        handlers.insert(super::person::HISTORY_TYPE.to_owned());
        handlers.insert(super::record::RECORD_ACTION_TYPE.to_owned());
        if self.stages.opens() {
            handlers.insert(OPEN_TYPE.to_owned());
        }
        // Opening another app is routed by the host to the workspace
        // broker, whichever resource offers it.
        handlers.insert(tilo_actions::remote::ApplicationLaunch::TYPE.to_owned());
        if R::READABLE {
            handlers.insert(READ_TYPE.to_owned());
        }
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
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        if let Some(output) =
            self.native_presentation
                .start_operation(op, doc, self.content.as_mut())?
        {
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        if R::READABLE
            && let Some(request) = op.unscoped()
            && request.type_id == READ_TYPE
        {
            let output = read::<R>(&self.live.get(), doc, request.payload)?;
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        if let Some(request) = op.unscoped()
            && request.type_id == OPEN_TYPE
        {
            let args: serde_json::Value =
                tilo_actions::decode_json(request.payload).map_err(|error| error.to_string())?;
            let anchor = args
                .get(OPEN_ANCHOR)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "open needs the anchor of an item".to_owned())?;
            self.stages.open(anchor)?;
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ),
            ));
        }
        if let Some(request) = op.unscoped()
            && request.type_id == super::history::VIEW_TYPE
        {
            let output = super::history::answer(doc, request.payload)?;
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        if let Some(request) = op.unscoped()
            && request.type_id == super::record::RECORD_ACTION_TYPE
        {
            let output = self
                .storage
                .record_action(self.dispatch.me(), request.payload)?;
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        if let Some(request) = op.unscoped()
            && request.type_id == super::person::HISTORY_TYPE
        {
            let output = self.storage.person_history(request.payload)?;
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        // An effect reads the resource as the document stands, though no
        // frame has run since a collaborator's change arrived.
        if let Err(error) = self.live.sync(doc) {
            tracing::warn!(%error, kind = R::KIND, "resource unreadable");
        }
        if let Some(started) = self.effects.start(op)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.view_mut().start_operation(op, doc, path)? {
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
            Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ),
            ))
        } else {
            Ok(None)
        }
    }
}

impl<R: Resource> ResourceRoot<R> {
    /// The index of the resource's entries at `revision` and the surfaces'
    /// `shown` ones. A large set of entries (a browsing history, a store
    /// catalogue) or one with prose bodies (whole books) is indexed away
    /// from the UI thread: until that index arrives, search answers from the
    /// one before it.
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
        if entries.len() >= OFF_THREAD_ENTRIES || entries.iter().any(|entry| entry.body.is_some()) {
            let (send, arriving) = std::sync::mpsc::channel();
            let building = entries.clone();
            let spawned = ui_core::background::Background::current().spawn(
                "resource-search-index",
                move || {
                    let _ = send.send(IndexedEntries::built(building, generation));
                },
            );
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

impl<R: Resource> RootSearch for ResourceRoot<R> {
    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        // The resource's entries change with its revision; what the surfaces
        // show (a page's text, live rows) changes on their own, so their
        // entries are read now and the index rebuilt only when either moved.
        let revision = self.live.revision();
        let mut shown = Vec::new();
        let mut queries = Vec::new();
        self.surfaces(&mut |surface| {
            shown.extend(surface.search_entries());
            queries.push(surface.search_queries());
        });
        let indexed = self.current_search_index(revision, shown);
        // The app's one search source: what its content widgets show beside
        // the resource reaches it as their surfaces' entries, never as a
        // provider of their own.
        let mut source = ResourceSearch::new(R::LABEL, indexed);
        source.scopes = R::search_scopes(&self.live.get());
        source.queries = queries;
        vec![Box::new(source)]
    }

    /// A picked hit opens the app at its anchor: the views registered with
    /// `on_open` show it, then any hit the content provided reaches it.
    fn activate_search_result(
        &mut self,
        action_id: &str,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        if self.stages.open(action_id).is_err() {
            ui_core::search::capability::activate_search_result(
                self.content.as_mut(),
                action_id,
                doc,
                path,
            );
        }
    }
}

impl<R: Resource> RootPublishes for ResourceRoot<R> {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        RootPublishes::resources_for(self, None, doc)
    }
    fn shared_resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Option<Vec<ui_core::published::SharedResource>> {
        self.shared_resources(participant, doc)
    }
    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        // What surfaces bind to where someone is, they bind to this
        // participant's view.
        let _viewer = participant
            .cloned()
            .map(ui_core::invocation_scope::ViewerScope::enter);
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
        if self.stages.opens()
            && !resource
                .actions
                .iter()
                .any(|existing| existing.id == "open")
        {
            resource = resource.with_action(open_spec());
        }
        if self.stages.opens() {
            offer_item_open::<R>(&mut resource);
        }
        let mut resources = vec![resource];
        resources.append(&mut extras);
        resources
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        RootPublishes::resource_for(self, None, doc, id)
    }
    /// Resource `id` in the published tree, or one the tree leaves out: a
    /// child the resource resolves by id, or a live row a surface resolves.
    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        let _viewer = participant
            .cloned()
            .map(ui_core::invocation_scope::ViewerScope::enter);
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
            Some(current) => tilo_resource::child_by_id::<R>(current, doc, id),
            None => tilo_resource::child_by_id::<R>(&self.live.get(), doc, id),
        };
        let mut row = None;
        self.surfaces(&mut |surface| {
            if row.is_none() {
                row = surface.live_row(id).map(super::LiveRow::into_resource);
            }
        });
        // An item past the published page is resolved on its own, and the row
        // a surface shows for it joins it as it does in the page.
        let resolved = match (child, row) {
            (Some(mut child), Some(row)) => {
                join(&mut child, row);
                Some(child)
            }
            (child, row) => child.or(row),
        };
        if let Some(mut resolved) = resolved {
            self.bind_row_declarations(std::slice::from_mut(&mut resolved));
            if self.stages.opens() {
                offer_item_open::<R>(&mut resolved);
            }
            return Some(resolved);
        }
        // Last, what the content resolves by id beyond what it publishes (a
        // place, a record past the published page), without a document
        // action: those are the resource's own.
        ui_core::resources::resource_for(self.content.as_ref(), None, doc, id)
            .map(super::rows::resolved_outside_document)
    }
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value> {
        // The record's own actions, and its verbs bound on its items.
        if address.resource == self.dispatch.resource_id()
            || tilo_resource::items::item_of_resource::<R>(address.resource.as_str()).is_some()
        {
            if super::history::is_edit(&address.action) {
                return super::history::result(&address.action, doc, changes);
            }
            // What the action minted rides beside its own answer, the
            // resource's or else its surface's. An answer that is not an
            // object (an editor's summary naming the blocks it made) already
            // says what it made and is kept as it is.
            let minted = tilo_resource::take_minted();
            let answer = R::answer(doc, &address.action, changes).or_else(|| {
                ui_core::resources::document_action_result(
                    self.content.as_ref(),
                    doc,
                    address,
                    changes,
                )
            });
            return match answer {
                _ if minted.is_empty() => answer,
                Some(serde_json::Value::Object(mut fields)) => {
                    fields.insert("minted".into(), minted.into());
                    Some(serde_json::Value::Object(fields))
                }
                Some(other) => Some(other),
                None => Some(serde_json::json!({ "minted": minted })),
            };
        }
        ui_core::resources::document_action_result(self.content.as_ref(), doc, address, changes)
    }
}

impl<R: Resource> RootControls for ResourceRoot<R> {
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
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        // The host delivers feedback to the exact emitting document owner;
        // forwarding stops at nested owner boundaries. Its children may have
        // arbitrary ids, including records removed since submission. Repair
        // this owner's projection without inferring custody from an id prefix
        // or retaining requests that History may capture or discard.
        if let Some(effect) = ui_core::widget::controls::result_on_behalf_of() {
            self.dispatch
                .note_continuation(effect, &address.action, result);
        }
        match result {
            Err(error) => {
                if !matches!(error, tilo_actions::DoorError::ConfirmationRequired) {
                    self.dispatch.refuse(&address.action, error.to_string());
                }
                self.live.represent();
            }
            Ok(ui_core::host_operation::ResourceOperationStart::Completed(output)) => {
                self.dispatch.note_landed(&address.action, output);
            }
            Ok(_) => {}
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
        !self.history
            || ui_core::widget::capability::capability::<
                dyn ui_core::history_mode::capability::DocumentHistory,
            >(self.content.as_ref())
            .is_some_and(
                ui_core::history_mode::capability::DocumentHistory::provides_document_history,
            )
    }

    fn physical_text_policy(
        &self,
        previous: Option<&tilo_actions::EncodedAction>,
        next: &tilo_actions::EncodedAction,
    ) -> (bool, bool) {
        if next.type_id != R::ACTION_TYPE {
            return (false, false);
        }
        let Ok(next) = tilo_actions::decode_json::<R::Action>(&next.payload) else {
            return (false, false);
        };
        let previous = previous
            .filter(|encoded| encoded.type_id == R::ACTION_TYPE)
            .and_then(|encoded| tilo_actions::decode_json::<R::Action>(&encoded.payload).ok());
        (
            previous
                .as_ref()
                .is_some_and(|previous| R::coalesces(previous, &next)),
            R::continuable(&next),
        )
    }

    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[ui_core::automerge::ChangeHash],
        doc: &ui_core::automerge::AutoCommit,
        path: &ui_core::automerge::ObjId,
    ) {
        // An action a participant took (an agent's verb) leaves them where
        // the resource says it lands, once the state follows it.
        if let Some(participant) = ui_core::invocation_scope::current_participant() {
            for applied in own_actions::<R>(action) {
                self.live.note_applied(participant.clone(), applied);
            }
        }
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

impl<R: Resource> RootApp for ResourceRoot<R> {
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

    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        fn perform<R: Resource>(
            widget: &mut dyn Widget,
            operation: &ui_core::floating_surface::DeviceShellOperation,
        ) -> Option<Result<serde_json::Value, String>> {
            if let Some(surface) = capability_mut::<dyn SurfacePublication<R>>(widget)
                && surface.supports_device_shell()
            {
                return Some(surface.device_shell_operation(operation));
            }
            widget
                .children_mut()
                .into_iter()
                .find_map(|child| perform::<R>(child, operation))
        }
        perform::<R>(self.content.as_mut(), operation)
            .unwrap_or_else(|| Err("Terminal is not available".into()))
    }

    fn request_device_shell(&mut self) -> bool {
        fn request<R: Resource>(widget: &mut dyn Widget) -> bool {
            if let Some(surface) = capability_mut::<dyn SurfacePublication<R>>(widget)
                && surface.request_device_shell()
            {
                return true;
            }
            widget.children_mut().into_iter().any(request::<R>)
        }
        request::<R>(self.content.as_mut())
    }

    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        fn take<R: Resource>(widget: &mut dyn Widget) -> Option<ui_core::widget::AppHostIntent> {
            if let Some(surface) = capability_mut::<dyn SurfacePublication<R>>(widget)
                && let Some(intent) = surface.take_host_intent()
            {
                return Some(intent);
            }
            widget.children_mut().into_iter().find_map(take::<R>)
        }
        take::<R>(self.content.as_mut())
    }

    fn take_save_retry(&mut self) -> bool {
        fn take<R: Resource>(widget: &mut dyn Widget) -> bool {
            let own = capability_mut::<dyn SurfacePublication<R>>(widget)
                .is_some_and(SurfacePublication::take_save_retry);
            // Consume every surface's one-shot even when another requested it.
            widget
                .children_mut()
                .into_iter()
                .fold(own, |retry, child| take::<R>(child) || retry)
        }
        take::<R>(self.content.as_mut())
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
#[cfg(not(target_arch = "wasm32"))]
mod search_tests {
    use super::*;
    use crate::prelude::*;
    use std::cell::RefCell;

    thread_local! {
        static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    #[derive(Model, Default)]
    pub struct Mailbox {
        pub subject: String,
    }

    fn entries(_: &Mailbox) -> Vec<SearchEntry> {
        vec![
            SearchEntry {
                anchor: Some("folder:inbox/msg:1".into()),
                title: "Invoice from Ada".into(),
                text: "March".into(),
                ..Default::default()
            },
            SearchEntry {
                anchor: Some("folder:sent/msg:2".into()),
                title: "Invoice to Grace".into(),
                text: "April".into(),
                ..Default::default()
            },
            // A flat id that lies in the inbox, read as a message row.
            SearchEntry {
                anchor: Some("msg:3".into()),
                title: "Invoice reminder".into(),
                text: "May".into(),
                within: Some("folder:inbox".into()),
                row: Some(
                    tilo_resource::SearchRow::shaped(tilo_resource::SearchRowShape::Message {
                        sender: "Ada".into(),
                        snippet: None,
                        date: None,
                        unread: true,
                    })
                    .icon("mail"),
                ),
                ..Default::default()
            },
        ]
    }

    fn scopes(_: &Mailbox) -> Vec<ScopeDecl> {
        vec![ScopeDecl::new("folder:inbox", "Inbox")]
    }

    #[resource(kind = "test.mailbox", label = "Mailbox", search = entries, scopes = scopes)]
    impl Mailbox {
        /// Set the subject.
        #[action]
        pub fn set_subject(&mut self, subject: String) {
            self.subject = subject;
        }
    }

    struct MailApp;

    impl ResourceApp for MailApp {
        type Resource = Mailbox;
        const ID: &'static str = "so.tilo.test-mailbox";
        const TITLE: &'static str = "Mailbox";

        fn view(cx: &mut ViewCx<Mailbox>) -> Element {
            cx.on_open(|anchor| OPENED.with_borrow_mut(|opened| opened.push(anchor.to_owned())));
            Box::new(Column::new().push(Box::new(
                cx.search_button(ui_core::search::SearchRequest::this_app()),
            )))
        }
    }

    thread_local! {
        static QUERIES: RefCell<Vec<SearchQuery>> = const { RefCell::new(Vec::new()) };
        static SAVE_RETRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct QuerySurface(ui_core::widget::Text);

    impl ui_core::widget::Decorator for QuerySurface {
        type Inner = ui_core::widget::Text;
        fn inner(&self) -> &Self::Inner {
            &self.0
        }
        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.0
        }
    }

    impl crate::framework::ResourceSurface<Mailbox> for QuerySurface {
        fn present(&mut self, _: &Rc<Mailbox>) {}
        fn take_save_retry(&mut self) -> bool {
            SAVE_RETRY.with(|retry| retry.replace(false))
        }
        fn search_query(&mut self, query: &SearchQuery) {
            QUERIES.with_borrow_mut(|queries| queries.push(query.clone()));
        }
    }

    struct QueryApp;
    impl ResourceApp for QueryApp {
        type Resource = Mailbox;
        const ID: &'static str = "so.tilo.test-search-query";
        const TITLE: &'static str = "Query";
        fn view(cx: &mut ViewCx<Mailbox>) -> Element {
            cx.surface(QuerySurface(ui_core::widget::Text::new("Results")))
        }
    }

    #[test]
    fn record_owner_consumes_the_surfaces_save_retry_once() {
        SAVE_RETRY.with(|retry| retry.set(false));
        let mut app = TestApp::<QueryApp>::new().unwrap();
        assert!(!app.take_save_retry().unwrap());
        SAVE_RETRY.with(|retry| retry.set(true));
        assert!(app.take_save_retry().unwrap());
        assert!(!app.take_save_retry().unwrap());
    }

    #[test]
    fn platform_search_delivers_only_the_latest_bounded_query_to_the_surface() {
        QUERIES.with_borrow_mut(Vec::clear);
        let mut app = TestApp::<QueryApp>::new().unwrap();
        QUERIES.with_borrow_mut(Vec::clear);
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot<Mailbox>| {
                let providers = RootSearch::search_providers(root);
                assert_eq!(providers.len(), 1, "one canonical resource source");
                let search = &providers[0];
                search.search("old");
                let query = SearchQuery::new("invoice").within("folder:inbox");
                search.search_page(&query, None, 10, 10);
                search.search_page(&query, None, 10, 10);
                search.search(&"x".repeat(ui_core::search::MAX_SEARCH_QUERY_SCALARS + 1));
                assert!(
                    QUERIES.with_borrow(Vec::is_empty),
                    "delivery waits for UI sync"
                );
            })
            .unwrap();
        app.settle();
        QUERIES.with_borrow(|queries| {
            assert_eq!(
                queries,
                &[SearchQuery::new("invoice").within("folder:inbox")]
            );
        });
        app.settle();
        assert_eq!(
            QUERIES.with_borrow(Vec::len),
            1,
            "a settled query is not repeated"
        );
    }

    #[test]
    fn a_hit_opens_the_app_at_its_anchor_and_scopes_narrow_the_search() {
        let mut app = TestApp::<MailApp>::new().unwrap();
        let (titles, mut scoped, declared, reminder) = app
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot<Mailbox>| {
                let providers = RootSearch::search_providers(root);
                let search = providers.first().unwrap();
                let titles = |query: &SearchQuery| {
                    search
                        .search_page(query, None, 10, 10)
                        .results
                        .into_iter()
                        .map(|hit| hit.title)
                        .collect::<Vec<_>>()
                };
                let reminder = search
                    .search_page(&SearchQuery::new("reminder"), None, 10, 10)
                    .results
                    .into_iter()
                    .next();
                (
                    titles(&SearchQuery::new("invoice")),
                    titles(&SearchQuery::new("invoice").within("folder:inbox")),
                    search.search_scopes(),
                    reminder,
                )
            })
            .unwrap();
        assert_eq!(titles.len(), 3);
        scoped.sort();
        assert_eq!(scoped, ["Invoice from Ada", "Invoice reminder"]);
        let reminder = reminder.expect("the flat-id entry is found");
        assert_eq!(reminder.anchor(), "folder:inbox");
        assert_eq!(reminder.icon, Some(ui_core::icons::Icon::Mail));
        assert!(matches!(
            reminder.row,
            Some(ui_core::search::SearchRow::Message { unread: true, .. })
        ));
        assert_eq!(declared, [ScopeDecl::new("folder:inbox", "Inbox")]);

        // Picking a hit opens the app at it.
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot<Mailbox>| {
                let mut doc = ui_core::automerge::AutoCommit::new();
                RootSearch::activate_search_result(
                    root,
                    "folder:inbox/msg:1",
                    &mut ui_core::widget::Edits::lend(&mut doc),
                    &ui_core::automerge::ROOT,
                );
            })
            .unwrap();
        OPENED.with_borrow(|opened| assert_eq!(opened.as_slice(), ["folder:inbox/msg:1"]));

        // An agent opens it through the same door.
        app.call("open", serde_json::json!({ "anchor": "folder:sent/msg:2" }))
            .unwrap();
        OPENED.with_borrow(|opened| {
            assert_eq!(opened.last().map(String::as_str), Some("folder:sent/msg:2"));
        });
    }
}

/// The window of a record that did not read as the resource when it was
/// opened: why, instead of the app's view, and the resource's actions
/// published as unavailable for that reason.
pub(crate) struct Unopened {
    title: &'static str,
    icon: IconDescriptor,
    /// What it publishes: the resource's declared actions, every document
    /// one unavailable for the reason.
    resource: tilo_actions::Resource,
    shown: Element,
}

impl Unopened {
    /// The window of a record that does not read as `R`. Only this
    /// constructor knows `R`; the widget itself is the same for every
    /// resource type.
    pub(crate) fn new<R: Resource>(
        title: &'static str,
        icon: IconDescriptor,
        reason: String,
    ) -> Self {
        let mut resource = tilo_actions::Resource::new(R::KIND, R::KIND, R::LABEL);
        for decl in R::declarations() {
            resource = resource.with_action(tilo_resource::declaration_spec(&decl));
        }
        withhold_document_actions(&mut resource, &reason);
        Self {
            title,
            icon,
            shown: Box::new(ui_core::widget::EmptyState::error(
                ui_core::i18n::I18nManager::lookup_plain("framework-unreadable"),
                reason,
            )),
            resource,
        }
    }
}

impl Decorator for Unopened {
    ui_core::provides!(dyn ui_core::resources::PublishesResources);

    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.shown.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.shown.as_mut()
    }
}

impl ui_core::resources::PublishesResources for Unopened {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![self.resource.clone()]
    }
}

impl AppWidget for Unopened {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids the framework keeps are the ones it publishes on every
    /// app: the generic actions, `open`, `read`, history and presentation.
    #[test]
    fn the_reserved_ids_are_what_the_framework_publishes() {
        let reserved = reserved_action_ids();
        for id in [
            "apply",
            "group",
            "field-edit",
            "open",
            "read",
            "undo",
            "redo",
            "restore",
            "checkpoint",
            "history",
            "diff",
            "render",
        ] {
            assert!(
                reserved.iter().any(|kept| kept == id),
                "{id} in {reserved:?}"
            );
        }
        assert!(
            !reserved.iter().any(|kept| kept == "rename"),
            "an app's own verb name is not kept"
        );
    }

    #[test]
    fn a_search_entry_naming_an_action_is_a_hit_that_invokes_it() {
        let open = tilo_actions::ActionAddress::new("notes.page:1", "open");
        let search = ResourceSearch {
            name: "Notes",
            indexed: Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Groceries", "milk and eggs").invoking(open.clone()),
                    SearchEntry::new("Garden", "roses").anchored("page:2"),
                ],
                0,
            )),
            scopes: Vec::new(),
            queries: Vec::new(),
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
            indexed: Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Sunset", "the last light")
                        .invoking(reveal)
                        .grouped("Timeline markers")
                        .subtitled("0:21 · Drone")
                        .with_icon("star")
                        .boosted(5),
                    SearchEntry::new("Sunset take", ""),
                ],
                0,
            )),
            scopes: Vec::new(),
            queries: Vec::new(),
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
        let search = ResourceSearch::new(
            "Tracks",
            Arc::new(IndexedEntries::built(
                (0..257)
                    .map(|n| SearchEntry::new(format!("Common track {n}"), ""))
                    .collect(),
                0,
            )),
        );
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = search.search_page(&SearchQuery::new("common"), cursor.as_ref(), 17, 5);
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
        let search = ResourceSearch::new(
            "Tables",
            Arc::new(IndexedEntries::built(
                (0..300)
                    .map(|n| SearchEntry::new(format!("table {n}"), ""))
                    .collect(),
                0,
            )),
        );
        assert_eq!(
            search.search_limited("", usize::MAX).len(),
            DEFAULT_SEARCH_RESULT_LIMIT
        );
        assert_eq!(
            search
                .search_page(&SearchQuery::new(""), None, usize::MAX, usize::MAX)
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
        let search = ResourceSearch::new(
            "Library",
            Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Storm Tales", "")
                        .invoking(tilo_actions::ActionAddress::new("book:1", "open"))
                        .grouped("Books")
                        .with_body(body.clone()),
                ],
                0,
            )),
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

    #[test]
    fn a_field_ranks_by_its_own_weight() {
        use tilo_resource::SearchWeight;
        let search = ResourceSearch::new(
            "Mail",
            Arc::new(IndexedEntries::built(
                vec![
                    SearchEntry::new("Lunch plans", "")
                        .field("ada@example.org", SearchWeight::Metadata),
                    SearchEntry::new("Quarterly report", "")
                        .field("ada@example.org", SearchWeight::Identity),
                ],
                0,
            )),
        );
        let hits = search.search("ada@example.org");
        assert_eq!(hits.len(), 2, "both are found by the address");
        assert_eq!(
            hits[0].title, "Quarterly report",
            "a match in an identity outranks one in metadata"
        );
    }
}

// Compiled only into native tests: the file's own `cfg` says so.
mod publication_tests;
