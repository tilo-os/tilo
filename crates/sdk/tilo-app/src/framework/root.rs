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
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, PagePoll, PendingToken,
    SearchCursor, SearchPage, SearchProvider, SearchQuery, SearchResult,
};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::view::ThawError;
use ui_core::widget::capability::{capability, capability_mut};
use ui_core::widget::{AppKind, AppWidget, Decorator, Element, IconDescriptor, Widget};
use ui_core::{LocalState, Theme};

use super::dispatch::DispatchCore;
use super::rows::{find, join, merge, outside_document};
use super::scenario::Run;
use super::stage::Stages;
use super::surface::SurfacePublication;
use super::view::ViewCx;

mod shared_tree;
mod typed;
mod widget;
use shared_tree::{ItemChildren, NestedViews, PublishedChildren, RecordChildren, SharedTreeCache};
use typed::TypedRoot;
pub(crate) use typed::{ResourceType, Typed};
pub(crate) use widget::{RootApp, RootControls, RootModel, RootPublishes, RootSearch, RootWidget};

pub(crate) struct ResourceRoot {
    title: &'static str,
    icon: IconDescriptor,
    label: String,
    /// The resource type the root shows, and what it says of itself.
    resource: ResourceType,
    /// What depends on the resource type: its live state, the views'
    /// updaters and bound edits, the dispatcher and the effects. Everything
    /// else here is compiled once in the shared runtime.
    typed: Box<dyn TypedRoot>,
    /// What bound controls look up ([`super::controls::Actions`]).
    action_bindings: super::controls::ActionTable,
    /// The live revision bound controls last followed.
    followed: std::cell::Cell<u64>,
    _alive: Rc<()>,
    pub(crate) dispatch: DispatchCore,
    saved: LocalState<RecordSaveStatus>,
    /// The window's save status over the view: the record's durability and
    /// the app's own saves, silent unless a save stalls or fails.
    save: super::saves::SaveIndicator,
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
    search: Rc<std::cell::RefCell<SearchCache>>,
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
    /// The children with the records kept beside the resource, as the shared
    /// publication last carried them.
    record_children: std::cell::RefCell<Option<RecordChildren>>,
    /// The head bound controls were last refreshed from, kept between syncs
    /// for a resource whose publication may be reused
    /// ([`Resource::publication_cache_epoch`]).
    binding_head: std::cell::RefCell<Option<BindingHead>>,
    /// What bound controls share of the publication at one head: built
    /// once per head, never copied per refresh.
    binding_base: std::cell::RefCell<Option<Rc<BindingBase>>>,
    /// The view operation types of the nested resources at one live revision.
    nested_views: std::cell::RefCell<Option<NestedViews>>,
    /// Whether the resource keeps document state History shows.
    history: bool,
    /// Platform presentation actions; the browser implementation has no state.
    native_presentation: crate::runner::Presentation,
    /// The links `share-link` is minting.
    share: super::share_link::ShareLinks,
}

