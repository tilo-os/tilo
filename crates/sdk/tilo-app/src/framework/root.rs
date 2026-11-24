//! The root widget of a framework app: keeps the resource state current,
//! publishes the resource and its actions, forwards edits to the host, and
//! indexes the resource for search.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use tilo_actions::ResourceSpec;
use ui_core::widget::{AppModel, ModelPath};

use crate::runner::{with_native_presentations, with_view_operations};
use tilo_resource::{Resource, SearchEntry};
use ui_core::PresencePayload;
use ui_core::application_view::{
    ApplicationFocusRestore, ApplicationLocalView, ApplicationViewState,
};
use ui_core::automerge::AutoCommit;
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::permissions::{LivePermissions, PermissionScope};
use ui_core::presence::PresenceKey;
use ui_core::resource::ResourceSaveStatus;
use ui_core::search::SearchProvider;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::view::ThawError;
use ui_core::widget::capability::{capability, capability_mut};
use ui_core::widget::{AppKind, AppWidget, Element, IconDescriptor, Widget};
use ui_core::{LocalState, Theme};

use super::dispatch::DispatchCore;
use super::rows::{find, join, outside_document};
use super::scenario::Run;
use super::stage::Stages;
use super::surface::SurfacePublication;
use super::view::ViewCx;
use ui_core::widget::DrawChild;

mod bindings;
mod controls;
mod derived;
mod facts;
mod live_head;
mod open_link;
pub(crate) use open_link::OpenLink;
mod overlay;
mod past_page;
use overlay::OverlayCache;
mod read;
mod search;
mod shared_tree;
mod shown_item;
mod typed;
mod unopened;
pub(crate) use unopened::Unopened;
mod widget;
use live_head::LiveHead;
pub(crate) use read::READ_TYPE;
pub(crate) use read::answer as answer_read;
use read::{offer_document_reads, offer_item_reads, read};
pub(crate) use search::{
    BodyText, IndexedEntries, LiveSearch, ResourceSearch, field_weight, index_entry,
};
pub(crate) use shared_tree::ItemChildren;
use shared_tree::{
    DocumentChildren, NestedViews, PublishedChildren, ResourceChildren, RowChildren,
    SharedTreeCache,
};
use typed::TypedRoot;
pub(crate) use typed::{ResourceType, Typed};
pub(crate) use widget::{RootApp, RootModel, RootPublishes, RootSearch, RootWidget};

pub(crate) struct ResourceRoot {
    title: &'static str,
    icon: IconDescriptor,
    /// The name the app gives the state it holds: [`ResourceApp::label`](super::ResourceApp::label),
    /// published with every state the root publishes, so a library of one
    /// book is named "1 book" the moment the book is on its shelf.
    label_of: fn(&dyn std::any::Any) -> String,
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
    saved: LocalState<ResourceSaveStatus>,
    /// The window's save status over the view: the record's durability and
    /// the app's own saves, silent unless a save stalls or fails.
    save: super::saves::SaveIndicator,
    /// The reactions to the record, in the window's bar, where the record
    /// takes them.
    reactions: super::reactions::ReactionsIndicator,
    /// The member's permissions, re-checked every sync so a role change
    /// while the app runs takes effect at once. `None` for a read-only view
    /// of a past version.
    permissions: Option<LivePermissions>,
    /// The records other workspaces hold that the window last asked its host about, and what the
    /// records it publishes beside its own were then ([`super::settings::ViewStorage::resources_key`]).
    shown_asked: (Vec<(String, u64)>, Vec<ui_core::shown_actions::ShownAction>),
    write: Option<PermissionScope>,
    stages: Stages,
    /// The item a view operation showed and its host laid out, which the
    /// operation, started again, reads without showing it anew.
    shown_first: Option<String>,
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
    /// What keeps the bindings of the app's view alive once it stands in
    /// the place of the window's arrival ([`Self::settle_arrival`]).
    arrived: Option<Rc<()>>,
    /// View and effect operations of the app's presentation, attached to the
    /// published resource. Document specs are dropped.
    presentation: Vec<tilo_actions::ActionSpec>,
    /// Shows what a link names ([`ResourceApp::open_link`](super::ResourceApp::open_link)): the
    /// framework's `open-link` on the resource.
    open_link: Option<OpenLink>,
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
    resource_children: std::cell::RefCell<Option<ResourceChildren>>,
    /// The children with the surfaces' live rows merged in, as the shared
    /// publication last carried them.
    row_children: std::cell::RefCell<Option<RowChildren>>,
    /// The head bound controls were last refreshed from, kept between syncs
    /// for a resource whose publication may be reused
    /// ([`Resource::publication_cache_epoch`]).
    binding_head: std::cell::RefCell<Option<BindingHead>>,
    /// What bound controls share of the publication at one head: built
    /// once per head, never copied per refresh.
    binding_base: std::cell::RefCell<Option<Rc<BindingBase>>>,
    /// The head of the live state and what it was published from.
    live_head: std::cell::RefCell<Option<LiveHead>>,
    /// The document children of the live state as last published, kept
    /// while they are unchanged.
    document_children: std::cell::RefCell<Option<DocumentChildren>>,
    /// The view operation types of the nested resources at one live revision.
    nested_views: std::cell::RefCell<Option<NestedViews>>,
    /// Whether the resource keeps document state History shows.
    history: bool,
    /// Whether the window stands on the framework's ground: every window but
    /// one shown over other apps' content — a sheet over the desktop
    /// ([`WindowRole::Sheet`]), or the on-screen keyboard over the app it
    /// types into ([`WindowRole::InputSurface`]).
    ///
    /// [`WindowRole::Sheet`]: ui_core::application::WindowRole::Sheet
    /// [`WindowRole::InputSurface`]: ui_core::application::WindowRole::InputSurface
    ground: bool,
    /// Platform presentation actions; the browser implementation has no state.
    native_presentation: crate::runner::Presentation,
    /// The links `share-link` is minting.
    share: super::share_link::ShareLinks,
    /// The edits the views bound, as last built ([`OverlayCache`]).
    overlay: std::cell::RefCell<Option<OverlayCache>>,
}

impl ResourceRoot {
    fn sync_over(&mut self, state: &AutoCommit, path: &automerge::ObjId) {
        self.refresh_access();
        if self.dispatch.take_represent() {
            self.typed.represent();
        }
        let head = self.refresh(state);
        for (action, reason) in self.dispatch.opener.take_refusals() {
            self.dispatch.refuse(&action, reason);
        }
        self.follow_readability();
        self.storage.refresh();
        self.typed
            .poll_window_jobs(effect_view(&mut self.unreadable, &mut self.content));
        {
            let _profile = tilo_resource::profiling::Phase::start("framework.content_sync");
            self.content.sync(state, path);
        }
        self.reveal_launched();
        self.settle_arrival();
        self.follow_search();
        // What the view's sync changed (its bound edits, its availability)
        // reaches the bindings; the document-owned head is rebuilt only when
        // the sync moved the state.
        let head = self.refresh_bindings(Some(head));
        *self.binding_head.borrow_mut() = Some(head);
        // The save status says itself in the window's bar.
        self.save.sync(ui_core::time::now());
        self.sync_reactions();
    }
}

impl ui_core::component_lifecycle::WindowActivation for ResourceRoot {
    /// Asked through the root's own API, or by the app's view: a view whose app runs
    /// elsewhere (a tilolang host's instance) hears the request there.
    fn take_window_activation_intent(&self) -> bool {
        let own = self.native_presentation.take_window_activation_intent();
        let view = app_view_in(self.content.as_ref())
            .is_some_and(ui_core::component_lifecycle::take_window_activation_intent);
        own || view
    }
}

impl ui_core::component_lifecycle::HearsElsewhere for ResourceRoot {
    /// Heard by the app's view: a view whose app runs elsewhere (a tilolang host's instance)
    /// hears the people at it on that app's connection.
    fn take_heard(&mut self) -> Vec<ui_core::component_lifecycle::Heard> {
        with_app_view_mut(&mut self.content, |view| {
            ui_core::component_lifecycle::take_heard(view)
        })
    }
}

impl ui_core::component_lifecycle::WindowTitle for ResourceRoot {
    /// The app's own view names what its window shows: the page, the record, the place it is at.
    /// A view that names nothing lets the window go by its app's name.
    fn window_title(&self) -> Option<&str> {
        let view = app_view_in(self.content.as_ref())?;
        ui_core::component_lifecycle::window_title(view)
    }
}

