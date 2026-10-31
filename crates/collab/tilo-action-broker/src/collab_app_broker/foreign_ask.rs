//! Where a person at a device that is not theirs is asked.
//!
//! An action their grants leave to them asks first. At a device that is
//! theirs, this host asks them itself ([`super::witness`]). At a foreign
//! device the room is not their own and the person is not at this host's
//! screen: the question is put to them on one of their own devices, which
//! draws it and answers it with what only that device's person chose. The
//! host then issues the confirmation for that one invocation.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    ActionHostError, Caller, CallerPrincipal, Confirmation, ConfirmationQuestion, InvokeRequest,
    Pending,
};

use super::{AppBrokerState, WitnessBinding};

/// How long the person is given to answer on their own device.
pub(super) const ANSWER_WAIT: Duration = Duration::from_secs(60);

impl AppBrokerState {
    /// Whether a question about `caller`'s invocation is asked on another of
    /// the person's own devices rather than here: the caller is a person at
    /// a foreign device, which is exactly when this host's screen is
    /// nobody's to answer on.
    pub(super) fn asks_on_own_device(caller: &Caller) -> bool {
        caller.is_foreign_device()
            && matches!(caller.principal(), CallerPrincipal::Person(_))
            && caller.user_public_key().is_some()
    }

    /// Ask the person whether `caller` may make `request`, on one of their
    /// own devices, and hold their answer to `binding`: the confirmation
    /// they give names this one invocation and dies with this process, like
    /// one given here on the host's own sheet.
    pub(super) fn own_device_witness(
        self: &Arc<Self>,
        caller: &Caller,
        binding: &WitnessBinding,
        request: &InvokeRequest,
    ) -> Pending<Result<Confirmation, ActionHostError>> {
        self.own_device_witness_within(caller, binding, request, ANSWER_WAIT)
    }

    /// [`Self::own_device_witness`], giving the person `within` to answer.
    /// The wait is a parameter so the refusal of an answer that never comes
    /// is observable without a minute's wait.
    pub(crate) fn own_device_witness_within(
        self: &Arc<Self>,
        caller: &Caller,
        binding: &WitnessBinding,
        request: &InvokeRequest,
        within: Duration,
    ) -> Pending<Result<Confirmation, ActionHostError>> {
        let Some(service) = self.services.own_device.as_ref() else {
            // Nobody to ask is nobody's confirmation.
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        };
        let question = asked_there(self, caller, &binding.room, request);
        let asked = match service.ask_person(caller, &question) {
            Ok(asked) => asked,
            Err(error) => return Pending::ready(Err(error)),
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let state = Arc::clone(self);
        let binding = binding.clone();
        // The person answers on their own device, in their own time; nothing
        // here holds a runtime while they decide.
        let spawned = std::thread::Builder::new()
            .name("foreign-confirmation".into())
            .spawn(move || {
                let witness = match asked.wait_timeout(within) {
                    Ok(Some(Ok(Confirmation::Witnessed(_)))) => state
                        .witness_confirmation(&binding)
                        .map(Confirmation::Witnessed)
                        .ok_or(ActionHostError::ConfirmationRequired),
                    // Denied, timed out, or no witness to name: nothing runs.
                    Ok(Some(Ok(_) | Err(_)) | None) | Err(_) => {
                        Err(ActionHostError::ConfirmationRequired)
                    }
                };
                completer.complete(witness);
            });
        if let Err(error) = spawned {
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: format!("the answer could not be awaited: {error}"),
            }));
        }
        answer
    }
}

impl AppBrokerState {
    /// How this host describes the action `target` names — its label, and
    /// what it acts on — for the provider it belongs to. `None` when this
    /// host has no description of it.
    fn described_action(
        &self,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Option<(String, String)> {
        let described = |descriptor: &tilo_actions::ActionDescriptor| {
            (
                descriptor.resource_label.clone(),
                descriptor.action_label.clone(),
            )
        };
        self.services
            .host_steps
            .iter()
            .find(|services| services.provider() == target.provider)
            .and_then(|services| {
                services
                    .invocation(&target.address, None)
                    .map(|descriptor| described(&descriptor))
            })
            .or_else(|| {
                self.live_surfaces(super::action_host::RowScope::Provider(&target.provider))
                    .into_iter()
                    .find_map(|surface| {
                        surface
                            .actions
                            .iter()
                            .find(|action| {
                                action.resource == target.address.resource
                                    && action.action == target.address.action
                            })
                            .map(|action| {
                                (action.resource_label.clone(), action.action_label.clone())
                            })
                    })
            })
    }
}

/// The question as the person's other device draws it.
///
/// The room is not the person's, so this host does not offer them its
/// catalogue: the action and the thing it acts on are named as this host
/// describes them for the provider the invocation names, and by the
/// invocation's own words when this host has no description.
fn asked_there(
    state: &AppBrokerState,
    caller: &Caller,
    room: &str,
    request: &InvokeRequest,
) -> ConfirmationQuestion {
    let mut id = [0u8; 32];
    id[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    id[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let described = state.described_action(&request.target);
    ConfirmationQuestion {
        id,
        // The device they are standing at, as the transport authenticated
        // it: a device of their own, elsewhere, is the other party here.
        device_label: caller.device_public_key().map_or_else(
            || "another device".to_owned(),
            |device| hex::encode(&device[..8]),
        ),
        room: room.to_owned(),
        resource_label: described.as_ref().map_or_else(
            || request.target.address.resource.clone(),
            |(r, _)| r.clone(),
        ),
        action_label: described
            .as_ref()
            .map_or_else(|| request.target.address.action.clone(), |(_, a)| a.clone()),
        // What it risks is not this host's to weigh for a room that is not
        // the person's, so it is read as the gravest a sheet shows.
        risk: tilo_actions::RiskLevel::Destructive.name().to_owned(),
    }
}
