//! Resources a widget publishes: the tree of open [`tilo_actions::Resource`]s
//! with the actions each exports to the palette, agents and scripts, and the
//! bounded pages the broker's pull directory reads.
//!
//! A widget takes part by providing [`PublishesResources`]; the host reaches
//! every laid-out widget through its node map with the functions below.

use automerge::AutoCommit;

use crate::published::SharedResource;
use crate::widget::capability::capability;
use crate::widget::{ActionDirectoryInvocationDomain, ActionDirectoryResourcePage, Widget};

/// A widget that publishes resources.
pub trait PublishesResources: Widget {
    /// The tree of resources this widget currently has open, each with the
    /// actions it exports, e.g. a playing video plus its comments. `doc` lets
    /// a provider enumerate resources living in the document.
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        Vec::new()
    }

    /// Explicit host-local presentation/request controls for the portable
    /// caller palette. Only View operations are accepted; shared effects
    /// remain broker-owned.
    fn caller_view_resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        Vec::new()
    }

    /// One cursor-addressable page of resource roots safe to expose through
    /// the broker pull directory. `max_results` is a hard ceiling; a live
    /// provider must return a non-empty opaque cursor when it has a tail.
    /// `None` is the default: an unbounded contextual [`Self::resources`]
    /// snapshot must not be reached from a remote query or revision.
    fn action_directory_resource_page(
        &self,
        _doc: &AutoCommit,
        _cursor: &[u8],
        _max_results: usize,
    ) -> Option<ActionDirectoryResourcePage> {
        None
    }

    /// Revision of non-document bounded directory roots; `None` when this
    /// widget has no independently mutable remote directory state.
    fn action_directory_revision(&self, _doc: &AutoCommit) -> Option<String> {
        None
    }

    /// Continuity coverage for this owning subtree's effective invocation
    /// domain. Returning a value is a semantic promise, not authority;
    /// unknown coverage stays closed.
    fn action_directory_invocation_domain(
        &self,
        _doc: &AutoCommit,
    ) -> ActionDirectoryInvocationDomain {
        ActionDirectoryInvocationDomain::Unsupported
    }

    /// Resolve an addressable resource omitted from the contextual snapshot.
    fn resource(&self, _doc: &AutoCommit, _id: &str) -> Option<tilo_actions::Resource> {
        None
    }

    /// The tree as one participant (an agent task) sees it, built from that
    /// participant's own view state. `None` is this widget's own view.
    /// Multi-participant widgets override this and [`Self::resource_for`],
    /// and never put one participant's selection into another's tree.
    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        let _ = participant;
        self.resources(doc)
    }

    /// [`Self::resources_for`] as a [`SharedResource`] forest whose
    /// unchanged subtrees are the very nodes of the last one, so consumers
    /// skip what they have seen. `None`, the default, has callers
    /// wrap [`Self::resources_for`]. An answer must equal it exactly once
    /// materialised ([`SharedResource::to_resource`]).
    fn shared_resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Option<Vec<SharedResource>> {
        let _ = (participant, doc);
        None
    }

    /// [`Self::resource`] in `participant`'s tree.
    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        let _ = participant;
        self.resource(doc, id)
    }

    /// Explicit disclosure authority for an actionless resource container in
    /// the broker pull directory. `None` means its label and hierarchy are
    /// not remotely discoverable unless the resource itself has a safe view
    /// action.
    fn action_directory_visibility(
        &self,
        _doc: &AutoCommit,
        _resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        None
    }

    /// The result of document action `address`, which has just applied to
    /// `doc` as `changes`: a compact summary of what it produced, so its
    /// caller needs no second read to see what happened. Asked only for an
    /// action whose contract declares a JSON result, which this value must
    /// satisfy.
    fn document_action_result(
        &self,
        _doc: &AutoCommit,
        _address: &tilo_actions::ActionAddress,
        _changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        None
    }
}

/// The first [`PublishesResources::document_action_result`] in `widget`'s
/// tree, depth first. A publisher that wraps another forwards to it.
pub fn document_action_result(
    widget: &dyn Widget,
    doc: &AutoCommit,
    address: &tilo_actions::ActionAddress,
    changes: &[automerge::Patch],
) -> Option<serde_json::Value> {
    if let Some(value) = capability::<dyn PublishesResources>(widget)
        .and_then(|publisher| publisher.document_action_result(doc, address, changes))
    {
        return Some(value);
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = document_action_result(child, doc, address, changes);
        }
    });
    found
}

/// The resources `widget` has open.
pub fn resources(widget: &(impl Widget + ?Sized), doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
    capability::<dyn PublishesResources>(widget).map_or_else(Vec::new, |w| w.resources(doc))
}

/// The resources `widget` has open, as a shared forest
/// ([`PublishesResources::shared_resources_for`]).
pub fn shared_resources(widget: &(impl Widget + ?Sized), doc: &AutoCommit) -> Vec<SharedResource> {
    let Some(publisher) = capability::<dyn PublishesResources>(widget) else {
        return Vec::new();
    };
    publisher
        .shared_resources_for(None, doc)
        .unwrap_or_else(|| {
            publisher
                .resources(doc)
                .into_iter()
                .map(SharedResource::from_resource)
                .collect()
        })
}

/// `widget`'s caller-palette view resources.
pub fn caller_view_resources(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> Vec<tilo_actions::Resource> {
    capability::<dyn PublishesResources>(widget)
        .map_or_else(Vec::new, |w| w.caller_view_resources(doc))
}

/// One bounded directory page from `widget`.
pub fn action_directory_resource_page(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
    cursor: &[u8],
    max_results: usize,
) -> Option<ActionDirectoryResourcePage> {
    capability::<dyn PublishesResources>(widget)?.action_directory_resource_page(
        doc,
        cursor,
        max_results,
    )
}

/// `widget`'s directory revision.
pub fn action_directory_revision(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> Option<String> {
    capability::<dyn PublishesResources>(widget)?.action_directory_revision(doc)
}

/// `widget`'s invocation-domain coverage.
pub fn action_directory_invocation_domain(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> ActionDirectoryInvocationDomain {
    capability::<dyn PublishesResources>(widget)
        .map_or(ActionDirectoryInvocationDomain::Unsupported, |w| {
            w.action_directory_invocation_domain(doc)
        })
}

/// Resolve resource `id` in `widget`.
pub fn resource(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::Resource> {
    capability::<dyn PublishesResources>(widget)?.resource(doc, id)
}

/// `widget`'s resources as `participant` sees them.
pub fn resources_for(
    widget: &(impl Widget + ?Sized),
    participant: Option<&tilo_actions::ParticipantId>,
    doc: &AutoCommit,
) -> Vec<tilo_actions::Resource> {
    capability::<dyn PublishesResources>(widget)
        .map_or_else(Vec::new, |w| w.resources_for(participant, doc))
}

/// Resolve resource `id` in `participant`'s tree of `widget`.
pub fn resource_for(
    widget: &(impl Widget + ?Sized),
    participant: Option<&tilo_actions::ParticipantId>,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::Resource> {
    capability::<dyn PublishesResources>(widget)?.resource_for(participant, doc, id)
}

/// The action disclosing container `resource_id` of `widget`, if any.
pub fn action_directory_visibility(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
    resource_id: &str,
) -> Option<tilo_actions::ActionAddress> {
    capability::<dyn PublishesResources>(widget)?.action_directory_visibility(doc, resource_id)
}
