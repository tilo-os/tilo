//! What a widget publishes as resources: the tree of open
//! [`tilo_actions::ResourceSpec`]s with the actions each exports to the
//! palette, agents and scripts, and the bounded pages the broker's pull
//! directory reads. The walks that compose a tree are above the toolkit; a
//! widget takes part by providing [`PublishesResources`].

use automerge::AutoCommit;

use crate::published::SharedResource;
use crate::widget::capability::capability;
use crate::widget::{ActionDirectoryInvocationDomain, ActionDirectoryResourcePage, Widget};

pub trait PublishesResources: Widget {
    /// The tree of resources this widget currently has open, each with the actions it exports, e.g.
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }

    /// Explicit host-local presentation/request controls for the portable caller palette.
    fn caller_view_resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }

    /// One cursor-addressable page of resource roots safe to expose through the broker pull directory.
    /// `max_results` is a hard ceiling.
    fn action_directory_resource_page(
        &self,
        _doc: &AutoCommit,
        _cursor: &[u8],
        _max_results: usize,
    ) -> Option<ActionDirectoryResourcePage> {
        None
    }

    /// Revision of non-document bounded directory roots; `None` when this widget has no independently
    /// mutable remote directory state.
    fn action_directory_revision(&self, _doc: &AutoCommit) -> Option<String> {
        None
    }

    /// Continuity coverage for this owning subtree's effective invocation domain.
    fn action_directory_invocation_domain(
        &self,
        _doc: &AutoCommit,
    ) -> ActionDirectoryInvocationDomain {
        ActionDirectoryInvocationDomain::Unsupported
    }

    /// Resolve an addressable resource omitted from the contextual snapshot: a publisher that
    /// lists its tree answers here from that list, so one resolver serves every caller; a
    /// publisher whose lookup is cheaper by id overrides this.
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::ResourceSpec> {
        self.resources(doc)
            .into_iter()
            .find(|spec| spec.id.as_str() == id)
    }

    /// Whether [`Self::resource`] answers by id without listing everything the publisher holds:
    /// the default builds the whole surface, so a publisher that overrides `resource` says so
    /// here, and a remote read may ask for one resource without paying for the surface.
    fn resolves_resources_by_id(&self) -> bool {
        false
    }

    /// The item resources the published tree leaves out because their collection holds more than one
    /// page of items, each resolved as [`Self::resource`] resolves it: what a listing paging past the
    /// published page lists, so every item is reachable. Children of the record resource.
    fn resources_past_page(&self, _doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }

    /// How many items [`Self::resources_past_page`] lists: counted without
    /// building them, so a caller paging a record of thousands asks its length
    /// and not its items.
    fn past_page_len(&self, doc: &AutoCommit) -> usize {
        self.resources_past_page(doc).len()
    }

    /// The items past the published pages as their id and label, read
    /// without building a resource for each: what a name lookup scans.
    fn past_page_labels(&self, doc: &AutoCommit) -> Vec<(String, String)> {
        self.resources_past_page(doc)
            .into_iter()
            .map(|resource| (resource.id.as_str().to_owned(), resource.label.clone()))
            .collect()
    }

    /// The items [`Self::resources_past_page`] lists from `offset` on, at most
    /// `take`, built alone: a page costs its window and not every item the
    /// record holds.
    fn past_page_range(
        &self,
        doc: &AutoCommit,
        offset: usize,
        take: usize,
    ) -> Vec<tilo_actions::ResourceSpec> {
        self.resources_past_page(doc)
            .into_iter()
            .skip(offset)
            .take(take)
            .collect()
    }

    /// Whether `id`, which resolves to nothing now, may resolve once what the widget is reading off the
    /// UI thread lands (a row of a list not read yet). A caller resolving it is answered then, never
    /// kept waiting on a blocking read nor told it is unknown before the read ends.
    fn resource_pending(&self, _id: &str) -> bool {
        false
    }

    /// The tree as one presence (an agent task) sees it, built from that presence's own view
    /// state. Multi-presence widgets override this and [`Self::resource_for`], and never put one
    /// presence's selection into another's tree.
    fn resources_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::ResourceSpec> {
        let _ = presence;
        self.resources(doc)
    }

    /// [`Self::resources_for`] as a [`SharedResource`] forest whose unchanged subtrees are the very
    /// nodes of the last one, so consumers skip what they have seen. An answer must equal it exactly
    /// once materialised ([`SharedResource::to_resource`]).
    fn shared_resources_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Option<Vec<SharedResource>> {
        let _ = (presence, doc);
        None
    }

    /// [`Self::resource`] in `presence`'s tree.
    fn resource_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        let _ = presence;
        self.resource(doc, id)
    }

    /// `spec`, an action of the resource `id` as the document declares it, as this widget publishes
    /// it now: what its surfaces say of that row laid over it (whether the effect can be used there,
    /// how it is named, the parameters it narrows). A caller resolving one address asks this, not the
    /// published tree, whose building no invocation may cost.
    fn published_action(
        &self,
        _id: &str,
        spec: tilo_actions::ActionSpec,
    ) -> tilo_actions::ActionSpec {
        spec
    }

    /// Explicit disclosure authority for an actionless resource container in the broker pull directory.
    fn action_directory_visibility(
        &self,
        _doc: &AutoCommit,
        _resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        None
    }

    /// The result of document action `address`, which has just applied to `doc` as `changes`: a compact
    /// summary of what it produced, so its caller needs no second read to see what happened. Asked only
    /// for an action whose contract declares a JSON result, which this value must satisfy.
    fn document_action_result(
        &self,
        _doc: &AutoCommit,
        _address: &tilo_actions::ActionAddress,
        _changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        None
    }
}

