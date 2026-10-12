//! The part of a root that depends on its resource type.
//!
//! [`ResourceRoot`](super::ResourceRoot) is one widget type for every app,
//! compiled once in the shared runtime. What it does with the resource's own
//! type — read its live state, publish its head and items, bind the edits
//! views declared, answer `read` — goes through [`TypedRoot`], whose one
//! implementation, [`Typed`], is the only root code compiled once per
//! resource type in every app. Its methods are thin: each calls the
//! resource's own function and hands back what does not name the type, the
//! state as `&dyn Any`.

use std::any::Any;
use std::rc::Rc;

use tilo_resource::{ActionDecl, ModelError, Resource, ScopeDecl, SearchEntry};
use ui_core::automerge::AutoCommit;
use ui_core::published::SharedResource;

use super::super::dispatch::Dispatcher;
use super::super::effect::Effects;
use super::super::live::Live;
use super::super::view::{Bindings, Updaters};
use super::shared_tree::{ItemSource, SharedTree};

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

    /// Present the stored state again to what previewed an edit.
    fn represent(&self);

    /// The resource as `doc` holds it.
    ///
    /// # Errors
    /// The document does not read as the resource.
    fn read(&self, doc: &AutoCommit) -> Result<Rc<dyn Any>, ModelError>;

    /// Run the views' updaters over the live state.
    fn run_updaters(&self);

    /// Take up and finish the views' jobs.
    fn poll_jobs(&self);

    /// [`Resource::publication_cache_epoch`] of `state`.
    fn cache_epoch(&self, state: &dyn Any) -> Option<String>;

    /// [`tilo_resource::publish_head`] of `state` at `id`, labelled `label`.
    fn head(&self, id: &str, label: &str, state: &dyn Any) -> tilo_actions::Resource;

    /// The document-owned tree of `state` at `head` (its id and label),
    /// sharing items through `tree`; `keep` when it is the live state's.
    fn document_tree(
        &self,
        tree: &SharedTree,
        head: (&str, &str),
        state: &dyn Any,
        keep: bool,
    ) -> SharedResource;

    /// The edits the views bound over `state`, which surfaces add theirs to
    /// ([`SurfacePublication::bound_declarations`](super::super::surface::SurfacePublication::bound_declarations)).
    fn declarations(&self, state: &dyn Any) -> Box<dyn Any>;

    /// `resource` with `declarations` (from [`Self::declarations`]) bound.
    fn bind(
        &self,
        resource: tilo_actions::Resource,
        declarations: Box<dyn Any>,
    ) -> tilo_actions::Resource;

    /// No edits yet for each of `rows` rows, which surfaces add theirs to
    /// ([`SurfacePublication::row_declarations`](super::super::surface::SurfacePublication::row_declarations)).
    fn row_declarations(&self, rows: usize) -> Box<dyn Any>;

    /// Bind to each of `rows` its edits in `declarations` (from
    /// [`Self::row_declarations`]).
    fn bind_rows(&self, rows: &mut [tilo_actions::Resource], declarations: Box<dyn Any>);

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

    /// [`Resource::child`].
    fn child(&self, state: &dyn Any, id: &str) -> Option<tilo_actions::Resource>;

    /// [`Resource::answer`].
    fn answer(
        &self,
        doc: &AutoCommit,
        action: &str,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value>;

    /// [`Resource::search`].
    fn search(&self, state: &dyn Any) -> Vec<SearchEntry>;

    /// [`Resource::search_scopes`].
    fn search_scopes(&self, state: &dyn Any) -> Vec<ScopeDecl>;

    /// Whether `next` continues `previous` ([`Resource::coalesces`]), and
    /// whether a later action may continue `next`
    /// ([`Resource::continuable`]); `(false, false)` for another type's.
    fn text_policy(
        &self,
        previous: Option<&tilo_actions::EncodedAction>,
        next: &tilo_actions::EncodedAction,
    ) -> (bool, bool);

    /// Start the document effects of an applied `operation`.
    ///
    /// # Errors
    /// The effect's refusal.
    fn start_document_effects(
        &self,
        operation: &ui_core::host_operation::ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String>;
}

/// The handles of a root that name its resource type.
pub(crate) struct Typed<R: Resource> {
    pub(crate) live: Live<R>,
    pub(super) updaters: Updaters<R>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(super) bindings: Bindings<R>,
    pub(super) effects: Effects<R>,
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

    fn represent(&self) {
        self.live.represent();
    }

    fn read(&self, doc: &AutoCommit) -> Result<Rc<dyn Any>, ModelError> {
        R::read(doc).map(|state| Rc::new(state) as Rc<dyn Any>)
    }

    fn run_updaters(&self) {
        self.updaters.run(&self.live.get());
    }

    fn poll_jobs(&self) {
        self.effects.poll_jobs();
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

    fn document_tree(
        &self,
        tree: &SharedTree,
        (id, label): (&str, &str),
        state: &dyn Any,
        keep: bool,
    ) -> SharedResource {
        let Some(state) = state_of::<R>(state) else {
            return SharedResource::from_resource(tilo_actions::Resource::new(id, R::KIND, label));
        };
        let head = tilo_resource::publish_head::<R>(id, label, state);
        let items = ItemSource {
            kind: R::KIND,
            walk: tilo_resource::items::for_each_item_child::<R>,
        };
        tree.document_tree(head, R::children_parts(state), &items, keep)
    }

    fn declarations(&self, state: &dyn Any) -> Box<dyn Any> {
        let declarations: Vec<ActionDecl<R>> = state_of::<R>(state)
            .map(|state| self.bindings.declarations(state))
            .unwrap_or_default();
        Box::new(declarations)
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

    fn row_declarations(&self, rows: usize) -> Box<dyn Any> {
        let mut declarations: Vec<Vec<ActionDecl<R>>> = Vec::new();
        declarations.resize_with(rows, Vec::new);
        Box::new(declarations)
    }

    fn bind_rows(&self, rows: &mut [tilo_actions::Resource], declarations: Box<dyn Any>) {
        let Ok(declarations) = declarations.downcast::<Vec<Vec<ActionDecl<R>>>>() else {
            return;
        };
        for (row, offered) in rows.iter_mut().zip(*declarations) {
            for decl in offered {
                let bound = std::mem::replace(row, tilo_actions::Resource::new("", "", ""));
                *row = tilo_resource::bind_declaration(bound, &decl);
            }
        }
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

    fn child(&self, state: &dyn Any, id: &str) -> Option<tilo_actions::Resource> {
        R::child(state_of::<R>(state)?, id)
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
        let Ok(next) = serde_json::from_slice::<R::Action>(&next.payload) else {
            return (false, false);
        };
        let previous = previous
            .filter(|encoded| encoded.type_id == R::ACTION_TYPE)
            .and_then(|encoded| serde_json::from_slice::<R::Action>(&encoded.payload).ok());
        (
            previous
                .as_ref()
                .is_some_and(|previous| R::coalesces(previous, &next)),
            R::continuable(&next),
        )
    }

    fn start_document_effects(
        &self,
        operation: &ui_core::host_operation::ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        self.effects.start_document_effects(operation, applied)
    }
}
