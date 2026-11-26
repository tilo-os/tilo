//! The part of a root that depends on its resource type.
//!
//! [`ResourceRoot`](super::ResourceRoot) is one widget type for every app,
//! compiled once in the shared runtime. What it does with the resource's own
//! type — read its live state, publish its head and children, bind the
//! edits views declared, run its effects, answer `read` — goes through
//! [`TypedRoot`], whose one implementation, [`Typed`], is the only root code
//! compiled once per resource type in every app. Its methods are thin: each
//! calls the resource's own function and hands back what does not name the
//! type, the state as `&dyn Any`.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

use tilo_resource::items::{ChildrenParts, ItemAction, ItemChildVisitor, ItemCollection};
use tilo_resource::{ActionDecl, ModelError, Resource, ScopeDecl, SearchEntry};
use ui_core::automerge::AutoCommit;
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};

use super::super::dispatch::Dispatcher;
use super::super::effect::Effects;
use super::super::live::Live;
use super::super::reactive::{Change, Updaters};
use super::super::view::Bindings;

/// What a root knows of its resource type without being generic over it:
/// its constants, and the few functions over ids that only name its kind.
#[derive(Clone, Copy)]
pub(crate) struct ResourceType {
    /// The type itself, which a root's hosted surfaces name.
    pub(crate) id: std::any::TypeId,
    /// [`Resource::KIND`].
    pub(crate) kind: &'static str,
    /// [`Resource::LABEL`].
    pub(crate) label: &'static str,
    /// [`Resource::READABLE`].
    pub(crate) readable: bool,
    /// [`Resource::HAS_FACTS`].
    pub(crate) facts: bool,
    /// [`Resource::RESOURCE_TABLE`].
    pub(crate) resource_table: Option<&'static tilo_resource::table::ModelTable>,
    /// [`Resource::local_operations`].
    pub(crate) local_operations: fn() -> Vec<tilo_actions::ActionSpec>,
    /// Whether a resource id names one of the resource's items, or a part
    /// beneath one: what the record's own actions answer for.
    pub(crate) item_of: fn(&str) -> bool,
    /// Whether a resource id names an item itself, what the framework's
    /// item `open` shows: never a part beneath one nor a derived place.
    pub(crate) opens_as_item: fn(&str) -> bool,
    /// [`tilo_resource::items::for_each_item_child`].
    pub(crate) items: fn(&[ItemCollection], &[ItemAction], &mut ItemChildVisitor<'_>),
    /// [`tilo_resource::items::item_child_id`].
    pub(crate) item_id: fn(&ItemCollection, &tilo_resource::Id) -> String,
}

impl ResourceType {
    pub(crate) fn of<R: Resource>() -> Self {
        Self {
            id: std::any::TypeId::of::<R>(),
            kind: R::KIND,
            label: R::LABEL,
            readable: R::READABLE,
            facts: R::HAS_FACTS,
            resource_table: R::RESOURCE_TABLE,
            local_operations: R::local_operations,
            item_of: |id| tilo_resource::items::item_of_resource::<R>(id).is_some(),
            opens_as_item: tilo_resource::kind::opens_as_item::<R>,
            items: tilo_resource::items::for_each_item_child::<R>,
            item_id: tilo_resource::items::item_child_id::<R>,
        }
    }
}

/// What a root does with its resource type. See the [module docs](self).
///
/// A `state` is one the root got from [`TypedRoot::state`] or
/// [`TypedRoot::read`] of the same root, so it is always the type's.
pub(crate) trait TypedRoot {
    /// The [`Typed`] behind this, for a caller that knows the type.
    fn as_any(&self) -> &dyn Any;

    /// The live state's revision.
    fn revision(&self) -> u64;

    /// [`Live::loads`] of the live state.
    fn loads(&self) -> u64;

    /// The live state.
    fn state(&self) -> Rc<dyn Any>;

    /// Bring the live state up to `doc`.
    fn sync(&self, doc: &AutoCommit) -> Result<bool, ModelError>;

    /// Whether the live state is `doc`'s.
    fn follows(&self, doc: &AutoCommit) -> bool;

    /// Whether `doc` moved on from the live state's document.
    fn leads_to(&self, doc: &AutoCommit) -> bool;

    /// Why the record does not read as the resource, while it does not.
    fn unreadable(&self) -> Option<String>;

    /// Present the stored state again to what previewed an edit.
    fn represent(&self);

