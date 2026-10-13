//! Live external rows: what a surface shows that lives outside the
//! resource's document — processes, devices, containers, journal entries,
//! fetched catalogue rows, running jobs — published beneath the resource for
//! the palette, agents and scripts.

use tilo_actions::{
    ActionSpec, OperationKind, Resource, ResourcePresentation, ResourceSemanticIdentity,
};
use tilo_resource::Effect;

/// One live external entity a surface publishes beneath its resource, with
/// the view and effect operations it offers (inspect a process, stop a
/// container).
///
/// A row is not document state: it has no history, no undo and nothing to
/// sync, so it can never carry a document mutation. A change to the
/// document is the resource's `Action` through the
/// [`Dispatcher`](super::Dispatcher), bound to a row's item through
/// [`ResourceSurface::bound_declarations`](super::ResourceSurface::bound_declarations)
/// or the resource's `children`.
///
/// Rows are merged by id into the resource's published children: a row with
/// the id of a document child adds its operations to that child instead of
/// publishing a second resource under the same id.
#[derive(Clone)]
pub struct LiveRow {
    resource: Resource,
}

impl LiveRow {
    /// A row with a stable `id` (unique within the resource, and naming the
    /// same entity for as long as it lives: a process id with its start
    /// time, a container id), a `kind` tag and a human `label`.
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            resource: Resource::new(id, kind, label),
        }
    }

    /// Attach the stable source-domain identity used to find this row across
    /// live dispatch aliases. It is descriptive metadata only and grants no
    /// permission or authority. Invalid identities are omitted at this
    /// publication boundary.
    #[must_use]
    pub fn semantic_identity(mut self, identity: ResourceSemanticIdentity) -> Self {
        self.resource.semantic_identity = identity.is_valid().then_some(identity);
        self
    }

    /// `resource`, an entity the surface already describes as a resource
    /// (a search hit, a catalogue entry), as a row: its presentation,
    /// views, operations and children. Its document actions are dropped at
    /// any depth: the resource's own publication carries them.
    pub fn from_resource(resource: Resource) -> Self {
        Self {
            resource: resolved_outside_document(resource),
        }
    }

    /// Offer a view or effect operation on the row.
    ///
    /// # Panics
    /// In a debug build, when `operation` is a document mutation; a release
    /// build drops it.
    #[must_use]
    pub fn operation(mut self, operation: ActionSpec) -> Self {
        if admits(&operation, self.id()) {
            self.resource.actions.push(operation);
        }
        self
    }

    /// How the entity stands right now (a process's CPU use, what a timer
    /// has left), shown beside its label. A row merged into a document child
    /// that has no presentation of its own lends it this one.
    #[must_use]
    pub fn presentation(mut self, presentation: ResourcePresentation) -> Self {
        self.resource.presentation = Some(presentation);
        self
    }

    /// Offer `effect`, bound to this row's entity (stop *this* process):
    /// invoked here, it reaches the surface's or view's handler of `E` as
    /// that value, under the effect's declared contract.
    #[must_use]
    pub fn effect<E: Effect>(self, effect: &E) -> Self {
        self.operation(effect.bound())
    }

    /// Offer `view` of the row's entity: a way to show it (a message in
    /// its conversation, a range of its text) that a link or an agent
    /// navigates to, opened by one of the row's own operations.
    #[must_use]
    pub fn view(mut self, view: tilo_actions::ResourceViewSpec) -> Self {
        self.resource = self.resource.with_view(view);
        self
    }

    /// Nest `row` beneath this one (a container's logs).
    #[must_use]
    pub fn row(mut self, row: LiveRow) -> Self {
        self.resource.children.push(row.resource);
        self
    }

    /// The row's id.
    pub fn id(&self) -> &str {
        self.resource.id.as_str()
    }

    /// The row as the published tree carries it.
    pub fn into_resource(mut self) -> Resource {
        tilo_resource::share::offer_tree(&mut self.resource);
        self.resource
    }
}

/// Whether `operation` may ride outside the document: views and effects do,
/// a document mutation never does.
fn admits(operation: &ActionSpec, on: &str) -> bool {
    let admitted = operation.kind != OperationKind::Document;
    debug_assert!(
        admitted,
        "`{on}` offers document action `{}` outside the resource's actions; \
         a document change is the resource's Action through its Dispatcher",
        operation.id
    );
    admitted
}

