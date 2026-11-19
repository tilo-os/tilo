//! Live external rows: what a surface shows that lives outside the
//! resource's document — processes, devices, containers, journal entries,
//! fetched catalogue rows, running jobs — published beneath the resource for
//! the palette, agents and scripts.
//!
//! The row itself is [`tilo_resource::row::LiveRow`], the builder a session
//! or an app reaches without the app framework; this module re-exports it and
//! adds how the framework's publication merges rows into the resource's
//! children.

pub use tilo_resource::row::LiveRow;

use tilo_actions::{ResourceSemanticIdentity, ResourceSpec};

pub(crate) use tilo_resource::share::{outside_document, resolved_outside_document};

/// An application the shell lists, as the type its rows are items of: the
/// kind every app's application rows share.
pub struct Application;

impl tilo_resource::words::RowType for Application {
    const KIND: &'static str = "applications.app";
    const LABEL: &'static str = "Application";
}

/// Merge `rows` by id into `children`: a row naming an existing child adds
/// the operations and rows that child lacks, and its presentation when the
/// child has none; any other row is appended. The first publisher of an id
/// keeps its label and kind.
pub(crate) fn merge(
    children: &mut Vec<ResourceSpec>,
    rows: impl IntoIterator<Item = ResourceSpec>,
) {
    for row in rows {
        match children.iter_mut().find(|child| child.id == row.id) {
            Some(existing) => join(existing, row),
            None => children.push(row),
        }
    }
}

/// Add to `existing` the presentation, semantic identity, operations, views and rows of `row`,
/// which names it, that it lacks.
pub(crate) fn join(existing: &mut ResourceSpec, row: ResourceSpec) {
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

pub(crate) fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// The actions `row` adds to the published child of its id, each with the
/// statuses `statuses` say of it now laid over: a row's effect the live state
/// makes unavailable stays off the palette, agents and the API with its
/// reason.
pub(crate) fn apply_statuses(
    resource: &mut ResourceSpec,
    statuses: &[crate::framework::EffectStatus],
) {
    for status in statuses {
        for spec in resource
            .actions
            .iter_mut()
            .filter(|spec| spec.id == status.id())
        {
            status.overlay(spec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_live_row_keeps_its_semantic_identity;

        phase merging_retains_the_selector_identity_without_overwriting_the_first_publisher;
        {
            let identity = ResourceSemanticIdentity::new("chat.agent", "chat.agent:scribe").unwrap();
            let row = || {
                LiveRow::new("agent:1", "chat.agent", "Scribe")
                    .semantic_identity(identity.clone())
                    .into_resource()
            };
            let mut children = vec![ResourceSpec::new("agent:1", "chat.agent", "Document label")];
            merge(&mut children, [row()]);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].semantic_identity, Some(identity.clone()));
            assert_eq!(children[0].label, "Document label");

            let first = ResourceSemanticIdentity::new("chat.agent", "chat.agent:reed").unwrap();
            children[0].semantic_identity = Some(first.clone());
            merge(&mut children, [row()]);
            assert_eq!(children[0].semantic_identity, Some(first));
        }

        phase live_row_retains_only_a_valid_typed_semantic_identity;
        {
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
}
