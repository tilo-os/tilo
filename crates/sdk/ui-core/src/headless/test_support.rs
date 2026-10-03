//! Helpers shared by application tests that drive a root under framework
//! History through the headless [`Driver`].
#![cfg(any(test, feature = "test-support"))]
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test helpers report a broken fixture by panicking the test that called them"
)]

use super::{Driver, InspectionNode};
use crate::a11y::{AccessNode, Action};
use crate::history_mode::{
    HistoryPresentation, HistoryRegistration, HistorySourceRegistration, ManagedRoot,
};
use crate::widget::Widget;

/// Drive `root` with its first History scope registered as unversioned.
///
/// # Panics
/// When the root declares no History or its first scope is not External.
#[must_use]
pub fn unversioned_driver(
    state: crate::State,
    root: Box<dyn Widget>,
    viewport: (u32, u32),
) -> Driver {
    let definition = crate::history_mode::capability::document_history_definition(root.as_ref())
        .expect("the root declares framework History");
    let scope = &definition.scopes[0];
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!(
            "{} must present its first History scope as External",
            definition.app_id
        );
    };
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::Unversioned {
            scope: scope.key,
            reason,
        }],
    )
    .unwrap();
    Driver::from_managed_root(state, ManagedRoot::new(root, registration), viewport).unwrap()
}

/// Assert that open History shows only `limitation`: no timeline, preview
/// sides or restore control.
///
/// # Panics
/// When the limitation is missing or a versioned History control is exported.
pub fn assert_external_history_open(driver: &Driver, limitation: &str) {
    let inspection = driver.inspection_json().unwrap();
    assert!(
        inspection.contains(limitation),
        "missing History limitation {limitation:?}"
    );
    let targets = driver.targets();
    for target in [
        "activity_timeline",
        "history-before",
        "history-after",
        "history-restore",
    ] {
        assert!(
            !targets.contains_key(target),
            "External History exports {target}"
        );
    }
}

/// Activate the Activity timeline entry recorded for `hash`.
///
/// # Panics
/// When the timeline refuses the activation.
pub fn select_timeline_entry(driver: &mut Driver, hash: automerge::ChangeHash) {
    let id = crate::history_source::entry_id_from_hash(&hash.to_string());
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&id)
    );
    driver
        .perform_semantic_named("activity_timeline", Some(&target), &Action::Activate)
        .unwrap();
}

/// Every semantic node of the current frame, paired with the inspection path
/// of the widget that owns it (the owner [`Driver::perform_semantic_named`]
/// routes to).
#[must_use]
pub fn semantic_nodes(driver: &Driver) -> Vec<(String, AccessNode)> {
    fn access(node: &AccessNode, owner: &str, out: &mut Vec<(String, AccessNode)>) {
        out.push((owner.to_owned(), node.clone()));
        for child in &node.children {
            access(child, owner, out);
        }
    }
    fn widget(node: &InspectionNode, out: &mut Vec<(String, AccessNode)>) {
        if let Some(semantics) = &node.semantics {
            access(semantics, &node.path, out);
        }
        for child in &node.children {
            widget(child, out);
        }
    }
    driver.drain_component_wakes();
    let mut out = Vec::new();
    widget(&driver.inspection().root, &mut out);
    out
}

/// The first semantic node matching `matches`, if any.
#[must_use]
pub fn find_semantic(driver: &Driver, matches: impl Fn(&AccessNode) -> bool) -> Option<AccessNode> {
    semantic_nodes(driver)
        .into_iter()
        .map(|(_, node)| node)
        .find(|node| matches(node))
}

/// The exported AccessKit node labelled `label`, optionally with `role`.
///
/// # Panics
/// When no exported node matches; the message lists the exported labels.
#[must_use]
pub fn a11y_node_labelled(
    driver: &Driver,
    label: &str,
    role: Option<accesskit::Role>,
) -> accesskit::Node {
    let nodes = driver.a11y_nodes();
    if let Some((_, node)) = nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label) && role.is_none_or(|r| node.role() == r))
    {
        return node.clone();
    }
    let labels = nodes
        .iter()
        .filter_map(|(_, node)| node.label())
        .collect::<Vec<_>>();
    panic!("missing accessible {role:?} node {label:?}; exported labels: {labels:?}")
}

/// Every published resource as `(id, label, action ids)`, depth first.
#[must_use]
pub fn resource_snapshot(driver: &Driver) -> Vec<(String, String, Vec<String>)> {
    fn collect(
        resource: &crate::tilo_actions::Resource,
        out: &mut Vec<(String, String, Vec<String>)>,
    ) {
        out.push((
            resource.id.0.clone(),
            resource.label.clone(),
            resource
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect(),
        ));
        for child in &resource.children {
            collect(child, out);
        }
    }
    let mut out = Vec::new();
    for resource in driver.resources() {
        collect(&resource, &mut out);
    }
    out
}