/// The first [`PublishesResources::document_action_result`] in `widget`'s tree, depth first.
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

/// Whether `operation` may be published on `on` by a widget below its component's root: views and
/// effects may, a document mutation never does.
pub use tilo_actions::outside_document::admits_outside_document;
/// `resource` without document actions, at any depth: what a widget below its component's root may
/// publish.
pub use tilo_actions::outside_document::outside_document;
/// `resource`, resolved by id below its component's root, without document actions.
pub use tilo_actions::outside_document::resolved_outside_document;

pub fn resources(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget).map_or_else(Vec::new, |w| w.resources(doc))
}

/// [`PublishesResources::published_action`] of `widget`.
pub fn published_action(
    widget: &(impl Widget + ?Sized),
    id: &str,
    spec: tilo_actions::ActionSpec,
) -> tilo_actions::ActionSpec {
    match capability::<dyn PublishesResources>(widget) {
        Some(publisher) => publisher.published_action(id, spec),
        None => spec,
    }
}

/// The resource roots a walk holds: a widget answering for the widget below it and that widget
/// itself publish the same resource, which belongs in the tree once.
#[derive(Default)]
pub struct TakenResources(std::collections::HashSet<String>);

impl TakenResources {
    /// Whether this walk holds resource root `id` already.
    pub fn holds(&self, id: &str) -> bool {
        self.0.contains(id)
    }

    /// Hold `id`. A widget publishing it twice is its own defect, which the tree reports.
    pub fn take(&mut self, id: &str) {
        self.0.insert(id.to_owned());
    }
}

/// The resources `widget` has open, as a shared forest ([`PublishesResources::shared_resources_for`]).
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

pub fn caller_view_resources(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget)
        .map_or_else(Vec::new, |w| w.caller_view_resources(doc))
}

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
) -> Option<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget)?.resource(doc, id)
}

/// Whether `widget`'s publisher answers for one resource without listing its surface
/// ([`PublishesResources::resolves_resources_by_id`]).
#[must_use]
pub fn resolves_resources_by_id(widget: &(impl Widget + ?Sized)) -> bool {
    capability::<dyn PublishesResources>(widget)
        .is_some_and(PublishesResources::resolves_resources_by_id)
}

/// [`PublishesResources::resources_past_page`] of `widget`.
pub fn resources_past_page(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget)
        .map_or_else(Vec::new, |w| w.resources_past_page(doc))
}

/// [`PublishesResources::past_page_len`] of `widget`.
pub fn past_page_len(widget: &(impl Widget + ?Sized), doc: &AutoCommit) -> usize {
    capability::<dyn PublishesResources>(widget).map_or(0, |w| w.past_page_len(doc))
}

/// [`PublishesResources::past_page_labels`] of `widget`.
pub fn past_page_labels(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
) -> Vec<(String, String)> {
    capability::<dyn PublishesResources>(widget).map_or_else(Vec::new, |w| w.past_page_labels(doc))
}

/// [`PublishesResources::past_page_range`] of `widget`.
pub fn past_page_range(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
    offset: usize,
    take: usize,
) -> Vec<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget)
        .map_or_else(Vec::new, |w| w.past_page_range(doc, offset, take))
}

/// `widget`'s resources as `presence` sees them.
pub fn resources_for(
    widget: &(impl Widget + ?Sized),
    presence: Option<&tilo_actions::PresenceId>,
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget)
        .map_or_else(Vec::new, |w| w.resources_for(presence, doc))
}

/// Resolve resource `id` in `presence`'s tree of `widget`.
pub fn resource_for(
    widget: &(impl Widget + ?Sized),
    presence: Option<&tilo_actions::PresenceId>,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::ResourceSpec> {
    capability::<dyn PublishesResources>(widget)?.resource_for(presence, doc, id)
}

/// Whether resource `id` of `widget` may resolve once what it reads lands
/// ([`PublishesResources::resource_pending`]).
pub fn resource_pending(widget: &(impl Widget + ?Sized), id: &str) -> bool {
    capability::<dyn PublishesResources>(widget).is_some_and(|w| w.resource_pending(id))
}

/// The action disclosing container `resource_id` of `widget`, if any.
pub fn action_directory_visibility(
    widget: &(impl Widget + ?Sized),
    doc: &AutoCommit,
    resource_id: &str,
) -> Option<tilo_actions::ActionAddress> {
    capability::<dyn PublishesResources>(widget)?.action_directory_visibility(doc, resource_id)
}
