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
    /// [`Resource::local_operations`].
    pub(crate) local_operations: fn() -> Vec<tilo_actions::ActionSpec>,
    /// Whether a resource id names one of the resource's items.
    pub(crate) item_of: fn(&str) -> bool,
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
            local_operations: R::local_operations,
            item_of: |id| tilo_resource::items::item_of_resource::<R>(id).is_some(),
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

    /// The live state.
    fn state(&self) -> Rc<dyn Any>;

    /// Bring the live state up to `doc`.
    ///
    /// # Errors
    /// The document does not read as the resource.
    fn sync(&self, doc: &AutoCommit) -> Result<bool, ModelError>;

    /// Whether the live state is `doc`'s.
    fn follows(&self, doc: &AutoCommit) -> bool;

    /// Why the record does not read as the resource, while it does not.
    fn unreadable(&self) -> Option<String>;

    /// Present the stored state again to what previewed an edit.
    fn represent(&self);

    /// The resource as `doc` holds it.
    ///
    /// # Errors
    /// The document does not read as the resource.
    fn read(&self, doc: &AutoCommit) -> Result<Rc<dyn Any>, ModelError>;

    /// Run the views' updaters over what changed since `followed`, the live
    /// revision they last followed, and place the participants the change
    /// landed.
    fn follow_change(&self, doc: &AutoCommit, followed: &Cell<u64>);

    /// Drain the projections the live state at `revision` lists, and take
    /// up the views' jobs.
    fn drain(&self, revision: u64);

    /// Note that `participant` applied `action`, where the resource lands
    /// them.
    fn note_applied(
        &self,
        participant: &tilo_actions::ParticipantId,
        action: &tilo_actions::EncodedAction,
    );

    /// [`Resource::publication_cache_epoch`] of `state`.
    fn cache_epoch(&self, state: &dyn Any) -> Option<String>;

    /// [`tilo_resource::publish_head`] of `state` at `id`, labelled `label`.
    fn head(&self, id: &str, label: &str, state: &dyn Any) -> tilo_actions::Resource;

    /// [`Resource::children_parts`] of `state`.
    fn children_parts(&self, state: &dyn Any) -> ChildrenParts;

    /// The edits the views bound over `state`, which surfaces add theirs to
    /// ([`SurfacePublication::bound_declarations`](super::super::surface::SurfacePublication::bound_declarations)).
    fn declarations(&self, state: &dyn Any) -> Box<dyn Any>;

    /// The ids of `declarations` (from [`Self::declarations`]).
    fn declaration_ids(&self, declarations: &dyn Any) -> Vec<&'static str>;

    /// No edits yet, which surfaces add theirs to
    /// ([`SurfacePublication::row_declarations`](super::super::surface::SurfacePublication::row_declarations)).
    fn no_declarations(&self) -> Box<dyn Any>;

    /// `resource` with `declarations` (from [`Self::declarations`] or
    /// [`Self::no_declarations`]) bound.
    fn bind(
        &self,
        resource: tilo_actions::Resource,
        declarations: Box<dyn Any>,
    ) -> tilo_actions::Resource;

    /// [`Resource::read_content`].
    ///
    /// # Errors
    /// As [`Resource::read_content`].
    fn read_content(
        &self,
        state: &dyn Any,
        doc: &AutoCommit,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String>;

    /// [`tilo_resource::child_by_id`] of `state`.
    fn child(&self, state: &dyn Any, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource>;

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
    ///
    /// # Errors
    /// The effect's refusal.
    fn start_effect(
        &self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String>;

    /// Poll the effect job `invocation`.
    fn poll_effect(
        &self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
    ) -> Option<tilo_actions::InvocationUpdate>;

    /// Apply what finished effects settle.
    fn apply_settlements(&self, edits: &mut ui_core::widget::Edits<'_>);

    /// Start the document effects of an applied `operation`.
    ///
    /// # Errors
    /// The effect's refusal.
    fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
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
        tracing::error!(kind = R::KIND, "a root was handed another resource's state");
    }
    typed
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
        return tilo_actions::decode_payload::<tilo_resource::ActionGroup<R::Action>>(
            &encoded.type_id,
            &encoded.payload,
        )
        .map(|group| group.actions)
        .unwrap_or_default();
    }
    Vec::new()
}

impl<R: Resource> TypedRoot for Typed<R> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn revision(&self) -> u64 {
        self.live.revision()
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

    fn unreadable(&self) -> Option<String> {
        self.live.unreadable().map(|unreadable| unreadable.reason)
    }

    fn represent(&self) {
        self.live.represent();
    }

    fn read(&self, doc: &AutoCommit) -> Result<Rc<dyn Any>, ModelError> {
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
        participant: &tilo_actions::ParticipantId,
        action: &tilo_actions::EncodedAction,
    ) {
        for applied in own_actions::<R>(action) {
            self.live.note_applied(participant.clone(), applied);
        }
    }

    fn cache_epoch(&self, state: &dyn Any) -> Option<String> {
        R::publication_cache_epoch(state_of::<R>(state)?)
    }

    fn head(&self, id: &str, label: &str, state: &dyn Any) -> tilo_actions::Resource {
        match state_of::<R>(state) {
            Some(state) => tilo_resource::publish_head::<R>(id, label, state),
            None => tilo_actions::Resource::new(id, R::KIND, label),
        }
    }

    fn children_parts(&self, state: &dyn Any) -> ChildrenParts {
        match state_of::<R>(state) {
            Some(state) => R::children_parts(state),
            None => ChildrenParts {
                others: Vec::new(),
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
        mut resource: tilo_actions::Resource,
        declarations: Box<dyn Any>,
    ) -> tilo_actions::Resource {
        if let Ok(declarations) = declarations.downcast::<Vec<ActionDecl<R>>>() {
            for decl in *declarations {
                resource = tilo_resource::bind_declaration(resource, &decl);
            }
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
        R::read_content(state, doc, item)
    }

    fn child(&self, state: &dyn Any, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
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
            .and_then(|encoded| tilo_actions::decode_json::<R::Action>(&encoded.payload).ok());
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
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.effects.start(op)
    }

    fn poll_effect(
        &self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.effects.poll(invocation, cancel)
    }

    fn apply_settlements(&self, edits: &mut ui_core::widget::Edits<'_>) {
        self.effects.apply_settlements(edits);
    }

    fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.effects.start_document_effects(operation, applied)
    }
}
