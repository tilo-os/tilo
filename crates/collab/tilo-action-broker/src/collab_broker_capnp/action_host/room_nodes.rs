//! An app's ask of one node of its workspace's tree: sharing a record or an
//! item of it with the room's members (Chat's live window share), ending
//! that share, or asking what the person may do there.
//!
//! The broker admits the ask for the person at this device in that room
//! only, and hands it to the host, which asks the room: the room decides it
//! as it decides every read and write a share's grant then covers. This
//! device keeps nothing of it, so a share whose room cannot be asked is
//! refused rather than shown as access no grant backs.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionHostError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, NODE_PERMITS_ACTION,
    NODE_RESOURCE_PREFIX, Pending, ReceiptStatus, SHARE_NODE_ACTION, UNSHARE_NODE_ACTION, pending,
    workspace_provider_room,
};

use super::super::CapnpBrokerState;
use crate::services::RoomNodeAsk;

/// The room and node `request` asks of, when it is a node ask.
pub(super) fn node_ask(request: &InvokeRequest) -> Option<(&str, &str)> {
    let room = workspace_provider_room(&request.target.provider)?;
    let node = request
        .target
        .address
        .resource
        .strip_prefix(NODE_RESOURCE_PREFIX)?;
    Some((room, node))
}

/// The ask `request` makes, from its action and arguments.
fn ask_of(request: &InvokeRequest) -> Result<RoomNodeAsk, ActionHostError> {
    let text = |name: &str| {
        match &request.argument {
            InvokeArgument::Json(arguments) => arguments
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            _ => None,
        }
        .ok_or_else(|| ActionHostError::Refused {
            reason: format!("`{name}` is missing"),
        })
    };
    Ok(match request.target.address.action.as_str() {
        SHARE_NODE_ACTION => RoomNodeAsk::Share {
            share: text("share")?,
            access: text("access")?,
        },
        UNSHARE_NODE_ACTION => RoomNodeAsk::Unshare {
            share: text("share")?,
        },
        NODE_PERMITS_ACTION => RoomNodeAsk::Permits,
        _ => return Err(ActionHostError::UnknownAction),
    })
}

impl CapnpBrokerState {
    /// Ask the room `request`'s node is in what it asks, for `caller`: the
    /// person at this device, in that room. Answered whole once the room
    /// answers.
    pub(super) fn invoke_room_node(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let started = (|| {
            let (room, node) = node_ask(request).ok_or(ActionHostError::UnknownAction)?;
            let ask = ask_of(request)?;
            if !super::own_device::is_person_here(caller, origin) || caller.room() != Some(room) {
                return Err(ActionHostError::NotPermitted);
            }
            self.decide_for(caller, room, |_, _, _| ())
                .ok_or(ActionHostError::NotPermitted)?;
            let service =
                self.services
                    .room_nodes
                    .as_ref()
                    .ok_or_else(|| ActionHostError::Unavailable {
                        reason: "This device can't ask the room about what it shares".into(),
                    })?;
            service
                .ask(room, node, ask)
                .map_err(|reason| ActionHostError::Unavailable { reason })
        })();
        let answered = match started {
            Ok(answered) => answered,
            Err(error) => return Pending::ready(Err(error)),
        };
        let invocation = request.invocation;
        let (receipt, answer) = pending();
        answered.then(move |outcome| {
            let status = match outcome {
                Some(Ok(value)) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
                Some(Err(reason)) => ReceiptStatus::Rejected {
                    error: ActionHostError::Refused { reason },
                },
                None => ReceiptStatus::Rejected {
                    error: ActionHostError::Unavailable {
                        reason: "The room did not answer".into(),
                    },
                },
            };
            receipt.complete(Ok(InvocationReceipt { invocation, status }));
        });
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::node_target;

    const NODE: &str = "room:r1/app:so.tilo.notes/kind:notes.notebook/resource:*/item:p1";

    fn request(action: &str, arguments: serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: node_target("r1", NODE, action),
            argument: match arguments {
                serde_json::Value::Object(map) => InvokeArgument::Json(map),
                _ => InvokeArgument::None,
            },
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    #[test]
    fn a_node_ask_names_its_room_node_and_what_it_asks() {
        let share = request(
            SHARE_NODE_ACTION,
            serde_json::json!({ "share": "m1", "access": "edit" }),
        );
        assert_eq!(node_ask(&share), Some(("r1", NODE)));
        assert!(matches!(
            ask_of(&share),
            Ok(RoomNodeAsk::Share { share, access }) if share == "m1" && access == "edit"
        ));
        assert!(matches!(
            ask_of(&request(UNSHARE_NODE_ACTION, serde_json::json!({ "share": "m1" }))),
            Ok(RoomNodeAsk::Unshare { share }) if share == "m1"
        ));
        assert!(matches!(
            ask_of(&request(NODE_PERMITS_ACTION, serde_json::Value::Null)),
            Ok(RoomNodeAsk::Permits)
        ));
        assert!(matches!(
            ask_of(&request(
                SHARE_NODE_ACTION,
                serde_json::json!({ "share": "m1" })
            )),
            Err(ActionHostError::Refused { .. })
        ));
        assert!(matches!(
            ask_of(&request("delete", serde_json::Value::Null)),
            Err(ActionHostError::UnknownAction)
        ));
        let mut other = share;
        other.target.address.resource = "app.so.tilo.notes".into();
        assert_eq!(
            node_ask(&other),
            None,
            "only a node's resource is a node ask"
        );
    }
}
