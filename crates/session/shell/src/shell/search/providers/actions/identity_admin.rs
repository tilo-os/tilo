//! `identity.list`, `.activate`, `.remove`, `.protect`, `.devices.list`,
//! `.home-peer.remove` and `.home-peer.retry-removals`: administering the
//! person's identities and devices through the palette, the control socket
//! and agents.
//!
//! The steps run in [`crate::shell::identity_admin`], which the broker's
//! door for Settings shares, under one contract. Listing and changing the
//! store answers at once; removing a home server reaches it over the
//! network, so it answers through a pending ticket the worker's completion
//! wakes and the loop never waits on it. Revoking a device stays
//! `identity.device.revoke`, which runs the same engine step.
//!
//! Every step is refused unless the caller administers the active
//! workspace's identity; every change but delivering pending home-server
//! removals also needs explicit confirmation.

use crate::shell::chrome_state::Chrome;
use crate::shell::device_pairing::Started;
use crate::shell::identity_admin::{self as engine, IdentityAdmin, Store};
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ActionReceipt, InvocationDisposition};

pub(super) const LIST_ID: &str = "identity.list";
pub(super) const ACTIVATE_ID: &str = "identity.activate";
pub(super) const REMOVE_ID: &str = "identity.remove";
pub(super) const PROTECT_ID: &str = "identity.protect";
pub(super) const DEVICES_ID: &str = "identity.devices.list";
pub(super) const HOME_PEER_REMOVE_ID: &str = "identity.home-peer.remove";
pub(super) const HOME_PEER_RETRY_ID: &str = "identity.home-peer.retry-removals";

/// Every shell action this module serves, with the engine step behind it.
const ACTIONS: [(&str, &str); 7] = [
    (LIST_ID, engine::LIST),
    (ACTIVATE_ID, engine::ACTIVATE),
    (REMOVE_ID, engine::REMOVE),
    (PROTECT_ID, engine::PROTECT),
    (DEVICES_ID, engine::DEVICES),
    (HOME_PEER_REMOVE_ID, engine::HOME_PEER_REMOVE),
    (HOME_PEER_RETRY_ID, engine::HOME_PEER_RETRY),
];

/// The engine's step behind shell action `id`.
fn step(id: &str) -> Option<&'static str> {
    ACTIONS
        .into_iter()
        .find(|(action, _)| *action == id)
        .map(|(_, step)| step)
}

pub(super) fn is_identity_action(id: &str) -> bool {
    step(id).is_some()
}

/// The step's contract: the one the broker's door offers too.
pub(super) fn contract(id: &str) -> tilo_actions::ActionContract {
    engine::contract(step(id).unwrap_or(engine::REMOVE))
}

/// Start step `id` for whoever invoked it from the active workspace: only
/// someone who administers that workspace's identity administers it.
pub(super) fn begin(
    admin: &IdentityAdmin,
    chrome: &Chrome,
    id: &str,
    args: &ActionArgs,
) -> Result<InvocationDisposition, ActionError> {
    let step =
        step(id).ok_or_else(|| ActionError::Refused(format!("{id} is not an identity step")))?;
    let identity = {
        let state = chrome.borrow();
        if !super::current_shell_action_authorized(&state, id) {
            return Err(ActionError::Refused(format!(
                "current workspace authority no longer permits {id}"
            )));
        }
        super::active_identity(&state)?.1
    };
    let store = Store::from_env().map_err(ActionError::Refused)?;
    match admin
        .start(step, args, &identity, &store)
        .map_err(ActionError::Refused)?
    {
        Started::Answered(value) => Ok(InvocationDisposition::Accepted(
            ActionReceipt::completed_with(value),
        )),
        Started::Later(answer) => Ok(InvocationDisposition::Pending(
            super::device_pairing::ticket(answer),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_actions_are_listed_contracted_and_take_their_steps_params() {
        for (id, step) in ACTIONS {
            assert!(is_identity_action(id));
            let action = super::super::Action::ALL
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("{id} is in the catalogue"));
            assert_eq!(action.params, engine::params(step), "{id}");
            assert_eq!(
                super::super::shell_action_contract(id),
                engine::contract(step),
                "{id}"
            );
            let (label, subtitle) = engine::copy_keys(step);
            assert_eq!((action.label, action.subtitle), (label, subtitle), "{id}");
        }
        for id in [ACTIVATE_ID, REMOVE_ID, PROTECT_ID, HOME_PEER_REMOVE_ID] {
            assert_eq!(
                contract(id).confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
                "{id} asks first"
            );
            assert!(
                engine::params(step(id).expect("an identity step"))
                    .iter()
                    .any(|param| param.name == "confirm"),
                "{id} takes an enforced confirmation"
            );
        }
        let revoke = super::super::Action::ALL
            .iter()
            .find(|action| action.id == "identity.device.revoke")
            .expect("device revocation is registered");
        assert_eq!(revoke.params, engine::REVOKE_PARAMS);
        assert!(!is_identity_action("identity.device.revoke"));
    }
}