    /// The resource as `doc` holds it.
    fn read(&self, doc: &AutoCommit) -> Result<Rc<dyn Any>, ModelError>;

    /// Run the views' updaters over what changed since `followed`, the live
    /// revision they last followed, and place the presences the change
    /// landed.
    fn follow_change(&self, doc: &AutoCommit, followed: &Cell<u64>);

    /// Drain the projections the live state at `revision` lists, and take
    /// up the views' jobs.
    fn drain(&self, revision: u64);

    /// Note that `presence` applied `action`, where the resource lands
    /// them.
    fn note_applied(
        &self,
        presence: &tilo_actions::PresenceId,
        action: &tilo_actions::EncodedAction,
    );

    /// Each presence where its last action left it, shown under `app_id`'s
    /// keys for the resource.
    fn landed_presences(&self, app_id: &str) -> Vec<tilo_ui_base::widget::ViewPresence>;

    /// Forget where `presence` was: its view closed.
    fn forget(&self, presence: &tilo_actions::PresenceId);

    /// Place a capture's agent where `landing` says.
    fn stage_landing(&self, landing: crate::framework::Landing);

    /// [`Resource::publication_cache_epoch`] of `state`.
    fn cache_epoch(&self, state: &dyn Any) -> Option<String>;

    /// [`tilo_resource::publish_head`] of `state` at `id`, labelled `label`.
    fn head(&self, id: &str, label: &str, state: &dyn Any) -> tilo_actions::ResourceSpec;

    /// [`tilo_resource::head_inputs`] of `state`.
    fn head_inputs(&self, state: &dyn Any) -> Option<tilo_resource::HeadInputs>;

    /// [`tilo_resource::publish_head_of`] `inputs` at `id`, labelled `label`.
    fn head_of(
        &self,
        id: &str,
        label: &str,
        inputs: &tilo_resource::HeadInputs,
    ) -> tilo_actions::ResourceSpec;

    /// Whether this exact child belongs to the declared document projection.
    fn document_child(&self, state: &dyn Any, id: &str) -> bool;

    /// [`Resource::children_parts`] of `state`.
    fn children_parts(&self, state: &dyn Any) -> ChildrenParts;

    /// The edits the views bound over `state`.
    fn declarations(&self, state: &dyn Any) -> Box<dyn Any>;

    /// How many times the list of bound edits changed
    /// ([`Bindings::generation`](super::super::view::Bindings)): a reader that
    /// saw the same count reads the same views' edits, over the same state.
    fn bindings_generation(&self) -> u64;

    /// The ids of `declarations` (from [`Self::declarations`]).
    fn declaration_ids(&self, declarations: &dyn Any) -> Vec<&'static str>;

    /// No edits yet, which views add theirs to
    /// ([`SurfacePublication::row_declarations`](super::super::surface::SurfacePublication::row_declarations)).
    fn no_declarations(&self) -> Box<dyn Any>;

    /// `resource` with `declarations` (from [`Self::declarations`] or
    /// [`Self::no_declarations`]) bound.
    fn bind(
        &self,
        resource: tilo_actions::ResourceSpec,
        declarations: Box<dyn Any>,
    ) -> tilo_actions::ResourceSpec;