impl ui_core::widget::WindowHistory for ResourceRoot {
    fn set_window_presents_history(&mut self, tab_presents: bool) {
        with_app_view_mut(&mut self.content, |view| {
            if let Some(history) = ui_core::widget::capability::capability_mut::<
                dyn ui_core::widget::WindowHistory,
            >(view)
            {
                history.set_window_presents_history(tab_presents);
            }
        });
    }

    /// The app's own view offers the window's History: a view whose app runs elsewhere (a
    /// tilolang host's instance) shows the system's history of what the window shows.
    fn window_history_control(&self) -> Option<ui_core::widget::HistoryControl> {
        let view = app_view_in(self.content.as_ref())?;
        ui_core::widget::capability::capability::<dyn ui_core::widget::WindowHistory>(view)?
            .window_history_control()
    }

    fn invoke_window_history_control(
        &mut self,
        control: ui_core::widget::HistoryControl,
    ) -> Result<(), String> {
        with_app_view_mut(&mut self.content, |view| {
            ui_core::widget::capability::capability_mut::<dyn ui_core::widget::WindowHistory>(view)
                .ok_or("History is unavailable")?
                .invoke_window_history_control(control)
        })
    }
}

/// The operation type of the framework's `open` view: show the item at an
/// anchor, as picking a search hit does.
pub(crate) const OPEN_TYPE: &str = tilo_actions::remote::APPLICATION_OPEN_TYPE;

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
pub(crate) fn item_open_spec(anchor: &str) -> tilo_actions::ActionSpec {
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

/// Offer `open` on the item resource `resource`, with the view and the own
/// `share-link` that go with it, unless it has an `open` already: what shows
/// the item where the app's window shows it and lets a link name it. An item
/// of the app's record and one kept in a part of a chain are offered alike,
/// so a link to either opens the item and not the record.
pub(crate) fn offer_item(resource: &mut ResourceSpec) {
    if resource
        .actions
        .iter()
        .any(|existing| existing.id == "open")
    {
        return;
    }
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
            tilo_actions::ResourceViewSpec::new("item", ITEM_VIEW_LABEL, "open").with_primary(true),
        );
        // A view is also what a link opens: with one, the item offers
        // `share-link` of its own, which names the item rather than the
        // record it is in.
        tilo_resource::share::offer(resource);
    }
}

/// Offer `open` on every item resource under `resource` (an item of the
/// collections of the resource `item_of` knows the items of), bound to that
/// item ([`offer_item`]).
fn offer_item_open(item_of: fn(&str) -> bool, resource: &mut ResourceSpec) {
    if item_of(resource.id.as_str()) {
        offer_item(resource);
    }
    for child in &mut resource.children {
        offer_item_open(item_of, child);
    }
}

/// The ids of the actions the framework publishes on every app's resource
/// beside the app's own: the generic `apply`, `group` and `field-edit`, the
/// views `open`, `read` and `facts`, history, and the presentation operations. An app
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
    ids.push(open_spec().id.clone());
    ids.push(open_link::spec().id.clone());
    ids.push(tilo_resource::read::spec(None).id.clone());
    ids.push(facts::FACTS.to_owned());
    ids.push(tilo_resource::share::SHARE_LINK.to_owned());
    ids.push(tilo_resource::share::SHARE_INVITE_LINK.to_owned());
    ids.extend(super::history::specs().iter().map(|spec| spec.id.clone()));
    ids.extend(
        with_native_presentations(with_view_operations(ResourceSpec::new("", "", "")))
            .actions
            .iter()
            .map(|spec| spec.id.clone()),
    );
    ids.sort();
    ids.dedup();
    ids
}

/// The window's arrival: the app's view, shown once the workspace's holdings
/// arrived ([`super::app`]'s `build_root`).
type WorkspaceArrival = ui_core::widget::ArrivingView;

/// The app's view in `content`, once it shows: `content` itself, or the
/// view in the window's arrival once that arrived. The arrival
/// answers for nothing of the view (a traversal meets the view as its
/// descendant, once), so what the root asks of the app's view it asks here.
fn app_view_in(content: &dyn Widget) -> Option<&dyn Widget> {
    let Some(arrival) = content
        .as_any()
        .and_then(|any| any.downcast_ref::<WorkspaceArrival>())
    else {
        return Some(content);
    };
    let switch = arrival
        .answer()?
        .as_any()?
        .downcast_ref::<super::switch::Switch>()?;
    Some(switch.part())
}

/// `visit` the app's view in `content` ([`app_view_in`]); `content` itself
/// while the view is on its way, where nothing of it answers.
fn with_app_view_mut<R>(content: &mut Element, visit: impl FnOnce(&mut dyn Widget) -> R) -> R {
    let arrived = content
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<WorkspaceArrival>())
        .and_then(WorkspaceArrival::answer_mut)
        .and_then(|answer| answer.as_any_mut()?.downcast_mut::<super::switch::Switch>());
    match arrived {
        Some(switch) => visit(switch.part_mut()),
        None => visit(content.as_mut()),
    }
}

/// The view effects answer on: the app's `content`, or what stands in for
/// it while the resource is `unreadable`.
fn effect_view<'a>(
    unreadable: &'a mut Option<(String, Element)>,
    content: &'a mut Element,
) -> &'a mut dyn Widget {
    match unreadable {
        Some((_, shown)) => shown.as_mut(),
        None => content.as_mut(),
    }
}

/// What one walk of the view learns about the rows it publishes.
struct RowBindings {
    /// The edits each row's surfaces offer, by row id.
    offered: std::collections::HashMap<String, Box<dyn std::any::Any>>,
    /// What the surfaces say of the effects on each row, nested ones
    /// included, by row id, where they differ from the declaration.
    statuses: std::collections::HashMap<String, Vec<super::effect::EffectStatus>>,
    /// How each top-level collection item stands now, by row id, where the
    /// view knows what its document does not hold.
    presentations: std::collections::HashMap<String, tilo_actions::ResourcePresentation>,
    /// The reads each top-level collection item answers, by row id: the
    /// app's registered readers of its committed state, a launch replaying
    /// what it carries.
    reads: std::collections::HashMap<String, Vec<tilo_resource::read::Declared>>,
}

