//! The Settings panes as resources: each pane is a section with an `open`
//! view that search, agents and accessibility use to reveal it.
//!
//! Preferences are not here: each is a [`crate::effects::SettingsEffect`]
//! that writes the person's or the device's settings record.

use automerge::AutoCommit;

use tilo_actions::{ActionAddress, ActionSpec, OperationKind, Resource};

use crate::categories::Category;

/// Operation protocol used by every Settings pane's exported presentation
/// action. The payload is the category's stable slug.
pub const OPEN_PANE_OPERATION_TYPE: &str = "settings.open-pane";

/// Stable action id shared by every pane resource.
pub const OPEN_PANE_ACTION: &str = "open";

/// Stable resource identity for a Settings pane.
pub fn pane_resource_id(category: Category) -> String {
    format!("settings.section:{}", category.slug())
}

/// Canonical address used by search, agents, accessibility, and resource-tree
/// navigation to reveal exactly one Settings pane.
pub fn open_pane_address(category: Category) -> ActionAddress {
    ActionAddress::new(pane_resource_id(category), OPEN_PANE_ACTION)
}

/// Build the minimal, canonical resource needed to resolve a Settings search
/// result lazily.  Search navigation is deliberately independent from the
/// eager resource snapshot: opening a category must remain available even
/// while a dynamic pane is loading or its device inventory is unavailable.
///
/// Keep this constructor next to [`open_pane_address`] so the address payload,
/// action id and resource id cannot drift apart between the search provider
/// and the full resource tree.
pub fn pane_navigation_resource(category: Category) -> Resource {
    let mut pane = Resource::new(
        pane_resource_id(category),
        "settings.section",
        category.label(),
    );
    if let Ok(open) = ActionSpec::operation(
        OPEN_PANE_ACTION,
        format!("Open {}", category.label()),
        OperationKind::View,
        OPEN_PANE_OPERATION_TYPE,
        &category.slug(),
    ) {
        pane = pane.with_action(open);
    }
    pane
}

/// Every pane, as a section.
pub(crate) fn live_device_resources(_doc: &AutoCommit) -> Vec<Resource> {
    Category::ALL
        .iter()
        .copied()
        .map(pane_navigation_resource)
        .collect()
}

/// The device sections under one root, as a test reads them.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("settings.root", "settings.root", "Settings");
    for section in live_device_resources(doc) {
        root = root.with_child(section);
    }
    vec![root]
}

impl crate::SettingsApp {
    /// The pane `id`, when an invocation names it.
    pub fn publish_resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
        Category::ALL
            .iter()
            .copied()
            .find(|category| pane_resource_id(*category) == id)
            .map(pane_navigation_resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ctrl+K tree, stable pane addresses, lookups, bounded inventories and refused stale targets.
    #[test]
    fn settings_tree_addresses_are_stable_shallow_and_bounded() {
        // Case `pane_addresses_are_stable_and_unique`.
        {
            let mut addresses = std::collections::HashSet::new();
            for category in Category::ALL.iter().copied() {
                let address = open_pane_address(category);
                assert_eq!(
                    address.resource,
                    format!("settings.section:{}", category.slug())
                );
                assert_eq!(address.action, OPEN_PANE_ACTION);
                assert!(
                    addresses.insert(address),
                    "duplicate address for {category:?}"
                );
            }
        }
    }
}
