//! Helpers shared by application tests that drive a root under framework
//! History through the headless [`FrameHarness`].
#![cfg(any(test, feature = "test-support"))]
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test helpers report a broken fixture by panicking the test that called them"
)]

use super::{FrameHarness, InspectionNode};
use crate::a11y::AccessNode;

/// Every semantic node of the current frame, paired with the inspection path
/// of the widget that owns it (the owner [`FrameHarness::perform_semantic_named`]
/// routes to).
#[must_use]
pub fn semantic_nodes(driver: &FrameHarness) -> Vec<(String, AccessNode)> {
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
pub fn find_semantic(
    driver: &FrameHarness,
    matches: impl Fn(&AccessNode) -> bool,
) -> Option<AccessNode> {
    semantic_nodes(driver)
        .into_iter()
        .map(|(_, node)| node)
        .find(|node| matches(node))
}

/// Every published resource as `(id, label, action ids)`, depth first.
#[must_use]
pub fn resource_snapshot(driver: &FrameHarness) -> Vec<(String, String, Vec<String>)> {
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
