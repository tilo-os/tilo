//! What a published row's actions are like now: a service or a surface
//! says of one row's state (a process that cannot be killed, a window only a
//! foreign reader may look at) what its published spec carries.

use crate::{Availability, ResourceSpec};

/// One published row's action as its publisher says it stands now: a
/// process that cannot be killed, a window only a foreign reader may look at.
/// Applied to the row's published spec with [`apply_row_statuses`].
#[derive(Clone, Debug, PartialEq)]
pub struct RowStatus {
    /// The id of the row's action, as it publishes it.
    pub action: String,
    /// What it is like now: unavailable, with why.
    pub availability: Availability,
}

/// Lay `statuses` over `resources` and every child, each row's actions by
/// id: what a service or surface publishes about one row of its state, the
/// same shape the app framework's `Publishes::row_status` gives. A status
/// naming an action the row does not offer changes nothing.
pub fn apply_row_statuses(
    resources: &mut [ResourceSpec],
    mut statuses: impl FnMut(&str) -> Vec<RowStatus>,
) {
    fn walk(resource: &mut ResourceSpec, statuses: &mut impl FnMut(&str) -> Vec<RowStatus>) {
        for status in statuses(resource.id.as_str()) {
            for spec in resource
                .actions
                .iter_mut()
                .filter(|spec| spec.id == status.action)
            {
                spec.type_mut().contract.availability = status.availability.clone();
            }
        }
        for child in &mut resource.children {
            walk(child, statuses);
        }
    }
    for resource in resources {
        walk(resource, &mut statuses);
    }
}
