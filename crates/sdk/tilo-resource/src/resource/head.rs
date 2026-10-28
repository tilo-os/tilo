use super::{
    ActionSpec, Own, Resource, Vocabulary, publish_own, type_epoch, vocabulary, weakening_now,
};

/// [`publish`](super::publish) without the children: the resource's identity, actions and
/// views exactly as `publish` gives them, at a cost that does not grow with
/// the children ([`Resource::children_offer_bound`]). What controls bind to.
pub fn publish_head<R: Resource>(
    id: &str,
    label: &str,
    state: &R::State,
) -> tilo_actions::Resource {
    publish_head_of::<R>(id, label, &head_inputs::<R>(state))
}

/// What [`publish_head`] reads of a state. Two states with equal inputs
/// publish the same head, so a publisher holding the head of one keeps it
/// for the other.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadInputs {
    epoch: u64,
    offered: Vec<&'static str>,
    availability: Vec<tilo_actions::Availability>,
    views: Vec<tilo_actions::ResourceViewSpec>,
    now: Vec<(String, String)>,
}

/// [`HeadInputs`] of `state`: what its head is published from, read
/// without publishing a child or an action.
#[must_use]
pub fn head_inputs<R: Resource>(state: &R::State) -> HeadInputs {
    let declarations = vocabulary::<R>().declarations();
    HeadInputs {
        epoch: type_epoch(),
        offered: R::children_offer_bound(state),
        availability: declarations
            .iter()
            .map(|decl| R::availability(state, decl.id))
            .collect(),
        views: R::views(state),
        now: weakening_now::<R>(state),
    }
}

/// [`publish_head`] of a state whose inputs are `inputs`.
#[must_use]
pub fn publish_head_of<R: Resource>(
    id: &str,
    label: &str,
    inputs: &HeadInputs,
) -> tilo_actions::Resource {
    publish_head_from(vocabulary::<R>(), R::local_operations(), id, label, inputs)
}

/// Kept apart from the resource type so it is compiled once, in the runtime,
/// and not again into every app for each resource it declares.
fn publish_head_from(
    vocabulary: &'static Vocabulary,
    local_operations: Vec<ActionSpec>,
    id: &str,
    label: &str,
    inputs: &HeadInputs,
) -> tilo_actions::Resource {
    let declarations = vocabulary.declarations();
    let availability = |action: &str| {
        declarations
            .iter()
            .position(|decl| decl.id == action)
            .and_then(|at| inputs.availability.get(at))
            .cloned()
            .unwrap_or_default()
    };
    let own = Own {
        availability: Some(&availability),
        views: inputs.views.clone(),
        local_operations,
        now: inputs.now.clone(),
    };
    publish_own(vocabulary, id, label, own, &|decl| {
        inputs.offered.contains(&decl.id)
    })
}
