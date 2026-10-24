//! Moving the room to another person, as governance actions
//! ([`ROOM_GOVERNANCE_PROVIDER`] on `workspace.permissions:<room>`): the
//! owner offers the room to a member or takes the offer back, and a device
//! of the person it is offered to accepts it, to govern the room from then
//! on. Each runs the [`Rooms`] method of the same name
//! (`rooms::ownership`), which decides it; the catalogue offers each only to
//! whom that decision would let take it, and invoking anything else is the
//! same `UnknownAction` as invoking what does not exist.
//!
//! [`ROOM_GOVERNANCE_PROVIDER`]: tilo_actions::remote::ROOM_GOVERNANCE_PROVIDER

use serde_json::{Value, json};
use tilo_action_policy::roles::{self, governance};
use tilo_actions::remote::{
    ActionHostError, Caller, CatalogueRow, InvokeRequest, room_governance_resource,
};
use tilo_actions::{ActionParam, ParamType};
use tilo_peer_state::handoff::HandoffAcceptance;
use tilo_user_identity::DeviceCert;

use super::{Args, GOVERNANCE, Verb, invalid, refused, request_id};
use crate::host_authority::HostOperation;
use crate::rooms::{GovernedRoom, Rooms};

/// Offering the room to another member, in place of any open offer.
pub(crate) const PROPOSE: &str = "ownership.propose";
/// Taking the open offer back.
pub(crate) const CANCEL: &str = "ownership.cancel";
/// Accepting the offer made to the caller's person, to govern the room
/// from the calling device.
pub(crate) const ACCEPT: &str = "ownership.accept";

/// Moving the room, in catalogue order. Each is offered by [`offered`],
/// not by a governance gate: only the owner moves the room, and only the
/// recipient takes it.
const OWNERSHIP_VERBS: [Verb; 3] = [
    Verb {
        id: PROPOSE,
        label: "Offer ownership",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: propose_params,
    },
    Verb {
        id: CANCEL,
        label: "Cancel ownership offer",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: cancel_params,
    },
    Verb {
        id: ACCEPT,
        label: "Accept ownership",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: accept_params,
    },
];

fn propose_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "person",
        ParamType::Text,
        "The member the workspace is offered to: their user root key, 64 hex characters.",
    )]
}

fn cancel_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "offer",
        ParamType::Text,
        "The open offer's id, 64 hex characters.",
    )]
}

fn accept_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "acceptance",
        ParamType::Text,
        "The calling device's signed acceptance of the open offer, as the hex of its encoding.",
    )]
}

/// Whether `action` moves the room.
pub(super) fn serves(action: &str) -> bool {
    OWNERSHIP_VERBS.iter().any(|verb| verb.id == action)
}

impl Rooms {
    /// Whether `device` may take `verb` in `room` now: offering and taking
    /// back are the owner's, while the room is governed here; accepting is
    /// the recipient's, while an offer to them is open.
    fn offered(&self, room: &GovernedRoom, device: &DeviceCert, verb: &str) -> bool {
        if room.moved() {
            return false;
        }
        let owner = HostOperation::TransferOwnership.permits(
            &self.inner.host.cert.user_public_key,
            &device.user_public_key,
        );
        let open = room.peer().governance().ownership_offer();
        match verb {
            PROPOSE => owner,
            CANCEL => owner && open.is_some(),
            ACCEPT => open.is_some_and(|offer| offer.recipient == device.user_public_key),
            _ => false,
        }
    }

    /// The ways of moving `room` `device` may take now, as rows on its
    /// governance resource `resource`.
    pub(super) fn ownership_rows(
        &self,
        room: &GovernedRoom,
        device: &DeviceCert,
        resource: &str,
    ) -> Vec<CatalogueRow> {
        OWNERSHIP_VERBS
            .iter()
            .filter(|verb| self.offered(room, device, verb.id))
            .map(|verb| verb.row(GOVERNANCE, resource, &room.record.name))
            .collect()
    }

    /// Run `request`, a way of moving the room, from `caller` against the
    /// room `workspace_id` carries.
    pub(super) async fn ownership_invoke(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let address = &request.target.address;
        let verb = OWNERSHIP_VERBS
            .iter()
            .find(|verb| verb.id == address.action)
            .ok_or(ActionHostError::UnknownAction)?;
        let (governed, device) = self
            .governed_by(workspace_id, caller)
            .await
            .ok_or(ActionHostError::UnknownAction)?;
        let room = {
            let room = governed.lock().await;
            // Unknown and forbidden are the same answer.
            if address.resource != room_governance_resource(room.room())
                || !self.offered(&room, &device, verb.id)
            {
                return Err(ActionHostError::UnknownAction);
            }
            room.room().to_owned()
        };
        let args = Args::of(&request.argument)?;
        let request_id = request_id(&device.user_public_key, request.invocation);
        match verb.id {
            PROPOSE => {
                let offer = self
                    .propose_ownership(&room, &device, request_id, args.required_key("person")?)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({
                    "offer": hex::encode(offer.offer_id),
                    "person": hex::encode(offer.recipient),
                    "expires_at_ns": offer.expires_at_ns,
                }))
            }
            CANCEL => {
                let generation = self
                    .cancel_ownership_offer(&room, &device, request_id, args.required_key("offer")?)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({ "generation": generation }))
            }
            ACCEPT => {
                let bytes = hex::decode(args.required_text("acceptance")?)
                    .map_err(|_| invalid("`acceptance` must be hex"))?;
                let acceptance: HandoffAcceptance = postcard::from_bytes(&bytes)
                    .map_err(|error| invalid(format!("`acceptance` does not decode: {error}")))?;
                let handoff = self
                    .accept_ownership(&room, &device, request_id, acceptance)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(json!({ "generation": handoff.generation }))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests;