impl ui_core::component_lifecycle::WindowActivation for ResourceRoot {
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

/// The label of the item view the framework gives an item resource that
/// presents no view of its own.
const ITEM_VIEW_LABEL: &str = "Item";

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

/// Offer `open` on every item resource under `resource` (an item of the
/// collections of the resource `item_of` knows the items of), bound to that
/// item.
fn offer_item_open(item_of: fn(&str) -> bool, resource: &mut tilo_actions::Resource) {
    if item_of(resource.id.as_str())
        && !resource
            .actions
            .iter()
            .any(|existing| existing.id == "open")
    {
        resource.actions.push(item_open_spec(resource.id.as_str()));
        // The opener is also a presentation binding, not just a verb in the
        // catalogue. Keep an app's explicit views; otherwise advertise the
        // item view that this exact bound opener actually shows. The view
        // names what it shows, the item, and not which item: the item's own
        // label is the resource's, published once with it. Every item's
        // `open` is then one catalogue entry however many items there are,
        // where a copy of the item's label made each item's `open`, and its
        // whole action template, an entry of its own.
        if resource.views.is_empty() {
            resource.views.push(
                tilo_actions::ResourceViewSpec::new("item", ITEM_VIEW_LABEL, "open")
                    .with_primary(true),
            );
        }
    }
    for child in &mut resource.children {
        offer_item_open(item_of, child);
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
    ids.push(tilo_resource::share::SHARE_LINK.to_owned());
    ids.push(tilo_resource::share::SHARE_INVITE_LINK.to_owned());
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
    read_spec_for(None)
}

/// Bind a document read to one of the readable resource's own children.
/// These declarations never apply to live surface rows or external resources.
fn offer_document_reads(resource: &mut tilo_actions::Resource, eligible: &dyn Fn(&str) -> bool) {
    if eligible(resource.id.as_str()) && !resource.actions.iter().any(|spec| spec.id == "read") {
        resource
            .actions
            .push(read_spec_for(Some(resource.id.as_str())));
    }
    for child in &mut resource.children {
        offer_document_reads(child, eligible);
    }
}

fn read_spec_for(item: Option<&str>) -> tilo_actions::ActionSpec {
    let item = item.map(str::to_owned);
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
            tilo_actions::ActionParam::optional(
                "version",
                tilo_actions::ParamType::Text,
                "A change id, exact causal frontier, or checkpoint name (default: current)",
            )
            .nullable(),
        ]
        .into_iter()
        .filter(|param| item.is_none() || param.name != "item")
        .collect(),
        move |args| {
            tilo_actions::EncodedAction::encode_operation(
                READ_TYPE,
                &serde_json::json!({ "item": item.as_deref().or_else(|| args.text("item")), "version": args.text("version") }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: READ_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "What this holds as typed data: with no item, the resource and the ids of what it contains; with an item id, that item's content. With version, read frozen historical content and its exact heads without changing the live resource.",
    );
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.reads_document()
}

/// Answer a `read` request against the committed document of the resource
/// `typed` reads, whose state is `state`.
fn read(
    typed: &dyn TypedRoot,
    state: &dyn std::any::Any,
    doc: &AutoCommit,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    let args: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
    };
    let item = args.get("item").and_then(serde_json::Value::as_str);
    let value = match args.get("version").and_then(serde_json::Value::as_str) {
        Some(version) => {
            let heads = ui_core::history_source::steps::resolve(doc, version)?;
            let frozen = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| error.to_string())?;
            let past = typed.read(&frozen).map_err(|error| error.to_string())?;
            let content = typed.read_content(past.as_ref(), &frozen, item)?;
            super::history::review::bounded(serde_json::json!({
                "version": heads.iter().map(ToString::to_string).collect::<Vec<_>>().join("+"),
                "heads": heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "content": content,
            }))?
        }
        None => typed.read_content(state, doc, item)?,
    };
    Ok(tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

impl ResourceRoot {
    /// The window ground is the framework's: an app never fills it, and one
    /// that paints nothing still stands on it.
    fn paint_ground(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.native_presentation.track_theme(theme);
        scene.fill(
            ui_core::vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.content.get_rect(),
        );
        self.run.people.follow(presence);
    }

    /// A root over `content`, the view built in `cx`, whose handles the root
    /// keeps; `alive` is the scope `cx`'s bindings are tied to. Only this
    /// constructor knows `R`.
    pub(crate) fn new<R: Resource>(
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
        let dispatch = env.dispatch.core().clone();
        let typed = Typed {
            live: env.live,
            updaters,
            dispatch: env.dispatch,
            bindings,
            effects: env.effects,
        };
        Self::assemble(
            (title, icon, label),
            ResourceType::of::<R>(),
            Box::new(typed),
            action_bindings.table(),
            Parts {
                alive,
                dispatch,
                saved: env.saved,
                saves: env.saves,
                stages,
                feeds,
                run: env.run,
                storage: env.storage,
                content,
                native_presentation,
            },
        )
    }

    /// [`Self::new`] past what names the resource type.
    fn assemble(
        (title, icon, label): (&'static str, IconDescriptor, String),
        resource: ResourceType,
        typed: Box<dyn TypedRoot>,
        action_bindings: super::controls::ActionTable,
        parts: Parts,
    ) -> Self {
        let Parts {
            alive,
            dispatch,
            saved,
            saves,
            stages,
            feeds,
            run,
            storage,
            content,
            native_presentation,
        } = parts;
        Self {
            title,
            icon,
            label,
            resource,
            typed,
            action_bindings,
            followed: std::cell::Cell::new(0),
            _alive: alive,
            dispatch,
            saved,
            save: super::saves::SaveIndicator::new(saves),
            permissions: None,
            write: None,
            stages,
            feeds,
            run,
            storage,
            content,
            unreadable: None,
            swapped: false,
            presentation: Vec::new(),
            search: Rc::default(),
            publication_tree: std::cell::RefCell::new(None),
            item_children: std::cell::RefCell::new(None),
            published_children: std::cell::RefCell::new(None),
            record_children: std::cell::RefCell::new(None),
            binding_head: std::cell::RefCell::new(None),
            binding_base: std::cell::RefCell::new(None),
            nested_views: std::cell::RefCell::new(None),
            history: true,
            native_presentation,
            share: super::share_link::ShareLinks::default(),
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
                let (key, payload) = at.presence_of(self.resource.kind, self.dispatch.app_id());
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
        let _profile = tilo_resource::profiling::Phase::start("framework.access_refresh");
        if let (Some(permissions), Some(write)) = (&self.permissions, &self.write) {
            self.dispatch.set_read_only(!permissions.check(write));
        }
        self.dispatch.refresh_permission_states();
    }

    /// The root as the widget a host mounts.
    pub(crate) fn into_widget(self) -> Box<dyn AppWidget> {
        RootWidget::boxed(Box::new(self))
    }

    /// The root as the widget a past version's preview draws. Boxed here,
    /// not where the resource's type is known, so its widget table is
    /// compiled once for every app.
    pub(crate) fn into_root_widget(self) -> Box<dyn Widget> {
        Box::new(RootWidget::new(Box::new(self)))
    }

    /// The handles of the root that name its resource type, when it is `R`.
    pub(crate) fn typed<R: Resource>(&self) -> Option<&Typed<R>> {
        self.typed.as_any().downcast_ref::<Typed<R>>()
    }

    /// What the launch that opened the window named.
    pub(crate) fn launch(&self) -> super::launch::LaunchInput<'_> {
        self.run.launch()
    }

    /// Every hosted surface of this resource in the view, in tree order.
    fn surfaces(&self, visit: &mut dyn FnMut(&dyn SurfacePublication)) {
        fn walk(
            widget: &dyn Widget,
            resource: std::any::TypeId,
            visit: &mut dyn FnMut(&dyn SurfacePublication),
        ) {
            if let Some(surface) =
                ui_core::widget::capability::capability::<dyn SurfacePublication>(widget)
                && surface.resource() == resource
            {
                visit(surface);
            }
            widget.for_each_child(&mut |child| walk(child, resource, visit));
        }
        walk(self.content.as_ref(), self.resource.id, visit);
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
        fn bind(
            typed: &dyn TypedRoot,
            rows: &mut [tilo_actions::Resource],
            offered: &mut std::collections::HashMap<String, Box<dyn std::any::Any>>,
        ) {
            for row in rows {
                if let Some(decls) = offered.remove(row.id.as_str()) {
                    let bound = std::mem::replace(row, tilo_actions::Resource::new("", "", ""));
                    *row = typed.bind(bound, decls);
                }
                bind(typed, &mut row.children, offered);
            }
        }
        if rows.is_empty() {
            return;
        }
        let mut all = Vec::new();
        ids(rows, &mut all);
        let mut offered: std::collections::HashMap<String, Box<dyn std::any::Any>> =
            std::collections::HashMap::new();
        let mut spare: Option<Box<dyn std::any::Any>> = None;
        self.surfaces(&mut |surface| {
            for id in &all {
                if let Some(decls) = offered.get_mut(id.as_str()) {
                    surface.row_declarations(id, decls.as_mut());
                    continue;
                }
                let decls = spare.get_or_insert_with(|| self.typed.no_declarations());
                if surface.row_declarations(id, decls.as_mut())
                    && let Some(decls) = spare.take()
                {
                    offered.insert(id.clone(), decls);
                }
            }
        });
        bind(self.typed.as_ref(), rows, &mut offered);
    }

    /// The resource as published: its own actions and children, the edits
    /// views and surfaces bound to where the user is, the live rows surfaces
    /// show, and the framework's operations every app answers.
    #[inline(never)]
    fn publication(&self) -> tilo_actions::Resource {
        let _profile = tilo_resource::profiling::Phase::start("framework.publication");
        let state = self.typed.state();
        let tree = self.shared_document_tree(&*state).to_resource();
        self.decorate_publication(&*state, tree, Decoration::Tree)
    }

    /// The document-owned part of what controls bind to: the live
    /// resource's identity and own actions, without its children. Taken
    /// from the cached tree when it holds this change; else published
    /// without building a child, so it costs the same however many items
    /// the resource holds.
    #[inline(never)]
    fn binding_head(&self) -> BindingHead {
        let state = self.typed.state();
        let revision = (
            self.typed.revision(),
            ui_core::i18n::I18nManager::revision(),
        );
        let epoch = self.typed.cache_epoch(&*state);
        let cached = epoch.as_ref().and_then(|epoch| {
            self.publication_tree
                .borrow()
                .as_ref()
                .filter(|cached| cached.revision == (revision.0, revision.1, epoch.clone()))
                .map(|cached| cached.tree.head().clone())
        });
        let resource = cached.unwrap_or_else(|| {
            let _profile = tilo_resource::profiling::Phase::start("framework.resource_head");
            self.typed
                .head(self.dispatch.resource_id(), &self.label, &*state)
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
        let current = self.typed.cache_epoch(&*self.typed.state());
        (head.epoch.is_some() && head.epoch == current).then_some(head)
    }

    /// What bound controls look up ([`super::controls::Actions`]), as one
    /// resource: the published resource's identity and actions over `head`,
    /// exactly as [`Self::publication`] publishes them, without building or
    /// decorating its children.
    #[cfg(test)]
    fn binding_publication(&self, head: &BindingHead) -> tilo_actions::Resource {
        let base = self.binding_base(head);
        let overlay = self.binding_overlay(&base);
        let mut resource = (*base.resource).clone();
        for spec in overlay {
            match resource.actions.iter_mut().find(|own| own.id == spec.id) {
                Some(own) => *own = spec,
                None => resource.actions.push(spec),
            }
        }
        resource
    }

    /// The part of what bound controls look up that only `head` and the
    /// framework decide: the head with the presentation's, the framework's
    /// and History's actions, as [`Self::decorate_publication`] adds them.
    /// Built once per head and kept, so a refresh shares it.
    fn binding_base(&self, head: &BindingHead) -> Rc<BindingBase> {
        let key = BaseKey {
            revision: head.revision,
            epoch: head.epoch.clone(),
            causal: self.typed.has_causal_drains(),
            unreadable: self.typed.unreadable(),
        };
        if let Some(base) = self
            .binding_base
            .borrow()
            .as_ref()
            .filter(|base| base.key == key)
        {
            return Rc::clone(base);
        }
        let mut resource = head.resource.clone();
        let head_len = resource.actions.len();
        resource = self.with_presentation_specs(resource);
        let shadow_len = resource.actions.len();
        resource = self.with_framework_specs(resource);
        if key.causal {
            mark_document_jobs(&mut resource);
        }
        if let Some(unreadable) = &key.unreadable {
            withhold_document_actions(&mut resource, unreadable);
        }
        let base = Rc::new(BindingBase {
            key,
            resource: Rc::new(resource),
            head_len,
            shadow_len,
        });
        *self.binding_base.borrow_mut() = Some(Rc::clone(&base));
        base
    }

    /// What the views and surfaces add over `base` now, as
    /// [`Self::decorate_publication`] adds it: the edits they bound, their
    /// availability, and the app's effects. Few actions, built per refresh.
    fn binding_overlay(&self, base: &BindingBase) -> Vec<tilo_actions::ActionSpec> {
        let state = self.typed.state();
        let mut bound = {
            let _profile = tilo_resource::profiling::Phase::start("framework.bound_declarations");
            self.typed.declarations(&*state)
        };
        let mut availability = Vec::new();
        self.surfaces(&mut |surface| {
            let _profile = tilo_resource::profiling::Phase::start("framework.surface_publication");
            surface.bound_declarations(bound.as_mut());
            availability.extend(surface.availability());
        });
        let own = base
            .resource
            .actions
            .get(..base.head_len)
            .unwrap_or_default();
        let shadows = base
            .resource
            .actions
            .get(..base.shadow_len)
            .unwrap_or_default();
        // The head's actions the edits bind over, refined where they say so.
        let ids = self.typed.declaration_ids(&*bound);
        let mut overlay = tilo_actions::Resource::new("", "", "");
        overlay.actions = own
            .iter()
            .filter(|spec| ids.contains(&spec.id.as_str()))
            .cloned()
            .collect();
        overlay = self.typed.bind(overlay, bound);
        for status in &availability {
            if !overlay.actions.iter().any(|spec| spec.id == status.id())
                && let Some(spec) = own.iter().find(|spec| spec.id == status.id())
            {
                overlay.actions.push(spec.clone());
            }
            for spec in overlay
                .actions
                .iter_mut()
                .filter(|spec| spec.id == status.id())
            {
                status.overlay(spec);
            }
        }
        let effects = {
            let _profile = tilo_resource::profiling::Phase::start("framework.effect_specs");
            self.typed.effect_specs()
        };
        for spec in effects {
            let shadowed = shadows
                .iter()
                .chain(&overlay.actions)
                .any(|existing| existing.id == spec.id);
            debug_assert!(!shadowed, "effect `{}` shadows an action", spec.id);
            if shadowed {
                tracing::warn!(effect = spec.id, "effect shadows an action; not published");
                continue;
            }
            overlay.actions.push(spec);
        }
        if base.key.causal {
            mark_document_jobs(&mut overlay);
        }
        if let Some(unreadable) = &base.key.unreadable {
            withhold_document_actions(&mut overlay, unreadable);
        }
        overlay.actions
    }

    /// Refresh the actions bound controls look up, reusing `head` while the
    /// live state and locale are those it was built at. Answers the head
    /// they were refreshed from.
    #[inline(never)]
    fn refresh_bindings(&self, head: Option<BindingHead>) -> BindingHead {
        let _profile = tilo_resource::profiling::Phase::start("framework.action_bindings");
        let revision = (
            self.typed.revision(),
            ui_core::i18n::I18nManager::revision(),
        );
        let head = match head {
            Some(head) if head.revision == revision => head,
            _ => self.binding_head(),
        };
        let base = self.binding_base(&head);
        let overlay = self.binding_overlay(&base);
        self.action_bindings
            .update_shared(Rc::clone(&base.resource), overlay);
        head
    }

    /// [`Self::publication`] of the resource as `doc` holds it now, which a
    /// change from another window or participant may have moved past what
    /// this view last synced.
    #[inline(never)]
    fn current_publication(&self, doc: &AutoCommit) -> tilo_actions::Resource {
        if !self.typed.follows(doc)
            && let Ok(current) = self.typed.read(doc)
        {
            // Publication can run after an edit and before the view syncs.
            // Validate its nested views against this same state, rather than
            // withdrawing the new resource tree using the previous frame's
            // handler table. The next live revision replaces this cache.
            self.pin_nested_views(&*current);
            return self.publication_of(&*current);
        }
        self.publication()
    }

    #[inline(never)]
    fn publication_of(&self, state: &dyn std::any::Any) -> tilo_actions::Resource {
        let _profile = tilo_resource::profiling::Phase::start("framework.publication");
        let tree = self.shared_document_tree(state).to_resource();
        self.decorate_publication(state, tree, Decoration::Tree)
    }

    /// Add to `resource`, the document-owned tree, what the view and the
    /// framework publish on it. [`Decoration::Actions`] decorates only the
    /// resource's own actions; its children stay as they are.
    #[inline(never)]
    fn decorate_publication(
        &self,
        state: &dyn std::any::Any,
        mut resource: tilo_actions::Resource,
        decoration: Decoration,
    ) -> tilo_actions::Resource {
        let children = decoration == Decoration::Tree;
        let mut bound = {
            let _profile = tilo_resource::profiling::Phase::start("framework.bound_declarations");
            self.typed.declarations(state)
        };
        let mut rows = Vec::new();
        let mut availability = Vec::new();
        self.surfaces(&mut |surface| {
            let _profile = tilo_resource::profiling::Phase::start("framework.surface_publication");
            surface.bound_declarations(bound.as_mut());
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
        resource = self.typed.bind(resource, bound);
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
            let _profile = tilo_resource::profiling::Phase::start("framework.row_declarations");
            self.bind_row_declarations(&mut resource.children);
        }
        resource = self.with_presentation_specs(resource);
        // The app's own effects come before the framework's history and
        // render operations: an app answering `diff` its own way is asked.
        let effects = {
            let _profile = tilo_resource::profiling::Phase::start("framework.effect_specs");
            self.typed.effect_specs()
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
        resource = self.with_framework_specs(resource);
        if self.typed.has_causal_drains() {
            mark_document_jobs(&mut resource);
        }
        if let Some(unreadable) = self.typed.unreadable() {
            withhold_document_actions(&mut resource, &unreadable);
        }
        if children {
            resource.children.extend(self.storage.record_resources());
        }
        resource
    }

    /// `resource` with the view and effect operations of the app's
    /// presentation it does not offer itself.
    fn with_presentation_specs(
        &self,
        mut resource: tilo_actions::Resource,
    ) -> tilo_actions::Resource {
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
        resource
    }

    /// `resource` with the framework's render, `read` and History actions it
    /// does not offer itself.
    fn with_framework_specs(&self, mut resource: tilo_actions::Resource) -> tilo_actions::Resource {
        let specs = framework_specs();
        let read = self.resource.readable.then_some(&specs.read);
        let history = specs.history.iter().filter(|_| self.history);
        for spec in specs.native.iter().chain(read).chain(history) {
            if !resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
            {
                resource = resource.with_action(spec.clone());
            }
        }
        resource
    }

    /// Show why the record does not read as the resource instead of the
    /// view while it does not, never the last state as if it were current,
    /// and refuse submissions with the same reason.
    #[inline(never)]
    fn follow_readability(&mut self) {
        let reason = self.typed.unreadable();
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

    /// Follow the document when it moved — a local action, a
    /// collaborator's change, an undo or a restore — reading only what the
    /// change touched.
    ///
    /// Answers the document-owned head the bindings were refreshed from, for
    /// the refresh after the view synced to reuse.
    fn refresh(&self, doc: &AutoCommit) -> BindingHead {
        if let Err(error) = self.typed.sync(doc) {
            tracing::warn!(%error, kind = self.resource.kind, "resource unreadable");
        }
        // Refresh the same declarations the host publishes before rebuilt
        // controls look up their bindings. Retained surface handles share it.
        let head = self.refresh_bindings(self.retained_head());
        self.typed.follow_change(doc, &self.followed);
        self.feeds.refresh(doc);
        self.typed.drain(self.typed.revision());
        head
    }
}

/// What [`ResourceRoot::new`] hands [`ResourceRoot::assemble`] of the view
/// it was built in.
struct Parts {
    alive: Rc<()>,
    dispatch: DispatchCore,
    saved: LocalState<RecordSaveStatus>,
    saves: super::saves::Saves,
    stages: Stages,
    feeds: super::history::HistoryFeeds,
    run: Run,
    storage: Rc<super::settings::ViewStorage>,
    content: Element,
    native_presentation: crate::runner::Presentation,
}

/// `widget`'s hosted surface, when it is one of the resource type
/// `resource`.
fn surface_mut(
    widget: &mut dyn Widget,
    resource: std::any::TypeId,
) -> Option<&mut (dyn SurfacePublication + 'static)> {
    capability_mut::<dyn SurfacePublication>(widget)
        .filter(|surface| surface.resource() == resource)
}

/// What [`ResourceRoot::binding_base`] is built from.
#[derive(Clone, PartialEq)]
struct BaseKey {
    revision: (u64, u64),
    epoch: Option<String>,
    causal: bool,
    unreadable: Option<String>,
}

/// The shared part of what bound controls look up, at one [`BaseKey`].
struct BindingBase {
    key: BaseKey,
    resource: Rc<tilo_actions::Resource>,
    /// How many of its actions are the head's own.
    head_len: usize,
    /// How many are the head's and the presentation's: what an effect of
    /// the same id would shadow.
    shadow_len: usize,
}

/// The framework's actions every app's resource offers unless it offers
/// them itself, at one locale.
struct FrameworkSpecs {
    native: Vec<tilo_actions::ActionSpec>,
    read: tilo_actions::ActionSpec,
    history: Vec<tilo_actions::ActionSpec>,
}

thread_local! {
    static FRAMEWORK_SPECS: std::cell::RefCell<Option<(u64, Rc<FrameworkSpecs>)>> =
        const { std::cell::RefCell::new(None) };
}

/// [`FrameworkSpecs`] at the current locale, built once per locale on
/// each thread rather than on every publication.
fn framework_specs() -> Rc<FrameworkSpecs> {
    let locale = ui_core::i18n::I18nManager::revision();
    FRAMEWORK_SPECS.with_borrow_mut(|kept| {
        if let Some((at, specs)) = kept.as_ref()
            && *at == locale
        {
            return Rc::clone(specs);
        }
        let specs = Rc::new(FrameworkSpecs {
            native: with_native_presentations(tilo_actions::Resource::new("", "", "")).actions,
            read: read_spec(),
            history: super::history::specs(),
        });
        *kept = Some((locale, Rc::clone(&specs)));
        specs
    })
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
    let root = root.downcast_mut::<ResourceRoot>()?;
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

impl ui_core::widget::document_effects::DocumentEffects for ResourceRoot {
    fn start_document_effects(
        &mut self,
        operation: &ui_core::host_operation::ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        self.typed.start_document_effects(operation, applied)
    }
}

/// How many search entries are indexed away from the UI thread rather than
/// in the frame that asks for them.
const OFF_THREAD_ENTRIES: usize = 1024;

/// What the resource's search entries are of: a revision of the resource
/// and the entries its surfaces show.
#[derive(PartialEq)]
struct SearchState {
    revision: u64,
    shown: Vec<SearchEntry>,
}

/// An index search answers from, and the state it is of: none for the empty
/// index answering before the first one arrives.
struct AnsweringIndex {
    of: Option<SearchState>,
    indexed: Arc<IndexedEntries>,
}

/// An index being built away from the UI thread.
struct BuildingIndex {
    of: SearchState,
    generation: u64,
    arriving: std::sync::mpsc::Receiver<IndexedEntries>,
}

/// The resource's search index, built only when someone searches and at
/// most one at a time.
///
/// Every state is given its index's generation when first seen, before
/// anything is built: the action directory names that generation on every
/// frame, and the index a later search builds carries it.
#[derive(Default)]
struct SearchCache {
    answering: Option<AnsweringIndex>,
    building: Option<BuildingIndex>,
    /// The newest state nothing is built of yet, and its generation: built
    /// when the next search asks once no build is in flight.
    wanted: Option<(SearchState, u64)>,
    /// Entries a showing search waits on, and their state and generation:
    /// indexed once the build in flight lands.
    queued: Option<(SearchState, u64, Vec<SearchEntry>)>,
    /// A search is showing that waits on its surfaces' answers.
    watching: bool,
    /// The generation that holds the surfaces' answers to the latest query:
    /// the waiting search shows what an index this new or newer finds.
    answered: Option<u64>,
    next_generation: u64,
    /// How many indexes of entries have been started.
    #[cfg(test)]
    builds: usize,
}

impl SearchCache {
    fn reserve_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = generation.wrapping_add(1);
        generation
    }

    /// Answer from an index that finished building since last asked, and
    /// start the one a waiting search wants once nothing is in flight.
    fn receive(&mut self) {
        self.land();
        if self.building.is_none()
            && let Some((state, generation, entries)) = self.queued.take()
        {
            self.build(state, generation, entries);
        }
    }

    fn land(&mut self) {
        let Some(building) = self.building.take() else {
            return;
        };
        match building.arriving.try_recv() {
            Ok(indexed) => {
                self.answering = Some(AnsweringIndex {
                    of: Some(building.of),
                    indexed: Arc::new(indexed),
                });
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => self.building = Some(building),
            // A lost build: its state is wanted afresh when next asked.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// The generation of the index that answers for `state`, given now if
    /// `state` is new. Builds nothing.
    fn generation_for(&mut self, state: SearchState) -> u64 {
        self.receive();
        if let Some(answering) = &self.answering
            && answering.of.as_ref() == Some(&state)
        {
            return answering.indexed.generation;
        }
        if let Some(building) = &self.building
            && building.of == state
        {
            return building.generation;
        }
        if let Some((wanted, generation)) = &self.wanted
            && *wanted == state
        {
            return *generation;
        }
        let generation = self.reserve_generation();
        self.wanted = Some((state, generation));
        generation
    }

    /// Index `entries`, of `state`: in this frame when they are few, else
    /// away from the UI thread. The index search answers from now.
    fn build(
        &mut self,
        state: SearchState,
        generation: u64,
        entries: Vec<SearchEntry>,
    ) -> Arc<IndexedEntries> {
        #[cfg(test)]
        {
            self.builds = self.builds.saturating_add(1);
        }
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
                self.building = Some(BuildingIndex {
                    of: state,
                    generation,
                    arriving,
                });
                return self.answering();
            }
        }
        let indexed = Arc::new(IndexedEntries::built(entries, generation));
        self.answering = Some(AnsweringIndex {
            of: Some(state),
            indexed: Arc::clone(&indexed),
        });
        indexed
    }

    /// The index holding the surfaces' answers to the waiting search, once
    /// it is in.
    fn answer(&mut self) -> Option<Arc<IndexedEntries>> {
        self.receive();
        let wanted = self.answered?;
        self.answering
            .as_ref()
            .filter(|answering| answering.of.is_some() && answering.indexed.generation >= wanted)
            .map(|answering| Arc::clone(&answering.indexed))
    }

    /// The index search answers from now: the empty one until a first
    /// arrives.
    fn answering(&mut self) -> Arc<IndexedEntries> {
        if let Some(answering) = &self.answering {
            return Arc::clone(&answering.indexed);
        }
        let generation = self.reserve_generation();
        let indexed = Arc::new(IndexedEntries::built(Vec::new(), generation));
        self.answering = Some(AnsweringIndex {
            of: None,
            indexed: Arc::clone(&indexed),
        });
        indexed
    }
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
    /// Where a search that waits on the surfaces' answers finds them: none
    /// for a fixed set of entries.
    live: Option<LiveSearch>,
}

/// The resource's index as it moves on, for a search that waits on its
/// surfaces' answers.
struct LiveSearch {
    cache: Rc<std::cell::RefCell<SearchCache>>,
    /// The index holding the answers, once they are in: later pages
    /// continue in it.
    answered: std::cell::RefCell<Option<Arc<IndexedEntries>>>,
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
            live: None,
        }
    }

    /// A token for the answers still coming from the surfaces, if any are:
    /// the search showing keeps asking for them.
    fn waiting(&self) -> Option<PendingToken> {
        let live = self.live.as_ref()?;
        if !self
            .queries
            .iter()
            .any(super::surface::SearchQueries::answering)
        {
            return None;
        }
        let mut cache = live.cache.borrow_mut();
        cache.watching = true;
        cache.answered = None;
        Some(PendingToken::new(self.indexed.generation))
    }

    /// The index this search answers from: the surfaces' answers once they
    /// are in.
    fn indexed(&self) -> Arc<IndexedEntries> {
        self.live
            .as_ref()
            .and_then(|live| live.answered.borrow().clone())
            .unwrap_or_else(|| Arc::clone(&self.indexed))
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
        Some(self.indexed().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        cursor.opaque_parts().0 == self.indexed().generation
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
        let pending = self.waiting();
        let limit = work_budget
            .min(DEFAULT_SEARCH_CANDIDATE_LIMIT)
            .min(result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT));
        let folded = format!(
            "{}\0{:?}",
            ui_core::search::normalize_search_text(query.text()),
            query.filters()
        );
        let indexed = self.indexed();
        let offset = cursor
            .map(SearchCursor::opaque_parts)
            .filter(|(generation, at_query, at_scope, _)| {
                *generation == indexed.generation
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
                pending,
            };
        }
        let (results, more, inspected) = self.results(query, Page { offset, limit });
        let next = more.then(|| {
            SearchCursor::opaque(
                indexed.generation,
                folded,
                query.scope().map(str::to_owned),
                u64::try_from(offset.saturating_add(inspected)).ok(),
            )
        });
        SearchPage {
            results,
            next,
            inspected,
            pending,
        }
    }

    // What the surfaces answer later (a server's results) reaches the
    // search showing them once all of them have answered: the latest query
    // asked again of the index holding the answers.
    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        let Some(live) = &self.live else {
            return PagePoll::Ready(SearchPage::empty());
        };
        if self
            .queries
            .iter()
            .any(super::surface::SearchQueries::answering)
        {
            return PagePoll::Pending;
        }
        let Some(answer) = live.cache.borrow_mut().answer() else {
            return PagePoll::Pending;
        };
        let _ = token;
        *live.answered.borrow_mut() = Some(answer);
        let Some(query) = self
            .queries
            .iter()
            .find_map(super::surface::SearchQueries::last)
        else {
            return PagePoll::Ready(SearchPage::empty());
        };
        PagePoll::Ready(self.search_page(
            &query,
            None,
            DEFAULT_SEARCH_CANDIDATE_LIMIT,
            DEFAULT_SEARCH_RESULT_LIMIT,
        ))
    }

    fn facets(&self) -> Vec<ui_core::search::Facet> {
        self.indexed().facets.clone()
    }

    fn search_scopes(&self) -> Vec<ScopeDecl> {
        self.scopes.clone()
    }
}

impl ResourceSearch {
    /// One page of hits for `query`, and whether more follow it.
    fn results(&self, query: &SearchQuery, page: Page) -> (Vec<SearchResult>, bool, usize) {
        let indexed = self.indexed();
        let found = indexed.index.search(query.text(), page);
        let inspected = found.hits.len();
        let results = found
            .hits
            .into_iter()
            .filter_map(|hit| {
                let parent = indexed.entries.get(usize::try_from(hit.key).ok()?)?;
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
impl RootModel for ResourceRoot {
    ui_core::provides!(dyn ui_core::undo::CommittedActions, dyn ui_core::history_mode::capability::DocumentHistory, dyn ui_core::widget::document_effects::DocumentEffects, dyn ui_core::widget::Collaborative, #[cfg(not(target_family = "wasm"))] dyn ui_core::component_lifecycle::WindowActivation; inner = content);

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

    // The root adds no structure of its own beyond swapping its view for
    // the unreadable state: a change its view can describe stays described.
    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange {
        let change = self.view_mut().take_structure_change();
        if std::mem::take(&mut self.swapped) {
            ui_core::widget::StructureChange::Rebuild
        } else {
            change
        }
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
            let _profile = tilo_resource::profiling::Phase::start("framework.content_sync");
            self.content.sync(state, path);
        }
        self.follow_search();
        // What the view's sync changed (a surface's bound edits, its
        // availability) reaches the bindings; the document-owned head is
        // rebuilt only when the sync moved the state.
        let head = self.refresh_bindings(Some(head));
        *self.binding_head.borrow_mut() = Some(head);
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId) {
        self.typed.apply_settlements(edits);
        self.content.settle(edits, path);
        self.storage.settle();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint_ground(scene, transform, theme, presence);
        self.content.draw(scene, transform, text, theme, presence);
        let area = self.content.get_rect();
        self.save
            .paint(scene, transform, text, theme, area, ui_core::time::now());
    }

    fn needs_redraw(&self) -> bool {
        self.native_presentation.needs_redraw()
            || self.content.needs_redraw()
            || self.save.needs_redraw(ui_core::time::now())
    }

    fn save_indicator(&self) -> &super::saves::SaveIndicator {
        &self.save
    }

    fn save_indicator_mut(&mut self) -> &mut super::saves::SaveIndicator {
        &mut self.save
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
        handlers.insert(super::chain::CHAIN_ACTION_TYPE.to_owned());
        handlers.insert(tilo_resource::share::SHARE_TYPE.to_owned());
        handlers.insert(tilo_resource::share::SHARE_INVITE_TYPE.to_owned());
        if self.stages.opens() {
            handlers.insert(OPEN_TYPE.to_owned());
        }
        // Opening another app is routed by the host to the workspace
        // broker, whichever resource offers it.
        handlers.insert(tilo_actions::remote::ApplicationLaunch::TYPE.to_owned());
        if self.resource.readable {
            handlers.insert(READ_TYPE.to_owned());
        }
        handlers.extend(
            self.typed
                .effect_operations()
                .into_iter()
                .map(str::to_owned),
        );
        for spec in (self.resource.local_operations)()
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
        if let Some((update, copied)) = self.share.poll(invocation, cancellation_requested) {
            if copied && let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &update {
                self.dispatch
                    .note_landed(tilo_resource::share::SHARE_LINK, output);
            }
            return Some(update);
        }
        self.typed
            .poll_effect(invocation, cancellation_requested)
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
        if self.resource.readable
            && let Some(request) = op.unscoped()
            && request.type_id == READ_TYPE
        {
            let output = read(
                self.typed.as_ref(),
                &*self.typed.state(),
                doc,
                request.payload,
            )?;
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        if let Some(request) = op.unscoped()
            && super::share_link::handles(request.type_id)
        {
            return self.start_share(request.invocation, request.payload);
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
        if let Some(output) = self.start_record_operation(op, doc)? {
            return Ok(Some(
                ui_core::host_operation::ResourceOperationStart::Completed(output),
            ));
        }
        // An effect reads the resource as the document stands, though no
        // frame has run since a collaborator's change arrived.
        if let Err(error) = self.typed.sync(doc) {
            tracing::warn!(%error, kind = self.resource.kind, "resource unreadable");
        }
        if let Some(started) = self.typed.start_effect(op)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.view_mut().start_operation(op, doc, path)? {
            return Ok(Some(started));
        }
        Ok(self.start_catalogue_view(op))
    }
}

impl ResourceRoot {
    /// The record-level requests every framework app answers the same way:
    /// its history at a version, an operation on one of its records or its
    /// chain of parts, and the person's own record history.
    fn start_record_operation(
        &self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &ui_core::widget::Edits<'_>,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        let output = if request.type_id == super::history::VIEW_TYPE {
            super::history::answer(doc, request.payload)?
        } else if request.type_id == super::record::RECORD_ACTION_TYPE {
            self.storage
                .record_action(self.dispatch.me(), request.payload)?
        } else if request.type_id == super::chain::CHAIN_ACTION_TYPE {
            self.storage
                .chain_action(self.dispatch.me(), request.payload)?
        } else if request.type_id == super::person::HISTORY_TYPE {
            self.storage.person_history(request.payload)?
        } else {
            return Ok(None);
        };
        Ok(Some(output))
    }

    /// What the resource's search entries are of now, and the surfaces'
    /// query sinks. What the surfaces show (a page's text, live rows)
    /// changes on its own, beside the resource's revision.
    fn search_state(&self) -> (SearchState, Vec<super::surface::SearchQueries>) {
        let mut shown = Vec::new();
        let mut queries = Vec::new();
        self.surfaces(&mut |surface| {
            shown.extend(surface.search_entries());
            queries.push(surface.search_queries());
        });
        let state = SearchState {
            revision: self.typed.revision(),
            shown,
        };
        (state, queries)
    }

    /// The index search answers `state` from, building it if nothing is. A
    /// large set of entries (a browsing history, a store catalogue) or one
    /// with prose bodies (whole books) is indexed away from the UI thread,
    /// one build at a time: until the index of `state` arrives, search
    /// answers from the one before it, and states that come and go while a
    /// build is in flight cost only the newest one's build after it.
    fn current_search_index(&self, state: SearchState) -> Arc<IndexedEntries> {
        let mut cache = self.search.borrow_mut();
        let generation = cache.generation_for(state);
        if let Some(answering) = &cache.answering
            && answering.of.is_some()
            && answering.indexed.generation == generation
        {
            return Arc::clone(&answering.indexed);
        }
        if cache.building.is_some() {
            return cache.answering();
        }
        let Some((state, generation)) = cache.wanted.take() else {
            return cache.answering();
        };
        let entries = self.entries_of(&state);
        cache.build(state, generation, entries)
    }

    /// Everything search finds in `state`: the resource's entries and what
    /// its surfaces show.
    fn entries_of(&self, state: &SearchState) -> Vec<SearchEntry> {
        let mut entries = self.typed.search(&*self.typed.state());
        entries.extend(state.shown.iter().cloned());
        entries
    }

    /// A search that waits on its surfaces' answers (a server still
    /// looking) gets an index of what they show once every one of them has
    /// answered the latest query.
    fn follow_search(&self) {
        if !self.search.borrow().watching {
            return;
        }
        let mut answering = false;
        self.surfaces(&mut |surface| answering |= surface.search_queries().answering());
        if answering {
            return;
        }
        let (state, _) = self.search_state();
        let mut cache = self.search.borrow_mut();
        cache.watching = false;
        let generation = cache.generation_for(state);
        cache.answered = Some(generation);
        if cache.building.is_some() {
            // Indexed once the build in flight lands.
            if let Some((state, wanted)) = cache.wanted.take() {
                let entries = self.entries_of(&state);
                cache.queued = Some((state, wanted, entries));
            }
            return;
        }
        if let Some((state, wanted)) = cache.wanted.take() {
            let entries = self.entries_of(&state);
            cache.build(state, wanted, entries);
        }
    }
}

impl ResourceRoot {
    fn start_catalogue_view(
        &self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
    ) -> Option<ui_core::host_operation::ResourceOperationStart> {
        // View openers from the resource/presentation catalogue and from its
        // nested resources (e.g. a rich-text body's `tilo.rich-text.view`
        // editor/compact) are already mounted; accept them so agents and
        // Ctrl+K can switch without a missing-handler fault.
        let request = op.unscoped()?;
        if request.kind != tilo_actions::OperationKind::View {
            return None;
        }
        let known = (self.resource.local_operations)()
            .into_iter()
            .chain(self.presentation.iter().cloned())
            .any(|spec| spec.operation_type.as_deref() == Some(request.type_id))
            || self.nested_views().contains(request.type_id);
        known.then(|| {
            ui_core::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )
        })
    }

    /// Start minting a link to one of the resources this root publishes.
    fn start_share(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        let record = self
            .run
            .launched
            .record
            .as_ref()
            .map(|record| record.as_str().to_owned());
        let publisher = super::share_link::Publisher {
            app: self.dispatch.app_id(),
            record: record.as_deref(),
            root: self.dispatch.resource_id(),
        };
        let may_invite = self.dispatch.may_invite();
        let started = self
            .share
            .start(invocation, payload, &publisher, may_invite)?;
        Ok(Some(match started {
            Some(minted) => {
                if minted.copied {
                    self.dispatch
                        .note_landed(tilo_resource::share::SHARE_LINK, &minted.output);
                }
                ui_core::host_operation::ResourceOperationStart::Completed(minted.output)
            }
            None => ui_core::host_operation::ResourceOperationStart::Accepted,
        }))
    }
}

// The root's own client for the action door (minting links) shares the
// app's collaboration connection with whatever the view keeps there.
impl ui_core::widget::Collaborative for ResourceRoot {
    fn take_collab_requests(&mut self) -> Vec<ui_core::collaboration::CollabRequest> {
        let mut requests =
            ui_core::widget::Collaborative::take_collab_requests(self.share.remote_mut());
        if let Some(content) =
            capability_mut::<dyn ui_core::widget::Collaborative>(self.content.as_mut())
        {
            requests.extend(content.take_collab_requests());
        }
        requests
    }

    fn handle_collab_event(&mut self, event: &ui_core::collaboration::CollabEvent) {
        ui_core::widget::Collaborative::handle_collab_event(self.share.remote_mut(), event);
        if let Some(content) =
            capability_mut::<dyn ui_core::widget::Collaborative>(self.content.as_mut())
        {
            content.handle_collab_event(event);
        }
    }
}

impl RootSearch for ResourceRoot {
    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        // The index is rebuilt only when the resource or what its surfaces
        // show moved since the last one.
        let (state, queries) = self.search_state();
        let indexed = self.current_search_index(state);
        // The app's one search source: what its content widgets show beside
        // the resource reaches it as their surfaces' entries, never as a
        // provider of their own.
        let mut source = ResourceSearch::new(self.resource.label, indexed);
        source.scopes = self.typed.search_scopes(&*self.typed.state());
        source.queries = queries;
        source.live = Some(LiveSearch {
            cache: Rc::clone(&self.search),
            answered: std::cell::RefCell::new(None),
        });
        vec![Box::new(source)]
    }

    /// The one source [`Self::search_providers`] makes, named with the
    /// generation its index has or will have once someone searches: the
    /// action directory reads this on every frame, and indexing whole books
    /// per keystroke for a search nobody runs is what it must not cost.
    fn search_provider_descriptors(
        &self,
    ) -> Vec<ui_core::search::capability::SearchProviderDescriptor> {
        let (state, _) = self.search_state();
        let generation = self.search.borrow_mut().generation_for(state);
        vec![ui_core::search::capability::SearchProviderDescriptor::new(
            self.resource.label,
            Some(generation),
            // `ResourceSearch` keeps the provider default.
            ui_core::widget::ActionDirectoryInvocationDomain::Unsupported,
        )]
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

impl RootPublishes for ResourceRoot {
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
        let surface = ui_core::widget::capability::capability::<dyn SurfacePublication>(
            self.content.as_ref(),
        )
        .is_some_and(|surface| surface.resource() == self.resource.id);
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
            offer_item_open(self.resource.item_of, &mut resource);
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
        let current = if self.typed.follows(doc) {
            None
        } else {
            self.typed.read(doc).ok()
        };
        let state = current.unwrap_or_else(|| self.typed.state());
        let mut child = self.typed.child(&*state, doc, id);
        if self.resource.readable
            && let Some(child) = &mut child
        {
            offer_document_reads(child, &|id| self.typed.document_child(&*state, id));
        }
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
                offer_item_open(self.resource.item_of, &mut resolved);
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
            || (self.resource.item_of)(address.resource.as_str())
        {
            if super::history::is_edit(&address.action) {
                return super::history::result(&address.action, doc, changes);
            }
            // What the action minted rides beside its own answer, the
            // resource's or else its surface's. An answer that is not an
            // object (an editor's summary naming the blocks it made) already
            // says what it made and is kept as it is.
            let minted = tilo_resource::take_minted();
            let answer = self
                .typed
                .answer(doc, &address.action, changes)
                .or_else(|| {
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

impl RootControls for ResourceRoot {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }
    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<ui_core::widget::controls::AttributedControlInvocation> {
        let taken = self.dispatch.take_attributed().or_else(|| {
            ui_core::widget::controls::take_attributed_control_invocation(self.content.as_mut())
        });
        if let Some((invocation, _)) = &taken {
            self.share.note_view_invocation(invocation);
        }
        taken
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
                self.typed.represent();
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
impl ui_core::history_mode::capability::DocumentHistory for ResourceRoot {
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
        self.typed.text_policy(previous, next)
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
            self.typed.note_applied(&participant, action);
        }
        if let Some(content) = ui_core::widget::capability::capability_mut::<
            dyn ui_core::history_mode::capability::DocumentHistory,
        >(self.content.as_mut())
        {
            content.document_action_applied(action, before, doc, path);
        }
    }
}

impl ui_core::undo::CommittedActions for ResourceRoot {
    fn take_committed_action(&mut self) -> Option<Vec<ui_core::automerge::ChangeHash>> {
        self.dispatch.take_committed()
    }
    fn keeps_undo_step_open(&self) -> bool {
        self.dispatch.keeps_step_open()
    }
}

impl RootApp for ResourceRoot {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &str {
        self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        self.icon.clone()
    }

    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        fn perform(
            widget: &mut dyn Widget,
            resource: std::any::TypeId,
            operation: &ui_core::floating_surface::DeviceShellOperation,
        ) -> Option<Result<serde_json::Value, String>> {
            if let Some(surface) = surface_mut(widget, resource)
                && surface.supports_device_shell()
            {
                return Some(surface.device_shell_operation(operation));
            }
            widget
                .children_mut()
                .into_iter()
                .find_map(|child| perform(child, resource, operation))
        }
        perform(self.content.as_mut(), self.resource.id, operation)
            .unwrap_or_else(|| Err("Terminal is not available".into()))
    }

    fn request_device_shell(&mut self) -> bool {
        fn request(widget: &mut dyn Widget, resource: std::any::TypeId) -> bool {
            if let Some(surface) = surface_mut(widget, resource)
                && surface.request_device_shell()
            {
                return true;
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| request(child, resource))
        }
        request(self.content.as_mut(), self.resource.id)
    }

    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        fn take(
            widget: &mut dyn Widget,
            resource: std::any::TypeId,
        ) -> Option<ui_core::widget::AppHostIntent> {
            if let Some(surface) = surface_mut(widget, resource)
                && let Some(intent) = surface.take_host_intent()
            {
                return Some(intent);
            }
            widget
                .children_mut()
                .into_iter()
                .find_map(|child| take(child, resource))
        }
        take(self.content.as_mut(), self.resource.id)
    }

    fn take_save_retry(&mut self) -> bool {
        fn take(widget: &mut dyn Widget, resource: std::any::TypeId) -> bool {
            let own =
                surface_mut(widget, resource).is_some_and(SurfacePublication::take_save_retry);
            // Consume every surface's one-shot even when another requested it.
            widget
                .children_mut()
                .into_iter()
                .fold(own, |retry, child| take(child, resource) || retry)
        }
        let asked = self.save.take_record_retry();
        take(self.content.as_mut(), self.resource.id) || asked
    }

    fn record_save_status(&mut self, status: RecordSaveStatus) {
        self.save.record(&status, ui_core::time::now());
        self.saved.set(status);
    }

    // The room refused what this record sent, and the record withdrew it:
    // the view shows the stored state again, and the action reads as
    // refused, as one this device refused does.
    fn record_refused(&mut self, refusal: &ui_core::record::RecordRefusal) {
        self.dispatch.refuse_by_room(refusal);
        self.typed.represent();
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
            .with_widget_mut(|root: &mut ResourceRoot| {
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

    thread_local! {
        static LOOKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static FOUND: RefCell<Vec<SearchEntry>> = const { RefCell::new(Vec::new()) };
    }

    /// A surface that asks a server what the search asks, which answers later.
    struct ServerSurface(ui_core::widget::Text);

    impl ui_core::widget::Decorator for ServerSurface {
        type Inner = ui_core::widget::Text;
        fn inner(&self) -> &Self::Inner {
            &self.0
        }
        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.0
        }
    }

    impl crate::framework::ResourceSurface<Mailbox> for ServerSurface {
        fn present(&mut self, _: &Rc<Mailbox>) {}
        fn search_query(&mut self, _: &SearchQuery) {
            LOOKING.with(|looking| looking.set(true));
        }
        fn search_pending(&self) -> bool {
            LOOKING.with(std::cell::Cell::get)
        }
        fn search_entries(&self) -> Vec<SearchEntry> {
            FOUND.with_borrow(Clone::clone)
        }
    }

    struct ServerApp;
    impl ResourceApp for ServerApp {
        type Resource = Mailbox;
        const ID: &'static str = "so.tilo.test-server-search";
        const TITLE: &'static str = "Server";
        fn view(cx: &mut ViewCx<Mailbox>) -> Element {
            cx.surface(ServerSurface(ui_core::widget::Text::new("Results")))
        }
    }

    #[test]
    fn a_showing_search_waits_for_what_a_server_finds_and_shows_it() {
        LOOKING.with(|looking| looking.set(false));
        FOUND.with_borrow_mut(Vec::clear);
        let mut app = TestApp::<ServerApp>::new().unwrap();
        let query = SearchQuery::new("archive");
        let (search, first) = app
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                let mut providers = RootSearch::search_providers(root);
                let search = providers.remove(0);
                let first = search.search_page(&query, None, 10, 10);
                (search, first)
            })
            .unwrap();
        assert!(first.results.is_empty(), "nothing held matches yet");
        let token = first.pending.expect("the surface has yet to answer");
        assert!(
            matches!(search.poll_page(&token), PagePoll::Pending),
            "not yet asked"
        );
        app.settle();
        assert!(LOOKING.with(std::cell::Cell::get), "the server was asked");
        assert!(
            matches!(search.poll_page(&token), PagePoll::Pending),
            "the server is still looking"
        );

        FOUND.with_borrow_mut(|found| {
            found.push(SearchEntry::new("From the archive", "an old letter").anchored("msg:9"));
        });
        LOOKING.with(|looking| looking.set(false));
        app.settle();
        let PagePoll::Ready(answered) = search.poll_page(&token) else {
            panic!("the server answered");
        };
        assert!(answered.pending.is_none(), "nothing more is coming");
        let titles: Vec<_> = answered
            .results
            .iter()
            .map(|hit| hit.title.as_str())
            .collect();
        assert_eq!(titles, ["From the archive"], "what it found shows");
        assert_eq!(
            search.search_page(&query, None, 10, 10).results.len(),
            1,
            "and stays found for the same search"
        );
    }

    #[test]
    fn a_hit_opens_the_app_at_its_anchor_and_scopes_narrow_the_search() {
        let mut app = TestApp::<MailApp>::new().unwrap();
        let (titles, mut scoped, declared, reminder) = app
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
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
            .with_widget_mut(|root: &mut ResourceRoot| {
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

    /// How many indexes of its entries `app`'s resource has started.
    fn index_builds<A: ResourceApp>(app: &mut TestApp<A>) -> usize {
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.search.borrow().builds)
            .unwrap()
    }

    #[test]
    fn the_action_directory_names_the_search_source_without_indexing_it() {
        let mut app = TestApp::<MailApp>::new().unwrap();
        let mut named = Vec::new();
        for edit in 0..4 {
            app.invoke(Mailbox::SET_SUBJECT.with(format!("Draft {edit}")))
                .unwrap();
            let (first, again) = app
                .harness()
                .with_widget_mut(|root: &mut ResourceRoot| {
                    (
                        RootSearch::search_provider_descriptors(root),
                        RootSearch::search_provider_descriptors(root),
                    )
                })
                .unwrap();
            assert_eq!(first, again, "an unchanged resource names the same build");
            named.push(first);
        }
        assert_eq!(
            index_builds(&mut app),
            0,
            "nobody searched: nothing indexed"
        );
        assert!(
            named.windows(2).all(|pair| pair[0] != pair[1]),
            "every revision names a new build, so the directory moves with it"
        );

        let (hits, searched, after) = app
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                let providers = RootSearch::search_providers(root);
                let hits = providers[0].search("invoice").len();
                (
                    hits,
                    ui_core::search::capability::SearchProviderDescriptor::of(
                        providers[0].as_ref(),
                    ),
                    RootSearch::search_provider_descriptors(root),
                )
            })
            .unwrap();
        assert_eq!(hits, 3);
        assert_eq!(index_builds(&mut app), 1, "a search builds the index once");
        assert_eq!(
            named.last(),
            Some(&vec![searched]),
            "the directory named the build the search answers from"
        );
        assert_eq!(named.last(), Some(&after), "and searching did not move it");
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                RootSearch::search_providers(root)[0].search("invoice");
            })
            .unwrap();
        assert_eq!(
            index_builds(&mut app),
            1,
            "an unchanged resource is not reindexed"
        );
    }

    /// Prose a book's entry carries, which sends its index off the UI thread.
    struct Prose(&'static str);

    impl tilo_resource::SearchText for Prose {
        fn segment_count(&self) -> usize {
            1
        }

        fn piece_count(&self, segment: usize) -> usize {
            usize::from(segment == 0)
        }

        fn piece(&self, segment: usize, piece: usize) -> Option<tilo_resource::SearchPiece<'_>> {
            (segment == 0 && piece == 0).then_some(tilo_resource::SearchPiece {
                text: self.0,
                space_before: false,
            })
        }
    }

    #[derive(Model, Default)]
    pub struct Shelf {
        pub title: String,
    }

    fn shelf_entries(shelf: &Shelf) -> Vec<SearchEntry> {
        vec![
            SearchEntry::new(shelf.title.clone(), "").with_body(tilo_resource::SearchBody::new(
                Arc::new(Prose("The lighthouse keeper counted the waves.")),
            )),
        ]
    }

    #[resource(kind = "test.shelf", label = "Shelf", search = shelf_entries)]
    impl Shelf {
        /// Retitle the book.
        #[action]
        pub fn retitle(&mut self, title: String) {
            self.title = title;
        }
    }

    struct ShelfApp;

    impl ResourceApp for ShelfApp {
        type Resource = Shelf;
        const ID: &'static str = "so.tilo.test-shelf";
        const TITLE: &'static str = "Shelf";

        fn view(_: &mut ViewCx<Shelf>) -> Element {
            Box::new(Column::new())
        }
    }

    #[test]
    fn edits_while_an_index_builds_cost_one_build_after_it() {
        let pumped = ui_core::background::Pumped::new();
        let search = |app: &mut TestApp<ShelfApp>| {
            let _background = pumped.background().enter();
            app.harness()
                .with_widget_mut(|root: &mut ResourceRoot| {
                    RootSearch::search_providers(root)[0]
                        .search("storm")
                        .into_iter()
                        .map(|hit| hit.title)
                        .collect::<Vec<_>>()
                })
                .unwrap()
        };
        let mut app = TestApp::<ShelfApp>::new().unwrap();
        app.invoke(Shelf::RETITLE.with("Storm Tales")).unwrap();
        assert!(
            search(&mut app).is_empty(),
            "a book is indexed off the UI thread"
        );
        assert_eq!(index_builds(&mut app), 1);
        for edit in 0..5 {
            app.invoke(Shelf::RETITLE.with(format!("Storm Tales {edit}")))
                .unwrap();
            assert!(
                search(&mut app).is_empty(),
                "until it arrives, search answers from the index before it"
            );
        }
        assert_eq!(
            index_builds(&mut app),
            1,
            "edits while a build is in flight start no build beside it"
        );

        pumped.run_pending();
        assert!(search(&mut app).contains(&"Storm Tales".to_owned()));
        assert_eq!(
            index_builds(&mut app),
            2,
            "once it arrives, the newest state alone is built"
        );
        pumped.run_pending();
        assert!(search(&mut app).contains(&"Storm Tales 4".to_owned()));
        assert_eq!(index_builds(&mut app), 2);
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
    shown: ui_core::widget::EmptyState,
}

impl Unopened {
    /// The window of a record that does not read as the resource of
    /// `kind` and `label`, whose actions are `declarations`.
    pub(crate) fn new(
        title: &'static str,
        icon: IconDescriptor,
        (kind, label): (&'static str, &'static str),
        declarations: &[tilo_resource::Declaration],
        reason: String,
    ) -> Self {
        let mut resource = tilo_actions::Resource::new(kind, kind, label);
        for decl in declarations {
            resource = resource.with_action(tilo_resource::declaration_spec(decl));
        }
        withhold_document_actions(&mut resource, &reason);
        Self {
            title,
            icon,
            shown: ui_core::widget::EmptyState::error(
                ui_core::i18n::I18nManager::lookup_plain("framework-unreadable"),
                reason,
            ),
            resource,
        }
    }
}

impl Unopened {
    /// The window as a host mounts it, its widget table compiled once for
    /// every app.
    pub(crate) fn boxed(self) -> Box<dyn AppWidget> {
        Box::new(self)
    }
}

impl Decorator for Unopened {
    ui_core::provides!(dyn ui_core::resources::PublishesResources);

    type Inner = ui_core::widget::EmptyState;

    fn inner(&self) -> &ui_core::widget::EmptyState {
        &self.shown
    }

    fn inner_mut(&mut self) -> &mut ui_core::widget::EmptyState {
        &mut self.shown
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_document_reads_are_bound_to_their_own_resource() {
        let mut child = tilo_actions::Resource::new("book/page/body", "text", "Body");
        child.children.push(tilo_actions::Resource::new(
            "private-child",
            "account",
            "Secret account",
        ));
        offer_document_reads(&mut child, &|id| id == "book/page/body");
        assert!(
            child.children[0].actions.is_empty(),
            "eligibility is not inherited"
        );
        let read = &child.actions[0];
        assert!(read.contract.reads_document());
        let encoded = read.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(encoded.type_id, READ_TYPE);
        let payload: serde_json::Value = tilo_actions::decode_json(&encoded.payload).unwrap();
        assert_eq!(payload["item"], "book/page/body");
        assert!(
            read.build_json(
                serde_json::json!({"item": "another/book"})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
        let encoded = read
            .build_json(serde_json::json!({"version": "past"}).as_object().unwrap())
            .unwrap();
        let payload: serde_json::Value = tilo_actions::decode_json(&encoded.payload).unwrap();
        assert_eq!(payload["item"], "book/page/body");
        assert_eq!(payload["version"], "past");
    }

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
            "restore-reviewed",
            "review-restore",
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
            live: None,
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
            live: None,
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