impl ResourceRoot {
    /// Once the window's arrival settled (the app's view shows, its
    /// cross-fade over), the view stands in the arrival's place: the root
    /// is the app's view plus what the framework adds again, and nothing of
    /// the arrival stays between them.
    fn settle_arrival(&mut self) {
        let Some(arrival) = self
            .content
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<WorkspaceArrival>())
        else {
            return;
        };
        let Some(mut answer) = arrival.take_answer(ui_core::motion::frame_now()) else {
            return;
        };
        let empty = || -> Element { Box::new(ui_core::widget::Spacer::fixed(0.0)) };
        self.content = match answer
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<super::switch::Switch>())
        {
            Some(switch) => {
                let (view, alive) = switch.take_part(empty());
                self.arrived = Some(alive);
                view
            }
            None => answer,
        };
        self.swapped = true;
    }

    /// The app's view: what the root asks the view for ([`app_view_in`]).
    pub(crate) fn app_view(&self) -> &dyn Widget {
        app_view_in(self.content.as_ref()).unwrap_or_else(|| self.content.as_ref())
    }

    /// The window ground is the framework's: an app never fills it, and one
    /// that paints nothing still stands on it. A sheet over the desktop
    /// stands on nothing ([`Self::without_ground`]).
    fn paint_ground(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        env: &ui_core::DrawEnv<'_>,
    ) {
        self.native_presentation.track_theme(theme);
        if self.ground {
            scene.fill(
                ui_core::vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.content,
                None,
                &self.content.get_rect(),
            );
        }
        if let Some(presence) = env.presence {
            self.run.people.follow(presence);
        }
    }

    /// A root over `content`, the view built in `cx`, whose handles the root
    /// keeps; `named` is the window's title, icon and live label, `alive` the
    /// scope `cx`'s bindings are tied to and `share` the client carried while
    /// `cx` built the view. Only this constructor knows `R`.
    pub(crate) fn new<R: Resource>(
        named: (
            &'static str,
            IconDescriptor,
            fn(&dyn std::any::Any) -> String,
        ),
        cx: ViewCx<R>,
        alive: Rc<()>,
        content: Element,
        share: super::share_link::ShareLinks,
    ) -> Self {
        let native_presentation = crate::runner::Presentation::new(&cx);
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
            named,
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
                share,
            },
        )
    }

    /// [`Self::new`] past what names the resource type.
    fn assemble(
        (title, icon, label_of): (
            &'static str,
            IconDescriptor,
            fn(&dyn std::any::Any) -> String,
        ),
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
            share,
        } = parts;
        Self {
            title,
            icon,
            label_of,
            resource,
            typed,
            action_bindings,
            followed: std::cell::Cell::new(0),
            _alive: alive,
            dispatch,
            saved,
            save: super::saves::SaveIndicator::new(saves, title),
            reactions: super::reactions::ReactionsIndicator::default(),
            permissions: None,
            shown_asked: Default::default(),
            write: None,
            stages,
            shown_first: None,
            feeds,
            run,
            storage,
            content,
            unreadable: None,
            swapped: false,
            arrived: None,
            presentation: Vec::new(),
            open_link: None,
            search: Rc::default(),
            publication_tree: std::cell::RefCell::new(None),
            item_children: std::cell::RefCell::new(None),
            published_children: std::cell::RefCell::new(None),
            resource_children: std::cell::RefCell::new(None),
            row_children: std::cell::RefCell::new(None),
            binding_head: std::cell::RefCell::new(None),
            binding_base: std::cell::RefCell::new(None),
            live_head: std::cell::RefCell::new(None),
            document_children: std::cell::RefCell::new(None),
            nested_views: std::cell::RefCell::new(None),
            history: true,
            ground: true,
            native_presentation,
            share,
            overlay: std::cell::RefCell::new(None),
        }
    }

    /// A root whose resource keeps no document state: it publishes no
    /// History actions and its window offers no History.
    pub(crate) fn without_history(mut self) -> Self {
        self.history = false;
        self
    }

    /// Stand on nothing: what the view does not paint shows what is under
    /// the window, in the live window and in the picture a host draws from
    /// it alike.
    pub(crate) fn without_ground(mut self) -> Self {
        self.ground = false;
        self.native_presentation.without_ground();
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
    /// Stage the views' state for `scenario`, and answer the members
    /// the capture shows as the presence frames their windows would send.
    pub(crate) fn stage(
        &self,
        scenario: Option<&str>,
    ) -> Vec<(String, PresenceKey, PresencePayload)> {
        self.stages.run(scenario);
        self.run
            .members()
            .iter()
            .filter_map(|shown| match shown {
                super::scenario::Shown::Member(name, at) => {
                    let (key, payload) = at.presence_of(self.resource.kind, self.dispatch.app_id());
                    Some((name.clone(), key, payload))
                }
                // An agent stands in its own view of the window, not in a
                // presence frame its window would send.
                super::scenario::Shown::Agent(landing) => {
                    self.typed.stage_landing(landing.clone());
                    None
                }
            })
            .collect()
    }

    /// Settle the window's save status for a capture
    /// ([`SaveIndicator::settle_for_capture`](super::saves::SaveIndicator::settle_for_capture)).
    pub(crate) fn settle_saves_for_capture(&mut self) {
        self.save.settle_for_capture(ui_core::time::now());
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

    /// Show the record's reactions in the window's bar, and run those
    /// pressed there ([`super::reactions`]). Nothing while the record does
    /// not read as the resource: its last state is not what it holds.
    fn sync_reactions(&mut self) {
        let state = self.typed.state();
        let readable = self.unreadable.is_none();
        self.reactions.sync(&super::reactions::ReactionsInput {
            table: self.resource.resource_table.filter(|_| readable),
            state: state.as_ref(),
            dispatch: &self.dispatch,
            people: &self.run.people,
            private: self.run.private,
        });
    }

    /// Show the item the launch asked for once a view that opens items is
    /// up. An item the record does not hold yet (it is still arriving) is
    /// asked for again at the next sync.
    fn reveal_launched(&self) {
        let mut reveal = self.run.launched.reveal.borrow_mut();
        if let Some(anchor) = reveal.as_deref()
            && self.stages.opens()
            && self.stages.open(anchor).is_ok()
        {
            *reveal = None;
        }
    }

    /// Apply a permission change made while the app runs: dispatchers stop
    /// (or resume) submitting, bound state and surfaces see the new value.
    fn refresh_access(&self) {
        let _profile = tilo_resource::profiling::Phase::start("framework.access_refresh");
        if let (Some(permissions), Some(write)) = (&self.permissions, &self.write) {
            let read_only = !permissions.check(write) && !self.may_change_shown_item(permissions);
            self.dispatch.set_read_only(read_only);
            // Its person sees, in the window's bar, that they may only look.
            ui_core::window_toolbar::show_view_only(read_only);
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

    /// What the surfaces say of the rows `rows` name, gathered in one walk
    /// of the view: the edits each row's views offer
    /// ([`Publishes::row_edits`](super::Publishes::row_edits)), and, where
    /// `items_of` names the collection's kind, the effects on each row
    /// (`Publishes::row_status`), how each item of that collection stands
    /// (`Publishes::item_presentation`) and the reads it answers
    /// (`Publishes::item_reads`).
    ///
    /// One walk asks every surface about every row, so binding costs the view
    /// plus surfaces × rows, never the view once per row. Each row's edits,
    /// statuses, presentations and reads arrive in tree order of the surfaces
    /// offering them.
    #[inline(never)]
    fn row_bindings(&self, rows: &[ResourceSpec], items_of: Option<&str>) -> RowBindings {
        // Every row, nested ones too (a playlist's tracks), asked of the
        // surfaces in one walk.
        fn ids(rows: &[ResourceSpec], out: &mut Vec<String>) {
            for row in rows {
                out.push(row.id.as_str().to_owned());
                ids(&row.children, out);
            }
        }
        let mut bindings = RowBindings {
            offered: std::collections::HashMap::new(),
            statuses: std::collections::HashMap::new(),
            presentations: std::collections::HashMap::new(),
            reads: std::collections::HashMap::new(),
        };
        if rows.is_empty() {
            return bindings;
        }
        let mut all = Vec::new();
        ids(rows, &mut all);
        // The collection field and item id each row is, for the collections
        // whose items the views give presentations to.
        let items: Vec<Option<(String, String)>> = rows
            .iter()
            .map(|row| {
                items_of.and_then(|kind| {
                    let (field, id) =
                        tilo_resource::items::item_of_resource_in(kind, row.id.as_str())?;
                    Some((field.to_owned(), id.as_str().to_owned()))
                })
            })
            .collect();
        let mut spare: Option<Box<dyn std::any::Any>> = None;
        self.surfaces(&mut |surface| {
            for id in &all {
                if let Some(decls) = bindings.offered.get_mut(id.as_str()) {
                    surface.row_declarations(id, decls.as_mut());
                    continue;
                }
                let decls = spare.get_or_insert_with(|| self.typed.no_declarations());
                if surface.row_declarations(id, decls.as_mut())
                    && let Some(decls) = spare.take()
                {
                    bindings.offered.insert(id.clone(), decls);
                }
            }
            if items_of.is_none() {
                return;
            }
            // A row's status reaches it wherever it is published, nested ones
            // too; an item's presentation is about the items of the page.
            for id in &all {
                let statuses = surface.row_status(id);
                if !statuses.is_empty() {
                    bindings
                        .statuses
                        .entry(id.clone())
                        .or_default()
                        .extend(statuses);
                }
            }
            for (index, row) in rows.iter().enumerate() {
                let id = row.id.as_str();
                if let Some((field, item)) = items.get(index).and_then(Option::as_ref) {
                    if !bindings.presentations.contains_key(id)
                        && let Some(presentation) = surface.item_presentation(field, item)
                    {
                        bindings.presentations.insert(id.to_owned(), presentation);
                    }
                    if !bindings.reads.contains_key(id) {
                        let reads = surface.item_reads(field, item);
                        if !reads.is_empty() {
                            bindings.reads.insert(id.to_owned(), reads);
                        }
                    }
                }
            }
        });
        bindings
    }

    /// Bind the edits `bindings` offers on the rows `rows` name.
    fn bind_rows(&self, rows: &mut [ResourceSpec], bindings: &mut RowBindings) {
        fn bind(
            typed: &dyn TypedRoot,
            rows: &mut [ResourceSpec],
            offered: &mut std::collections::HashMap<String, Box<dyn std::any::Any>>,
        ) {
            for row in rows {
                if let Some(decls) = offered.remove(row.id.as_str()) {
                    let bound = std::mem::replace(row, ResourceSpec::new("", "", ""));
                    *row = typed.bind(bound, decls);
                }
                bind(typed, &mut row.children, offered);
            }
        }
        bind(self.typed.as_ref(), rows, &mut bindings.offered);
    }

    /// Bind the edits views offer on the rows `rows` name
    /// ([`Publishes::row_edits`](super::Publishes::row_edits)).
    fn bind_row_declarations(&self, rows: &mut [ResourceSpec]) {
        if rows.is_empty() {
            return;
        }
        let mut bindings = self.row_bindings(rows, None);
        self.bind_rows(rows, &mut bindings);
    }

    /// Lay what `bindings` gathered over the top-level `rows`: the effects
    /// each view says differ from their declaration, and the presentation it
    /// gives an item from state the document does not hold.
    fn apply_row_bindings(rows: &mut [ResourceSpec], bindings: &mut RowBindings) {
        for row in rows {
            Self::apply_row_binding(row, bindings);
        }
    }

    /// [`Self::apply_row_bindings`] over `rows` and every row nested beneath
    /// them: a row's status reaches it wherever it is published.
    fn apply_row_bindings_below(rows: &mut [ResourceSpec], bindings: &mut RowBindings) {
        for row in rows {
            Self::apply_row_binding(row, bindings);
            Self::apply_row_bindings_below(&mut row.children, bindings);
        }
    }

    /// Lay what `bindings` gathered over `row` alone.
    fn apply_row_binding(row: &mut ResourceSpec, bindings: &mut RowBindings) {
        if let Some(statuses) = bindings.statuses.remove(row.id.as_str()) {
            super::rows::apply_statuses(row, &statuses);
        }
        if let Some(presentation) = bindings.presentations.remove(row.id.as_str())
            && presentation.is_valid()
        {
            row.presentation = Some(presentation);
        }
        if let Some(reads) = bindings.reads.remove(row.id.as_str()) {
            for read in reads {
                let read = read.into_spec();
                if !row.actions.iter().any(|known| known.id == read.id) {
                    row.actions.push(read);
                }
            }
        }
    }

    /// The resource as published: its own actions and children, the edits
    /// views bound to where the user is, the live rows views and surfaces
    /// show, and the framework's operations every app answers.
    #[inline(never)]
    fn publication(&self) -> ResourceSpec {
        let _profile = tilo_resource::profiling::Phase::start("framework.publication");
        let state = self.typed.state();
        let mut tree = self.shared_document_tree(&*state).to_resource();
        if let Some(reason) = self.typed.unreadable() {
            // Read out of the last state that did read: the record publishes
            // why its document actions do not apply, never that state as the
            // one it holds.
            tilo_resource::withhold_document_actions(&mut tree, &reason);
        }
        self.decorate_publication(&*state, tree, Decoration::Tree)
    }

    /// What the live state holds: its revision and how much of the document
    /// it read ([`super::Live::loads`]).
    fn version(&self) -> (u64, u64) {
        (self.typed.revision(), self.typed.loads())
    }

    /// What the app calls the resource `state` is: the name it is published
    /// and named in history and the catalogue by, as of what it holds now.
    fn name_of(&self, state: &dyn std::any::Any) -> String {
        (self.label_of)(state)
    }
}

/// What [`ResourceRoot::new`] hands [`ResourceRoot::assemble`] of the view
/// it was built in.
struct Parts {
    alive: Rc<()>,
    dispatch: DispatchCore,
    saved: LocalState<ResourceSaveStatus>,
    saves: super::saves::Saves,
    stages: Stages,
    feeds: super::history::HistoryFeeds,
    run: Run,
    storage: Rc<super::settings::ViewStorage>,
    content: Element,
    native_presentation: crate::runner::Presentation,
    /// The window's client for the session's action API. It is made, and carried, before any of
    /// the app's code runs: a client the view makes while it is built (at once, in a window whose
    /// workspace is readable at launch) asks over the connection the root drains. One made before
    /// it would keep its requests in an outbox nothing sends.
    share: super::share_link::ShareLinks,
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
#[derive(Clone)]
struct BaseKey {
    head: Rc<ResourceSpec>,
    locale: u64,
    epoch: Option<String>,
    causal: bool,
    unreadable: Option<String>,
}

impl PartialEq for BaseKey {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.head, &other.head)
            && self.locale == other.locale
            && self.epoch == other.epoch
            && self.causal == other.causal
            && self.unreadable == other.unreadable
    }
}

/// The shared part of what bound controls look up, at one [`BaseKey`].
struct BindingBase {
    key: BaseKey,
    resource: Rc<ResourceSpec>,
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
    facts: tilo_actions::ActionSpec,
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
            native: with_native_presentations(with_view_operations(ResourceSpec::new("", "", "")))
                .actions,
            read: tilo_resource::read::spec(None),
            facts: facts::facts_spec(),
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

/// The document-owned head of the publication, at the live version and
/// locale it was taken at.
#[derive(Clone)]
struct BindingHead {
    version: ((u64, u64), u64),
    /// [`Resource::publication_cache_epoch`] at the time.
    epoch: Option<String>,
    resource: Rc<ResourceSpec>,
}

/// `resource` without its children, cloning nothing of them.
#[cfg(all(test, not(target_arch = "wasm32")))]
fn without_children(resource: &ResourceSpec) -> ResourceSpec {
    let ResourceSpec {
        id,
        kind,
        label,
        presentation,
        semantic_identity,
        app_id,
        children: _,
        actions,
        views,
        personal,
        workspace,
        resource_type,
    } = resource;
    ResourceSpec {
        id: id.clone(),
        kind: kind.clone(),
        label: label.clone(),
        presentation: presentation.clone(),
        semantic_identity: semantic_identity.clone(),
        app_id: app_id.clone(),
        children: Vec::new(),
        actions: actions.clone(),
        views: views.clone(),
        personal: *personal,
        workspace: workspace.clone(),
        resource_type: resource_type.clone(),
    }
}

/// Visit the surface of type `S` the root of a resource app of `R` shows: for
/// a test, or a host that mounted the app, reaching the surface between
/// frames (`App::with_component_root` lends the root of a mounted one).
/// `None` when `root` is not such a root or shows no `S`.
pub fn with_surface<R: Resource, S: ui_core::widget::Widget + 'static, T>(
    root: &mut dyn std::any::Any,
    visit: impl FnOnce(&mut S) -> T,
) -> Option<T> {
    let root = root.downcast_mut::<ResourceRoot>()?;
    ui_core::widget::find_mut::<S>(root.view_mut()).map(visit)
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

/// Whether `entries` are indexed away from the frame that asks for them.
///
/// Natively, a few short entries index in that frame: tantivy costs tens of
/// microseconds, and a build that landed at once saves the search a look
/// again. A page moves every build off the frame — its one thread runs the
/// toolkit's background queue between frames, so a build in the frame holds
/// the palette's first paint until it finishes. A set large enough to be
/// slow, or one carrying prose bodies, moves off the frame on either host.
fn indexes_off_frame(entries: &[SearchEntry]) -> bool {
    // Tests drive both rules on the host they run on.
    #[cfg(test)]
    if let Some(forced) = FORCE_OFF_FRAME.with(std::cell::Cell::get) {
        return forced;
    }
    crate::runner::IN_A_PAGE
        || entries.len() >= OFF_THREAD_ENTRIES
        || entries.iter().any(|entry| entry.body.is_some())
}

#[cfg(test)]
thread_local! {
    /// Tests set whether every build moves off the frame, as a page's does.
    pub(crate) static FORCE_OFF_FRAME: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// What the resource's search entries are of: a revision of the resource
/// and the entries its surfaces show.
#[derive(PartialEq)]
struct SearchState {
    /// The record's revision and loads, and the chains' revision.
    revision: (u64, u64, u64),
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
pub(crate) struct SearchCache {
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
    ///
    /// A page never indexes in the frame that asks for it, however few the
    /// entries. Its one thread runs the toolkit's
    /// [`background`](ui_core::background) queue between frames, so a build
    /// in the frame holds the palette's first paint until it finishes, and
    /// indexing a window's content into tantivy there is what the page
    /// shell measured as the 14–57 s between typing and the first row, per
    /// app, moving with whichever window was opened last. A background job
    /// runs after the frame returns, so the ready rows paint at once and the
    /// content's rows join them when the index is built
    /// ([`SearchCache::building`], which the search showing polls for).
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
        // The index before this one, which this one changes: only what came,
        // went or changed since it is indexed.
        let before = self
            .answering
            .as_ref()
            .map(|answering| Arc::clone(&answering.indexed));
        if indexes_off_frame(&entries) {
            // The index landing wakes the window that asked: it is an
            // answer its next sync takes up, never one a frame polls for.
            let (send, arriving) = ui_core::wake::waking_channel();
            let building = entries.clone();
            let spawned = ui_core::background::Background::current().spawn(
                "resource-search-index",
                move || {
                    let indexed = match before {
                        Some(before) => before.updated(building, generation),
                        None => IndexedEntries::built(building, generation),
                    };
                    let _ = send.send(indexed);
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
        let indexed = Arc::new(match &self.answering {
            Some(before) => before.indexed.updated(entries, generation),
            None => IndexedEntries::built(entries, generation),
        });
        self.answering = Some(AnsweringIndex {
            of: Some(state),
            indexed: Arc::clone(&indexed),
        });
        indexed
    }

    /// Whether an index is being built off the frame for a state this cache
    /// was asked for: the search showing looks again shortly, so the rows the
    /// build finds are drawn when it lands.
    pub(crate) fn building(&mut self) -> bool {
        self.receive();
        self.building.is_some()
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

// The root is the app's view plus what the framework adds: it keeps the
// state current, publishes the resource, forwards edits and indexes search.
// Everything else — layout, input, traversal, operation admission — is the
// view's own, forwarded by the decorator.
impl RootModel for ResourceRoot {
    ui_core::provides!(dyn ui_core::undo::CommittedActions, dyn ui_core::history_mode::capability::DocumentHistory, dyn ui_core::widget::document_effects::DocumentEffects, dyn ui_core::widget::Collaborative, dyn ui_core::history_mode::capability::PresentsHistoryEntries, #[cfg(not(target_family = "wasm"))] dyn ui_core::component_lifecycle::WindowActivation, #[cfg(not(target_family = "wasm"))] dyn ui_core::component_lifecycle::WindowTitle, dyn ui_core::component_lifecycle::HearsElsewhere, dyn ui_core::widget::WindowHistory, dyn ui_core::collaboration::presences::Presences; inner = content);

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

    fn paints_ground(&self) -> bool {
        self.ground
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

    fn resource_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        let mut owners = self.storage.person_owners();
        owners.extend(self.content.resource_owners());
        owners
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui_core::doc::document_ref(state, path);
        self.sync_over(state, path);
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId) {
        self.typed.apply_settlements(edits);
        self.content.settle(edits, path);
        self.storage.settle();
    }

    fn draw(&mut self, ctx: &mut ui_core::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.paint_ground(scene, transform, theme, &env);
        self.content.draw_child(scene, transform, text, theme, &env);
        // The window drew: what its bar says of the save is seen.
        self.save.seen();
    }

    fn needs_redraw(&self) -> bool {
        self.native_presentation.needs_redraw()
            || self.content.needs_redraw()
            || self.save.needs_redraw(ui_core::time::now())
    }

    fn gives_up_at(&self) -> Option<ui_core::time::Instant> {
        self.storage.gives_up_at()
    }

    fn save_indicator(&self) -> &super::saves::SaveIndicator {
        &self.save
    }

    #[cfg(test)]
    fn save_indicator_mut(&mut self) -> &mut super::saves::SaveIndicator {
        &mut self.save
    }

    // A screenshot harness reaches the root to stage a scenario's view state
    // (`framework::stage`).
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    // The resource is the same for every member (an agent task sees and
    // invokes the same actions); only the content's tree is per member.

    // The undo boundary of a surface's immediate actions this event; the
    // host records one step per event and author.

    // A surface's step stays open for the actions that continue it (the
    // letters of a word), which report no boundary of their own.

    /// Resource and app presentation APIs publish effect/view actions; their
    /// operation types must be in the retained handler table or publication
    /// withdraws the whole component surface.
    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = ui_core::host_operation::resource_operation_handlers(self.view());
        handlers.extend(
            crate::runner::operation_types()
                .iter()
                .map(|op| (*op).to_owned()),
        );
        handlers.insert(super::history::VIEW_TYPE.to_owned());
        handlers.insert(super::person::HISTORY_TYPE.to_owned());
        handlers.insert(super::resources::RESOURCE_ACTION_TYPE.to_owned());
        handlers.insert(super::chain::CHAIN_ACTION_TYPE.to_owned());
        handlers.insert(super::chain::CHAIN_READ_TYPE.to_owned());
        handlers.insert(tilo_resource::share::SHARE_TYPE.to_owned());
        handlers.insert(tilo_resource::share::SHARE_INVITE_TYPE.to_owned());
        if self.stages.opens() {
            handlers.insert(OPEN_TYPE.to_owned());
        }
        if self.open_link.is_some() {
            handlers.insert(tilo_actions::remote::APPLICATION_OPEN_LINK_TYPE.to_owned());
        }
        // Opening another app is routed by the host to the workspace
        // broker, whichever resource offers it.
        handlers.insert(tilo_actions::remote::ApplicationLaunch::TYPE.to_owned());
        // A resource reads itself when it says it does, and its items from
        // its record's table when it does not (`offer_item_reads`); a record
        // kept beside it that reads itself is read through it too.
        handlers.insert(READ_TYPE.to_owned());
        if self.resource.facts {
            handlers.insert(facts::FACTS_TYPE.to_owned());
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
            if let Some(type_id) = &spec.operation_type {
                handlers.insert(type_id.clone());
            }
        }
        handlers.extend(self.nested_views().iter().cloned());
        handlers.extend(self.storage.view_types_beside());
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
            .poll_effect(
                invocation,
                cancellation_requested,
                effect_view(&mut self.unreadable, &mut self.content),
            )
            .or_else(|| {
                with_app_view_mut(&mut self.content, |view| {
                    ui_core::host_operation::poll_resource_operation(
                        view,
                        invocation,
                        cancellation_requested,
                        doc,
                        path,
                    )
                })
            })
    }

    fn start_operation(
        &mut self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        self.start_operation_refused(op, doc, path)
            .map_err(tilo_resource::Refusal::into_reason)
    }

    /// [`Self::start_operation`], keeping the refusal an action gave: a
    /// chain's answer is the app's own refusal, and one a caller decides on
    /// keeps its code (a message the mailbox holds already).
    fn start_operation_refused(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, tilo_resource::Refusal> {
        if let Some(started) = self.start_shown(op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(output) = self.start_resource_operation(op, doc)? {
            return Ok(Some(ResourceOperationStart::Completed(output)));
        }
        if let Some(started) = self.start_read(op, doc)? {
            return Ok(Some(started));
        }
        let request = op.request();
        if request.type_id == OPEN_TYPE {
            let args: serde_json::Value = tilo_actions::decode_json(request.payload)
                .map_err(|error| tilo_resource::Refusal::new(error.to_string()))?;
            let anchor = args
                .get(OPEN_ANCHOR)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tilo_resource::Refusal::new("open needs the anchor of an item"))?;
            self.stages
                .open(anchor)
                .map_err(tilo_resource::Refusal::new)?;
            // Whichever window of the app shows the item, it comes forward with it.
            self.native_presentation.request_activation();
            return Ok(Some(ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        if let Some(started) = self
            .start_open_link(op)
            .map_err(tilo_resource::Refusal::new)?
        {
            return Ok(Some(started));
        }
        // An effect reads the resource as the document stands, though no
        // frame has been drawn since a member's change arrived.
        if let Err(error) = self.typed.sync(doc) {
            tracing::warn!(%error, kind = self.resource.kind, "resource unreadable");
        }
        if let Some(started) = self
            .typed
            .start_effect(
                op,
                doc.doc(),
                effect_view(&mut self.unreadable, &mut self.content),
            )
            .map_err(tilo_resource::Refusal::new)?
        {
            return Ok(Some(started));
        }
        if let Some(started) =
            ui_core::host_operation::start_operation(self.view_mut(), op, doc, path)
                .map_err(tilo_resource::Refusal::new)?
        {
            return Ok(Some(started));
        }
        Ok(self.start_catalogue_view(op))
    }
}

impl ResourceRoot {
    /// The operations that show something first: a view of a resource the
    /// system asked for, and the app's own presentation ones.
    ///
    /// A view of a resource the system asked for shows that item first, so
    /// the picture is of the item and not of whatever the window happened to
    /// show: `render` is the app's answer to "show me this resource", and
    /// `outline` the same request answered as lines.
    fn start_shown(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, tilo_resource::Refusal> {
        let request = op.request();
        if matches!(
            request.type_id,
            crate::runner::RENDER_TYPE | crate::runner::OUTLINE_TYPE
        ) && !request.payload.is_empty()
            && let Some(anchor) = crate::runner::rendered_anchor(request.payload)
            && self.shown_first.take().as_deref() != Some(anchor.as_str())
        {
            self.stages
                .open(&anchor)
                .map_err(tilo_resource::Refusal::new)?;
            // Showing the item changed the view's own state: the widgets that
            // show it are built at a sync and placed by a layout, and without
            // them the read below answers with the item the window showed
            // before, or with the item at no place at all. A host that lays
            // the view out first does so and starts the read again, which
            // then reads the item laid out; one that cannot gets the sync the
            // next frame would run.
            if ui_core::host_operation::host_lays_out_first() {
                self.shown_first = Some(anchor);
                return Err(ui_core::host_operation::lay_out_first());
            }
            self.content.sync(doc.doc(), path);
        }
        Ok(with_app_view_mut(&mut self.content, |view| {
            self.native_presentation.start_operation(op, doc, view)
        })
        .map_err(tilo_resource::Refusal::new)?
        .map(ResourceOperationStart::Completed))
    }

    /// What reads the data this window keeps, its chain of parts or what the
    /// app publishes beside it, answered where it is asked: what the app
    /// holds, what a chain's parts hold, the facts of what the window shows,
    /// and a share link's own answer.
    fn start_read(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &ui_core::widget::Edits<'_>,
    ) -> Result<Option<ResourceOperationStart>, tilo_resource::Refusal> {
        let completed = ResourceOperationStart::Completed;
        let request = op.request();
        if (self.resource.readable || self.resource.resource_table.is_some())
            && request.type_id == READ_TYPE
        {
            match read(
                self.typed.as_ref(),
                &self.resource,
                &*self.typed.state(),
                doc,
                request.payload,
            ) {
                Ok(output) => return Ok(Some(completed(output))),
                Err(why) => {
                    // An item the resource does not hold may be one what it
                    // keeps beside it derives over its parts, as a mail folder
                    // is the messages the chain beside the mailbox holds; a
                    // resource that derives nothing answers as it would have.
                    if let Some(answer) = derived::answer(&self.storage, request.payload) {
                        return Ok(Some(completed(
                            answer.map_err(tilo_resource::Refusal::new)?,
                        )));
                    }
                    return Err(tilo_resource::Refusal::new(why));
                }
            }
        }
        // A chain of parts kept beside the record reads from its parts: what
        // it holds is not in the record's document, and a caller reading the
        // chain by its address never meant the record.
        if request.type_id == super::chain::CHAIN_READ_TYPE {
            let output = self
                .storage
                .chain_read(request.payload)
                .map_err(tilo_resource::Refusal::new)?;
            return Ok(Some(completed(output)));
        }
        if self.resource.facts && request.type_id == facts::FACTS_TYPE {
            let output = facts::answer(
                self.typed.as_ref(),
                &*self.typed.state(),
                doc,
                request.payload,
            )
            .map_err(tilo_resource::Refusal::new)?;
            return Ok(Some(completed(output)));
        }
        if tilo_resource::share::handles(request.type_id) {
            return self
                .start_share(request.invocation, request.payload)
                .map_err(tilo_resource::Refusal::new);
        }
        Ok(None)
    }

    /// The record-level requests every framework app answers the same way:
    /// its history at a version, an operation on one of its records or its
    /// chain of parts, the person's own record history, and a read of a
    /// record kept beside its own.
    fn start_resource_operation(
        &self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_resource::Refusal> {
        let request = op.request();
        let output = if request.type_id == super::history::VIEW_TYPE {
            super::history::answer(doc, request.payload).map_err(tilo_resource::Refusal::new)?
        } else if request.type_id == super::resources::RESOURCE_ACTION_TYPE {
            self.storage
                .resource_action(self.dispatch.me(), request.payload)
                .map_err(tilo_resource::Refusal::new)?
        } else if request.type_id == super::chain::CHAIN_ACTION_TYPE {
            self.storage
                .chain_action(self.dispatch.me(), request.payload, doc)?
        } else if request.type_id == super::person::HISTORY_TYPE {
            self.storage
                .person_history(request.payload)
                .map_err(tilo_resource::Refusal::new)?
        } else if request.type_id == READ_TYPE
            && let Some(resource) = tilo_resource::read::resource_of(request.payload)
        {
            self.storage
                .resource_read(&resource, request.payload)
                .map_err(tilo_resource::Refusal::new)?
        } else {
            return Ok(None);
        };
        Ok(Some(output))
    }

    /// What the resource's search entries are of now, and the surfaces'
    /// query sinks. What the surfaces show (a page's text, live rows)
    /// changes on its own, beside the resource's revision.
    fn search_state(&self) -> (SearchState, Vec<super::publishing::SearchQueries>) {
        let mut shown = Vec::new();
        let mut queries = Vec::new();
        self.surfaces(&mut |surface| {
            shown.extend(surface.search_entries());
            queries.extend(surface.search_queries());
        });
        let (revision, loads) = self.version();
        let state = SearchState {
            revision: (revision, loads, self.storage.chains_revision()),
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

    /// Whether the surfaces' secrets are unlocked: locked when any surface
    /// says so, unlocked when one says so and none disagrees, else unsaid.
    fn secrets_unlocked(&self) -> Option<bool> {
        let mut said = None;
        self.surfaces(&mut |surface| match surface.secrets_unlocked() {
            Some(false) => said = Some(false),
            Some(true) if said.is_none() => said = Some(true),
            _ => {}
        });
        said
    }

    /// Everything search finds in `state`: the resource's entries and what
    /// its surfaces show.
    fn entries_of(&self, state: &SearchState) -> Vec<SearchEntry> {
        let mut entries = self.typed.search(&*self.typed.state());
        entries.extend(self.storage.chains_search());
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
        self.surfaces(&mut |surface| {
            answering |= surface
                .search_queries()
                .is_some_and(|queries| queries.answering());
        });
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
        let request = op.request();
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

// The root's own clients for the action API (minting links, opening
// references) share the app's collaboration connection with whatever the
// view keeps there.
impl ui_core::widget::Collaborative for ResourceRoot {
    fn take_collab_requests(&mut self) -> Vec<ui_core::collaboration::CollabRequest> {
        let mut requests =
            ui_core::widget::Collaborative::take_collab_requests(self.share.remote_mut());
        if let Some(actions) = self.shown_to_ask() {
            requests.push(ui_core::collaboration::CollabRequest::DecideShownActions { actions });
        }
        if let Some(mut api) = self.dispatch.opener.api() {
            requests.extend(ui_core::widget::Collaborative::take_collab_requests(
                &mut api,
            ));
        }
        with_app_view_mut(&mut self.content, |view| {
            if let Some(content) = capability_mut::<dyn ui_core::widget::Collaborative>(view) {
                requests.extend(content.take_collab_requests());
            }
        });
        requests
    }

    fn handle_collab_event(&mut self, event: &ui_core::collaboration::CollabEvent) {
        if let ui_core::collaboration::CollabEvent::ActionVerdicts { verdicts } = event
            && let Some(permissions) = &self.permissions
        {
            permissions.decide_shown(verdicts);
            self.dispatch.refresh_permission_states();
        }
        ui_core::widget::Collaborative::handle_collab_event(self.share.remote_mut(), event);
        if let Some(mut api) = self.dispatch.opener.api() {
            ui_core::widget::Collaborative::handle_collab_event(&mut api, event);
        }
        with_app_view_mut(&mut self.content, |view| {
            if let Some(content) = capability_mut::<dyn ui_core::widget::Collaborative>(view) {
                content.handle_collab_event(event);
            }
        });
    }
}

impl ResourceRoot {
    /// The actions of the records the window publishes beside its own that another workspace
    /// holds (a person's profile record, held in their own workspace), when they changed since the
    /// window last asked its host about them: what it asks now. They are refused here until the
    /// host's verdicts arrive.
    fn shown_to_ask(&mut self) -> Option<Vec<ui_core::shown_actions::ShownAction>> {
        let permissions = self.permissions.as_ref()?;
        let key = self.storage.resources_key();
        if key == self.shown_asked.0 {
            return None;
        }
        let mut actions = Vec::new();
        for resource in &self.storage.resource_resources() {
            shown_actions_of(self.dispatch.app_id(), resource, &mut actions);
        }
        self.shown_asked.0 = key;
        if actions == self.shown_asked.1 {
            return None;
        }
        permissions.ask_shown(&actions);
        self.shown_asked.1.clone_from(&actions);
        Some(actions)
    }
}

/// Add to `actions` every action of `resource`, and of its children, that a workspace other than
/// the window's holds.
fn shown_actions_of(
    app: &str,
    resource: &ResourceSpec,
    actions: &mut Vec<ui_core::shown_actions::ShownAction>,
) {
    if let Some(workspace) = &resource.workspace {
        actions.extend(resource.actions.iter().map(|action| {
            ui_core::shown_actions::ShownAction::new(
                Some(workspace),
                app,
                (&resource.kind, resource.id.as_str(), &action.id),
                &action.contract.need(),
            )
        }));
    }
    for child in &resource.children {
        shown_actions_of(app, child, actions);
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
        let mut source = ResourceSearch::new(self.resource.label, self.dispatch.app_id(), indexed);
        source.scopes = self.typed.search_scopes(&*self.typed.state());
        source.queries = queries;
        // A person who may not read the record is found nothing of it.
        source.readable = self.dispatch.may_read();
        source.live = Some(LiveSearch {
            cache: Rc::clone(&self.search),
            answered: std::cell::RefCell::new(None),
        });
        source.opens = self
            .stages
            .opens()
            .then(|| tilo_actions::ActionAddress::new(self.dispatch.resource_id(), "open"));
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

    /// The resource's entries and what its surfaces show, as of the same
    /// generation the action directory names, with whether the surfaces'
    /// secrets are unlocked.
    fn search_index_state(&self) -> Option<ui_core::search::index_publication::IndexState> {
        let (state, _) = self.search_state();
        let generation = self.search.borrow_mut().generation_for(state);
        Some(ui_core::search::index_publication::IndexState {
            generation,
            secrets_unlocked: self.secrets_unlocked(),
        })
    }

    /// Every entry a hit can open, as the session's persistent index keeps
    /// it: one per resource, found by what each entry about it says, each
    /// text cut short. A view that keeps its
    /// entries out of the index
    /// ([`Publishes::search_entries_while_running`](super::Publishes::search_entries_while_running))
    /// contributes none.
    fn search_index_entries(&self) -> Vec<ui_core::search::index_publication::IndexEntry> {
        let mut entries = self.typed.search(&*self.typed.state());
        entries.extend(self.storage.chains_search());
        self.surfaces(&mut |surface| {
            if surface.kept_in_search_index() {
                entries.extend(surface.search_entries());
            }
        });
        let opens = self.dispatch.resource_id();
        // An item of the app's own resource publishes an `open` of its own, so it keeps it rather
        // than addressing the resource's. One a part keeps beside it opens through the resource's,
        // told the item as its anchor.
        let is_item = self.resource.opens_as_item;
        ui_core::search::index_publication::merged_by_key(
            entries
                .iter()
                .filter_map(|entry| index_entry(entry, opens, &is_item)),
        )
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
            with_app_view_mut(&mut self.content, |view| {
                ui_core::search::capability::activate_search_result(view, action_id, doc, path);
            });
        }
    }
}

impl RootPublishes for ResourceRoot {
    fn resources(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        RootPublishes::resources_for(self, None, doc)
    }
    fn shared_resources_for(
        &self,
        member: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Option<Vec<ui_core::published::SharedResource>> {
        self.shared_resources(member, doc)
    }
    fn resources_for(
        &self,
        member: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Vec<ResourceSpec> {
        // What surfaces bind to where someone is, they bind to this
        // member's view.
        let _profile = tilo_resource::profiling::Phase::start("framework.resources_for");
        let _viewer = member
            .cloned()
            .map(ui_core::invocation_scope::ViewerScope::enter);
        let mut resource = self.current_publication(doc);
        // The content may add live external entities beside the resource,
        // never a document action: those are the resource's own. One naming
        // the resource joins it rather than publishing it twice.
        let mut extras = Vec::new();
        for extra in ui_core::resources::resources_for(self.app_view(), member, doc) {
            let extra = outside_document(extra);
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
            offer_item_open(self.resource.opens_as_item, &mut resource);
        }
        if let Some(table) = self.resource.resource_table {
            offer_item_reads(&mut resource, table, self.resource.kind);
        }
        // A chain holds the app's items, so its resource stands at the top
        // of the app's tree, beside the app's own resource rather than under
        // it: a path names the chain and its item, nothing in between.
        let mut chains = Vec::new();
        let mut children = Vec::with_capacity(resource.children.len());
        for child in std::mem::take(&mut resource.children) {
            if super::chain_of_resource_id(child.id.as_str()).is_some() {
                chains.push(child);
            } else {
                children.push(child);
            }
        }
        resource.children = children;
        let mut resources = vec![resource];
        resources.append(&mut chains);
        resources.append(&mut extras);
        resources
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<ResourceSpec> {
        RootPublishes::resource_for(self, None, doc, id)
    }

    /// A live row a surface is still reading.
    fn resources_past_page(&self, doc: &AutoCommit) -> Vec<ResourceSpec> {
        self.past_page_all(doc)
    }

    fn past_page_len(&self, doc: &AutoCommit) -> usize {
        self.past_page_count(doc)
    }

    fn past_page_labels(&self, doc: &AutoCommit) -> Vec<(String, String)> {
        self.past_page_names(doc)
    }

    fn past_page_range(&self, doc: &AutoCommit, offset: usize, take: usize) -> Vec<ResourceSpec> {
        self.past_page_window(doc, offset, take)
    }

    fn resource_pending(&self, id: &str) -> bool {
        let mut pending = false;
        self.surfaces(&mut |surface| pending = pending || surface.live_row_pending(id));
        pending
    }
    /// Resource `id` in the published tree, or one the tree leaves out: a
    /// child the resource resolves by id, or a live row a surface resolves.
    fn resource_for(
        &self,
        member: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<ResourceSpec> {
        let _viewer = member
            .cloned()
            .map(ui_core::invocation_scope::ViewerScope::enter);
        // An item a chain keeps is built alone by the part keeping it, as
        // the chain carries it, whether or not the chain's published page
        // does: one past that page is addressed as one on it is, and either
        // costs the item, never the whole publication.
        if let Some(item) = self.storage.chain_item(id) {
            return Some(item);
        }
        // The chain itself is its head part's resource and the chain's own
        // verbs, as the tree carries it: never the whole publication, which
        // holds every item the chain keeps.
        if let Some(mut head) = self.storage.chain_head(id) {
            if self.stages.opens() {
                offer_item_open(self.resource.opens_as_item, &mut head);
            }
            return Some(head);
        }
        // A change from another window or member may not have reached
        // this view yet: an item it removed is gone, one it made is there.
        // The chains above answer a resource at the cost it takes; only this
        // builds the publication, which is what `framework.resource_for`
        // measures.
        let _profile = tilo_resource::profiling::Phase::start("framework.resource_for");
        if let Some(mut published) = find(&[self.current_publication(doc)], id) {
            // The publication's items are listed with the read each answers;
            // this one is built alone, so it carries the same
            // ([`Self::resources_for`]).
            if let Some(table) = self.resource.resource_table {
                offer_item_reads(&mut published, table, self.resource.kind);
            }
            return Some(published);
        }
        let current = if self.follow_document(doc) {
            None
        } else {
            self.typed.read(doc).ok()
        };
        let state = current.unwrap_or_else(|| self.typed.state());
        let mut child = self.typed.child(&*state, doc, id);
        if let Some(child) = &mut child {
            if self.resource.readable {
                offer_document_reads(child, &|id| self.typed.document_child(&*state, id));
            }
            // The published tree offers each item of the app's collections a
            // read of its own ([`Self::resources_for`]); one built alone carries
            // the same, so an item's address answers the read it lists.
            if let Some(table) = self.resource.resource_table {
                offer_item_reads(child, table, self.resource.kind);
            }
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
            let mut bindings =
                self.row_bindings(std::slice::from_ref(&resolved), Some(self.resource.kind));
            self.bind_rows(std::slice::from_mut(&mut resolved), &mut bindings);
            Self::apply_row_bindings(std::slice::from_mut(&mut resolved), &mut bindings);
            if self.stages.opens() {
                offer_item_open(self.resource.opens_as_item, &mut resolved);
            }
            return Some(resolved);
        }
        // Last, what the content resolves by id beyond what it publishes (a
        // place, a record past the published page), without a document
        // action: those are the resource's own.
        ui_core::resources::resource_for(self.app_view(), None, doc, id)
            .map(super::rows::resolved_outside_document)
    }
    fn published_action(
        &self,
        id: &str,
        mut spec: tilo_actions::ActionSpec,
    ) -> tilo_actions::ActionSpec {
        // The resource's own actions take the effects whose status is about
        // the resource; a row takes what the surfaces say of that row.
        let own = id == self.dispatch.resource_id();
        self.surfaces(&mut |surface| {
            let statuses = if own {
                surface.availability()
            } else {
                surface.row_status(id)
            };
            for status in &statuses {
                if status.id() == spec.id && (!own || status.item().is_none()) {
                    status.overlay(&mut spec);
                }
            }
        });
        spec
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
                        self.app_view(),
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
        ui_core::resources::document_action_result(self.app_view(), doc, address, changes)
    }
}

// The root owns the folder whose history it shows beside the resource's (the
// runner checks the source it registered is this root's); everything else
// about History is its content's, as before.
// How History shows this app's changes is the view's to say; what a change acted on opens as
// any item of the app opens, so a row's link to the thing it changed works in every app whose
// views open items, not only in an app that presents its changes itself.
impl ui_core::history_mode::capability::PresentsHistoryEntries for ResourceRoot {
    fn history_entry_presenter(
        &self,
    ) -> Option<std::rc::Rc<dyn ui_core::widget::HistoryEntryPresenter>> {
        ui_core::history_mode::capability::history_entry_presenter(self.content.as_ref())
    }

    fn open_history_entry(&mut self, entry: &ui_core::widget::HistoryEntry) -> bool {
        if ui_core::history_mode::capability::open_history_entry(self.content.as_mut(), entry) {
            return true;
        }
        entry
            .target
            .as_ref()
            .is_some_and(|target| self.stages.opens() && self.stages.open(&target.id).is_ok())
    }
}

impl ui_core::history_mode::capability::DocumentHistory for ResourceRoot {
    fn history_registration(
        &self,
    ) -> Result<
        Option<ui_core::history_mode::HistoryRegistration>,
        ui_core::history_mode::HistoryRegistrationError,
    > {
        match &self.run.folder_history {
            // The folder History lists is the window's own: the root observes the source its
            // runner registered.
            Some((source, definition)) => {
                let scopes = definition.scopes.iter().map(|scope| {
                    ui_core::history_mode::HistorySourceRegistration::FolderFiles {
                        scope: scope.key,
                        source: source.clone(),
                        window: None,
                    }
                });
                ui_core::history_mode::HistoryRegistration::new(definition, scopes.collect())
                    .map(Some)
            }
            None => ui_core::history_mode::capability::history_registration(self.app_view()),
        }
    }

    fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
        ui_core::widget::capability::capability::<
            dyn ui_core::history_mode::capability::DocumentHistory,
        >(self.app_view())
        .map_or(Ok(()), |content| content.history_restore_readiness(scope))
    }

    fn document_history_definition(
        &self,
    ) -> Option<&'static ui_core::history_mode::HistoryDefinition> {
        ui_core::history_mode::capability::document_history_definition(self.app_view())
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
        // An action a member took (an agent's verb) leaves them where
        // the resource says it lands, once the state follows it.
        if let Some(member) = ui_core::invocation_scope::current_presence() {
            self.typed.note_applied(&member, action);
        }
        with_app_view_mut(&mut self.content, |view| {
            if let Some(content) = ui_core::widget::capability::capability_mut::<
                dyn ui_core::history_mode::capability::DocumentHistory,
            >(view)
            {
                content.document_action_applied(action, before, doc, path);
            }
        });
    }
}

/// The view's own presences, and every other presence where its last action
/// left it: a presence answered in its own view (an agent shown a row) is seen
/// there by the person at this window.
impl ui_core::collaboration::presences::Presences for ResourceRoot {
    fn join_presence(&mut self, presence: &tilo_actions::PresenceId) -> bool {
        ui_core::collaboration::presences::join_presence(self.content.as_mut(), presence)
    }

    fn leave_presence(&mut self, presence: &tilo_actions::PresenceId) {
        self.typed.forget(presence);
        ui_core::collaboration::presences::leave_presence(self.content.as_mut(), presence);
    }

    fn set_presence_label(&mut self, presence: &tilo_actions::PresenceId, label: &str) {
        ui_core::collaboration::presences::set_presence_label(
            self.content.as_mut(),
            presence,
            label,
        );
    }

    fn presences(
        &self,
        doc: &ui_core::automerge::AutoCommit,
    ) -> Vec<ui_core::widget::ViewPresence> {
        let mut shown = ui_core::collaboration::presences::presences(self.content.as_ref(), doc);
        for landed in self.typed.landed_presences(self.dispatch.app_id()) {
            if !shown.iter().any(|view| view.presence == landed.presence) {
                shown.push(landed);
            }
        }
        shown
    }

    fn set_remote_only(&mut self, remote_only: bool) {
        ui_core::collaboration::presences::set_remote_only(self.content.as_mut(), remote_only);
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

    fn take_process_intent(&mut self) -> Option<ui_core::widget::AppProcessIntent> {
        fn take(
            widget: &mut dyn Widget,
            resource: std::any::TypeId,
        ) -> Option<ui_core::widget::AppProcessIntent> {
            if let Some(surface) = surface_mut(widget, resource)
                && let Some(intent) = surface.take_process_intent()
            {
                return Some(intent);
            }
            // A view whose app runs elsewhere asks through its own API.
            if let Some(intent) = ui_core::component_lifecycle::take_process_intent(widget) {
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
        self.save.take_resource_retry()
    }

    fn resource_save_status(&mut self, status: ResourceSaveStatus) -> bool {
        // Sampled on every sync: only a new status is a change. A save that
        // lands within the surface's stall window shows nothing, and asks no
        // frame of its own.
        let shown = self.save.record(&status, ui_core::time::now());
        self.saved.set_if_changed(status);
        shown
    }

    // The workspace refused what this record sent, and the record withdrew it:
    // the view shows the stored state again, and the action reads as
    // refused, as one this device refused does.
    fn record_refused(&mut self, refusal: &ui_core::resource::ResourceRefusal) {
        self.dispatch.refuse_by_workspace(refusal);
        self.typed.represent();
    }

    fn take_notices(&mut self) -> Vec<String> {
        self.dispatch.take_notices()
    }

    fn record_said(&mut self, text: &str) {
        self.dispatch.kept(text);
    }

    // A surface that keeps presentation of its own across restarts (the
    // open item, the caret) saves it as the window's.
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        capability::<dyn ApplicationViewState>(self.app_view())
            .map_or(Ok(None), ApplicationViewState::capture_application_view)
    }

    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        doc: &AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        with_app_view_mut(&mut self.content, |view| {
            match capability_mut::<dyn ApplicationViewState>(view) {
                Some(view) => view.restore_application_view(local, doc),
                None => Err(ThawError::State(
                    "application-local restoration unsupported".into(),
                )),
            }
        })
    }

    fn finish_application_view_restore(&mut self) -> Result<(), ThawError> {
        with_app_view_mut(&mut self.content, |view| {
            capability_mut::<dyn ApplicationViewState>(view).map_or(
                Ok(()),
                ApplicationViewState::finish_application_view_restore,
            )
        })
    }
}

#[cfg(test)]
mod index_entry_tests;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod search_tests;

/// What a window shows instead of the app's view while its record does not
/// read as the resource: what happened and what to do, in the person's
/// words, with the reason the record was refused (a path and a missing
/// value) behind the state's "Details".
fn unreadable_state(reason: &str) -> ui_core::widget::EmptyState {
    ui_core::widget::EmptyState::new(ui_core::i18n::widget_text("framework-unreadable"))
        .with_icon(ui_core::icons::Icon::CircleAlert)
        .with_message(ui_core::i18n::widget_text("framework-unreadable-hint"))
        .with_details(reason)
}

#[cfg(test)]
mod tests;

// Compiled only into native tests: the file's own `cfg` says so.
mod publication_tests;