/// `resource` without document actions, at any depth: what a content widget
/// may add to the published tree beside the resource's own publication.
pub(crate) fn outside_document(mut resource: Resource) -> Resource {
    let on = resource.id.as_str().to_owned();
    resource.actions.retain(|action| admits(action, &on));
    resource.children = resource
        .children
        .into_iter()
        .map(outside_document)
        .collect();
    resource
}

/// `resource`, resolved by id, without document actions. A lookup is asked
/// for ids the resource itself publishes too (an item of its own), so this
/// drops them without complaint: the resource's publication carries them.
pub(crate) fn resolved_outside_document(mut resource: Resource) -> Resource {
    resource
        .actions
        .retain(|action| action.kind != OperationKind::Document);
    resource.children = resource
        .children
        .into_iter()
        .map(resolved_outside_document)
        .collect();
    resource
}

/// Merge `rows` by id into `children`: a row naming an existing child adds
/// the operations and rows that child lacks, and its presentation when the
/// child has none; any other row is appended. The first publisher of an id
/// keeps its label and kind.
pub(crate) fn merge(children: &mut Vec<Resource>, rows: impl IntoIterator<Item = Resource>) {
    for row in rows {
        match children.iter_mut().find(|child| child.id == row.id) {
            Some(existing) => join(existing, row),
            None => children.push(row),
        }
    }
}

/// Add to `existing` the presentation, semantic identity, operations, views and rows of `row`,
/// which names it, that it lacks.
pub(crate) fn join(existing: &mut Resource, row: Resource) {
    if existing.presentation.is_none() {
        existing.presentation = row.presentation;
    }
    if existing.semantic_identity.is_none() {
        existing.semantic_identity = row
            .semantic_identity
            .filter(ResourceSemanticIdentity::is_valid);
    }
    for action in row.actions {
        if !existing.actions.iter().any(|known| known.id == action.id) {
            existing.actions.push(action);
        }
    }
    let mut views_added = false;
    for view in row.views {
        if !existing.views.iter().any(|known| known.id == view.id) {
            existing.views.push(view);
            views_added = true;
        }
    }
    if views_added {
        // The link actions name the views they were offered with.
        tilo_resource::share::reoffer(existing);
    }
    merge(&mut existing.children, row.children);
}

/// The resource `id` in `resources`, at any depth.
pub(crate) fn find(resources: &[Resource], id: &str) -> Option<Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_retains_the_selector_identity_without_overwriting_the_first_publisher() {
        let identity = ResourceSemanticIdentity::new("chat.agent", "chat.agent:scribe").unwrap();
        let row = || {
            LiveRow::new("agent:1", "chat.agent", "Scribe")
                .semantic_identity(identity.clone())
                .into_resource()
        };
        let mut children = vec![Resource::new("agent:1", "chat.agent", "Document label")];
        merge(&mut children, [row()]);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].semantic_identity, Some(identity.clone()));
        assert_eq!(children[0].label, "Document label");

        let first = ResourceSemanticIdentity::new("chat.agent", "chat.agent:reed").unwrap();
        children[0].semantic_identity = Some(first.clone());
        merge(&mut children, [row()]);
        assert_eq!(children[0].semantic_identity, Some(first));
    }

    #[test]
    fn live_row_retains_only_a_valid_typed_semantic_identity() {
        let identity = ResourceSemanticIdentity::new("chat.agent", "chat.agent:scribe")
            .expect("the bounded typed identity is valid");
        let resource = LiveRow::new("opaque:1", "chat.agent", "Scribe")
            .semantic_identity(identity.clone())
            .into_resource();
        assert_eq!(resource.semantic_identity, Some(identity));

        let invalid = ResourceSemanticIdentity {
            kind: "chat agent".into(),
            key: "chat.agent:scribe".into(),
        };
        assert!(
            LiveRow::new("opaque:2", "chat.agent", "Scribe")
                .semantic_identity(invalid)
                .into_resource()
                .semantic_identity
                .is_none(),
            "a live row never publishes malformed selector metadata"
        );
    }
}