    /// [`tilo_resource::read::content`].
    ///
    /// # Errors
    /// As [`tilo_resource::read::content`].
    fn read_content(
        &self,
        state: &dyn Any,
        doc: &AutoCommit,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String>;

    /// [`Resource::facts`].
    ///
    /// # Errors
    /// As [`Resource::facts`].
    fn facts(&self, state: &dyn Any, doc: &AutoCommit) -> Result<serde_json::Value, String>;

    /// One version of the record as the type's own adapter answers it, or
    /// the exact bytes it holds
    /// ([`historical_content`](super::super::preview::historical_content)).
    ///
    /// # Errors
    /// The [`HistoricalGap`](ui_core::history_mode::HistoricalGap) to show:
    /// the version does not read as the type, or the type has nothing to
    /// show.
    fn historical_content(
        &self,
        frozen: &AutoCommit,
        item: Option<&str>,
    ) -> Result<serde_json::Value, ui_core::history_mode::HistoricalGap>;

    /// [`tilo_resource::child_by_id`] of `state`.
    fn child(
        &self,
        state: &dyn Any,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec>;

    /// [`Resource::answer`].
    fn answer(
        &self,
        doc: &AutoCommit,
        action: &str,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value>;

    /// [`Resource::search`] of `state`.
    fn search(&self, state: &dyn Any) -> Vec<SearchEntry>;

    /// [`Resource::search_scopes`] of `state`.
    fn search_scopes(&self, state: &dyn Any) -> Vec<ScopeDecl>;

    /// Whether `next` continues `previous` ([`Resource::coalesces`]), and
    /// whether a later action may continue `next`
    /// ([`Resource::continuable`]); `(false, false)` for another type's.
    fn text_policy(
        &self,
        previous: Option<&tilo_actions::EncodedAction>,
        next: &tilo_actions::EncodedAction,
    ) -> (bool, bool);

    /// The specs of the effects the views registered.
    fn effect_specs(&self) -> Vec<tilo_actions::ActionSpec>;

    /// The operation types of the effects the views registered.
    fn effect_operations(&self) -> Vec<&'static str>;

    /// Whether a view drains effects causally.
    fn has_causal_drains(&self) -> bool;

    /// Start the effect `op` invokes, when a view registered it.
    fn start_effect(
        &self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<ResourceOperationStart>, String>;

    fn poll_effect(
        &self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Option<tilo_actions::InvocationUpdate>;

    /// Finish the jobs the view's windows started.
    fn poll_window_jobs(&self, view: &mut dyn tilo_ui_base::widget::Widget);

    /// Apply what finished effects settle.
    fn apply_settlements(&self, edits: &mut tilo_ui_base::widget::Edits<'_>);

    /// Start the document effects of an applied `operation`.
    fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &tilo_ui_base::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String>;
}

/// The handles of a root that name its resource type.
pub(crate) struct Typed<R: Resource> {
    pub(crate) live: Live<R>,
    pub(super) updaters: Updaters<R>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(super) bindings: Bindings<R>,
    pub(crate) effects: Effects<R>,
}

/// `state` as `R`'s. A root only ever hands its own type's state back, so
/// another type's is a framework bug, answered as nothing.
fn state_of<R: Resource>(state: &dyn Any) -> Option<&R::State> {
    let typed = state.downcast_ref::<R::State>();
    if typed.is_none() {
        foreign_state(R::KIND);
    }
    typed
}

/// `resource` with each declaration `decl_at` gives, from the first until
/// it gives none, bound ([`tilo_resource::bind_declaration`]): compiled once
/// rather than in every app's root.
#[inline(never)]
fn bind_each<'a>(
    mut resource: tilo_actions::ResourceSpec,
    decl_at: &dyn Fn(usize) -> Option<&'a tilo_resource::Declaration>,
) -> tilo_actions::ResourceSpec {
    let mut at = 0usize;
    while let Some(decl) = decl_at(at) {
        resource = tilo_resource::bind_declaration(resource, decl);
        at = at.saturating_add(1);
    }
    resource
}

/// Log that a root of `kind` was handed another resource's state: one
/// event site in the runtime rather than one in every app for each root.
#[cold]
#[inline(never)]
fn foreign_state(kind: &'static str) {
    tracing::error!(kind, "a root was handed another resource's state");
}

/// The resource's own actions `encoded` applies: itself, or each member of
/// a group of them, last landing last.
/// What a window draws of `landings` on a resource of `kind`: every presence where it
/// was placed. Apart from [`TypedRoot::landed_presences`] because it names no resource
/// type, so it is compiled once in the runtime rather than in every app.
#[inline(never)]
fn presences_of(
    landings: Vec<crate::framework::Landing>,
    kind: &str,
    app_id: &str,
) -> Vec<tilo_ui_base::widget::ViewPresence> {
    landings
        .into_iter()
        .map(|landing| {
            let (key, payload) =
                crate::framework::ResourceSelection::from(landing.place).presence_of(kind, app_id);
            tilo_ui_base::widget::ViewPresence {
                presence: landing.presence,
                label: landing.name,
                key,
                payload,
            }
        })
        .collect()
}

fn own_actions<R: Resource>(encoded: &tilo_actions::EncodedAction) -> Vec<R::Action> {
    if encoded.type_id == R::ACTION_TYPE {
        return match tilo_actions::decode_json(&encoded.payload) {
            Ok(action) => vec![action],
            Err(error) => {
                unread(&error, false);
                Vec::new()
            }
        };
    }
    if encoded.type_id == tilo_resource::group::group_type::<R>() {
        return match tilo_actions::decode_payload::<tilo_resource::ActionGroup<R::Action>>(
            &encoded.type_id,
            &encoded.payload,
        ) {
            Ok(group) => group.actions,
            Err(error) => {
                unread(&error, true);
                Vec::new()
            }
        };
    }
    Vec::new()
}

/// Say that an action of a resource, or a `group` of them, did not read back.
/// Its permissions and its presentation both come from what it says, so an
/// action that does not read back is none of the resource's own. Not generic,
/// so it is compiled once rather than for every resource.
#[cold]
#[inline(never)]
fn unread(error: &dyn std::fmt::Display, group: bool) {
    if group {
        tracing::warn!(%error, "a group of this resource's actions did not read back; it is not among its own actions");
    } else {
        tracing::warn!(%error, "an action of this resource did not read back; it is not among its own actions");
    }
}

impl<R: Resource> TypedRoot for Typed<R> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn revision(&self) -> u64 {
        self.live.revision()
    }

    fn loads(&self) -> u64 {
        self.live.loads()
    }

    fn state(&self) -> Rc<dyn Any> {
        self.live.get()
    }

    fn sync(&self, doc: &AutoCommit) -> Result<bool, ModelError> {
        self.live.sync(doc)
    }

    fn follows(&self, doc: &AutoCommit) -> bool {
        self.live.follows(doc)
    }

    fn leads_to(&self, doc: &AutoCommit) -> bool {
        self.live.leads_to(doc)
    }

    fn unreadable(&self) -> Option<String> {
        self.live.unreadable().map(|unreadable| unreadable.reason)
    }

    fn represent(&self) {
        self.live.represent();
    }

    fn read(&self, doc: &AutoCommit) -> Result<Rc<dyn Any>, ModelError> {
        let _profile = tilo_resource::profiling::Phase::start("framework.read_whole");
        R::read(doc).map(|state| Rc::new(state) as Rc<dyn Any>)
    }

    fn follow_change(&self, doc: &AutoCommit, followed: &Cell<u64>) {
        // A surface may have synced the state itself after its own edit.
        let revision = self.live.revision();
        let change = self.live.take_change();
        if revision != followed.get() || change.is_some() {
            let _profile = tilo_resource::profiling::Phase::start("framework.updaters");
            followed.set(revision);
            let change = match &change {
                Some(delta) => Change::Delta { delta, doc },
                None => Change::Everything,
            };
            self.updaters.run(&self.live.get(), &change);
        }
        self.live.place_landings();
    }

    fn drain(&self, revision: u64) {
        self.live
            .with(|state| self.effects.drain_projections(state, revision));
        self.effects.poll_jobs();
    }

    fn note_applied(
        &self,
        presence: &tilo_actions::PresenceId,
        action: &tilo_actions::EncodedAction,
    ) {
        for applied in own_actions::<R>(action) {
            self.live.note_applied(presence.clone(), applied);
        }
    }

    fn landed_presences(&self, app_id: &str) -> Vec<tilo_ui_base::widget::ViewPresence> {
        presences_of(self.live.landings(), R::KIND, app_id)
    }

    fn forget(&self, presence: &tilo_actions::PresenceId) {
        self.live.forget(presence);
    }

    fn stage_landing(&self, landing: crate::framework::Landing) {
        self.live.place(landing);
    }

    fn cache_epoch(&self, state: &dyn Any) -> Option<String> {
        R::publication_cache_epoch(state_of::<R>(state)?)
    }

    fn head(&self, id: &str, label: &str, state: &dyn Any) -> tilo_actions::ResourceSpec {
        match state_of::<R>(state) {
            Some(state) => tilo_resource::publish_head::<R>(id, label, state),
            None => tilo_actions::ResourceSpec::new(
                id,
                R::KIND,
                tilo_resource::label::published(R::KIND, label, R::LABEL, R::CATALOGUE),
            ),
        }
    }

    fn head_inputs(&self, state: &dyn Any) -> Option<tilo_resource::HeadInputs> {
        state_of::<R>(state).map(tilo_resource::head_inputs::<R>)
    }

    fn head_of(
        &self,
        id: &str,
        label: &str,
        inputs: &tilo_resource::HeadInputs,
    ) -> tilo_actions::ResourceSpec {
        tilo_resource::publish_head_of::<R>(id, label, inputs)
    }

    fn document_child(&self, state: &dyn Any, id: &str) -> bool {
        state_of::<R>(state).is_some_and(|state| R::document_child(state, id))
    }

    fn children_parts(&self, state: &dyn Any) -> ChildrenParts {
        match state_of::<R>(state) {
            Some(state) => R::children_parts(state),
            None => ChildrenParts {
                others: Vec::new(),
                kept: Rc::default(),
                items: Vec::new(),
                item_actions: Vec::new(),
            },
        }
    }

    fn declarations(&self, state: &dyn Any) -> Box<dyn Any> {
        let declarations: Vec<ActionDecl<R>> = state_of::<R>(state)
            .map(|state| self.bindings.declarations(state))
            .unwrap_or_default();
        Box::new(declarations)
    }

    fn bindings_generation(&self) -> u64 {
        self.bindings.generation()
    }

    fn declaration_ids(&self, declarations: &dyn Any) -> Vec<&'static str> {
        declarations
            .downcast_ref::<Vec<ActionDecl<R>>>()
            .map(|declarations| declarations.iter().map(|decl| decl.id).collect())
            .unwrap_or_default()
    }

    fn no_declarations(&self) -> Box<dyn Any> {
        Box::new(Vec::<ActionDecl<R>>::new())
    }

    fn bind(
        &self,
        mut resource: tilo_actions::ResourceSpec,
        declarations: Box<dyn Any>,
    ) -> tilo_actions::ResourceSpec {
        if let Some(declarations) = declarations.downcast_ref::<Vec<ActionDecl<R>>>() {
            resource = bind_each(resource, &|at| declarations.get(at).map(|decl| &**decl));
        }
        resource
    }

    fn read_content(
        &self,
        state: &dyn Any,
        doc: &AutoCommit,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let state =
            state_of::<R>(state).ok_or_else(|| "the resource is not readable here".to_owned())?;
        tilo_resource::read::content::<R>(state, doc, item)
    }

    fn facts(&self, state: &dyn Any, doc: &AutoCommit) -> Result<serde_json::Value, String> {
        let state =
            state_of::<R>(state).ok_or_else(|| "the resource states no facts here".to_owned())?;
        R::facts(state, doc)
    }

    fn historical_content(
        &self,
        frozen: &AutoCommit,
        item: Option<&str>,
    ) -> Result<serde_json::Value, ui_core::history_mode::HistoricalGap> {
        super::super::preview::historical_content::<R>(frozen, item)
    }

    fn child(
        &self,
        state: &dyn Any,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        tilo_resource::child_by_id::<R>(state_of::<R>(state)?, doc, id)
    }

    fn answer(
        &self,
        doc: &AutoCommit,
        action: &str,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value> {
        R::answer(doc, action, changes)
    }

    fn search(&self, state: &dyn Any) -> Vec<SearchEntry> {
        state_of::<R>(state).map(R::search).unwrap_or_default()
    }

    fn search_scopes(&self, state: &dyn Any) -> Vec<ScopeDecl> {
        state_of::<R>(state)
            .map(R::search_scopes)
            .unwrap_or_default()
    }

    fn text_policy(
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
            .and_then(|encoded| {
                tilo_failure::ignored!(
                    "an action that does not read back does not coalesce with this one",
                    tilo_actions::decode_json::<R::Action>(&encoded.payload).ok()
                )
            });
        (
            previous
                .as_ref()
                .is_some_and(|previous| R::coalesces(previous, &next)),
            R::continuable(&next),
        )
    }

    fn effect_specs(&self) -> Vec<tilo_actions::ActionSpec> {
        self.effects.specs()
    }

    fn effect_operations(&self) -> Vec<&'static str> {
        self.effects.operations()
    }

    fn has_causal_drains(&self) -> bool {
        self.effects.has_causal_drains()
    }

    fn start_effect(
        &self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.effects.start(op, doc, view)
    }

    fn poll_effect(
        &self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
        view: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.effects.poll(invocation, cancel, view)
    }

    fn poll_window_jobs(&self, view: &mut dyn tilo_ui_base::widget::Widget) {
        self.effects.poll_window_jobs(view);
    }

    fn apply_settlements(&self, edits: &mut tilo_ui_base::widget::Edits<'_>) {
        self.effects.apply_settlements(edits);
    }

    fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &tilo_ui_base::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.effects.start_document_effects(operation, applied)
    }
}
