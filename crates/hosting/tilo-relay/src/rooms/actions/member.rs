//! A member's own business in a room this relay governs, as actions
//! (provider [`MEMBER_ACTIONS_PROVIDER`]): claiming their topology namespace,
//! recording their own effect receipt, and having the host create the empty
//! record of an app they open there first.
//!
//! None is governance, and none grants anything: each runs a room method
//! that decides it for the person the room certified the calling device to
//! (a claim needs their current Write, a receipt is only their own, and a
//! record only for an app the room lets author and a record they may
//! change). Every person the room admitted is offered all three; an agent or
//! a bot is offered none.

use serde_json::{Value, json};
use tilo_actions::remote::{ActionHostError, Caller, CatalogueRow, InvokeRequest};
use tilo_actions::{ActionKind, ActionParam, ParamType};
use tilo_relay_session::rooms::{
    CLAIM_TOPOLOGY_NAMESPACE, MEMBER_ACTIONS_PROVIDER, MEMBER_RESOURCE_KIND,
    PREPARE_APPLICATION_RECORD, RECORD_EFFECT_RECEIPT, member_resource,
};

use super::{Args, Publisher, Verb, invalid, refused};
use crate::rooms::{GovernedRoom, Rooms};

/// A member's own business.
pub(super) const MEMBER: Publisher = Publisher {
    provider: MEMBER_ACTIONS_PROVIDER,
    kind: MEMBER_RESOURCE_KIND,
};

/// What a member does for themselves, in catalogue order.
const MEMBER_VERBS: [Verb; 3] = [
    Verb {
        id: CLAIM_TOPOLOGY_NAMESPACE,
        label: "Claim topology namespace",
        gate: CLAIM_TOPOLOGY_NAMESPACE,
        kind: ActionKind::Outside,
        params: claim_params,
    },
    Verb {
        id: RECORD_EFFECT_RECEIPT,
        label: "Record effect receipt",
        gate: RECORD_EFFECT_RECEIPT,
        kind: ActionKind::Outside,
        params: receipt_params,
    },
    Verb {
        id: PREPARE_APPLICATION_RECORD,
        label: "Prepare application record",
        gate: PREPARE_APPLICATION_RECORD,
        kind: ActionKind::Outside,
        params: record_params,
    },
];

fn claim_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "salt",
        ParamType::Integer,
        "The namespace, 1 to 65535; one another device owns is never rebound.",
    )]
}

fn receipt_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "invocation",
            ParamType::Text,
            "The invocation the receipt ends, a UUID.",
        ),
        ActionParam::required(
            "redacted",
            ParamType::Text,
            "The canonical redacted public receipt, as JSON text; never the action's arguments or results.",
        ),
    ]
}

fn record_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "app",
            ParamType::Text,
            "The application, which the room must already let author its records.",
        ),
        ActionParam::optional(
            "record",
            ParamType::Text,
            "The record's id on the wire; the app's primary when omitted.",
        ),
    ]
}

/// What a member of `room` is offered for themselves.
pub(super) fn rows(room: &GovernedRoom) -> Vec<CatalogueRow> {
    let resource = member_resource(room.room());
    MEMBER_VERBS
        .iter()
        .map(|verb| verb.row(MEMBER, &resource, &room.record.name))
        .collect()
}

impl Rooms {
    /// Run the member's own `request` from `caller` in the room
    /// `workspace_id` carries, through the room method its wire door called.
    pub(super) async fn member_invoke(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let address = &request.target.address;
        if !MEMBER_VERBS.iter().any(|verb| verb.id == address.action) {
            return Err(ActionHostError::UnknownAction);
        }
        let (governed, device) = self
            .governed_by(workspace_id, caller)
            .await
            .ok_or(ActionHostError::UnknownAction)?;
        let room = {
            let room = governed.lock().await;
            if address.resource != member_resource(room.room()) {
                return Err(ActionHostError::UnknownAction);
            }
            room.room().to_owned()
        };
        let args = Args::of(&request.argument)?;
        let done = match address.action.as_str() {
            CLAIM_TOPOLOGY_NAMESPACE => {
                let salt = args
                    .integer("salt")?
                    .and_then(|salt| u16::try_from(salt).ok())
                    .filter(|salt| *salt != 0)
                    .ok_or_else(|| invalid("`salt` must be a whole number from 1 to 65535"))?;
                self.claim_topology_namespace(&room, &device, salt).await
            }
            RECORD_EFFECT_RECEIPT => {
                let invocation = uuid::Uuid::parse_str(args.required_text("invocation")?)
                    .map_err(|_| invalid("`invocation` must be a UUID"))?;
                let redacted = args.required_text("redacted")?;
                self.record_member_effect_receipt(&room, &device, invocation, redacted.as_bytes())
                    .await
            }
            PREPARE_APPLICATION_RECORD => {
                let request = crate::rooms::member_records::PrepareApplicationRecordRequest {
                    app: args.required_text("app")?.to_owned(),
                    record: args.text("record")?.unwrap_or_default().to_owned(),
                };
                return self
                    .prepare_member_application_record(&room, &device, &request)
                    .await
                    .map(|done| json!({ "created": done.created, "generation": done.generation }))
                    .map_err(|error| refused(&error));
            }
            _ => return Err(ActionHostError::UnknownAction),
        };
        done.map(|done| json!({ "generation": done.generation }))
            .map_err(|error| refused(&error))
    }
}

#[cfg(test)]
mod tests;
