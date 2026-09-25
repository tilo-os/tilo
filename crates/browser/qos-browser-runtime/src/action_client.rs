//! Exact authenticated private-response correlation. Never executes effects.
use std::collections::BTreeMap;
use tilo_relay::{
    peer_actions::{ActionMessage, DirectedAction},
    transport_v2::EnvelopeV2,
};
use uuid::Uuid;
#[derive(Default)]
pub struct PendingActions {
    requests: BTreeMap<Uuid, (DirectedAction, Option<ActionMessage>)>,
}
impl PendingActions {
    pub fn insert(&mut self, action: DirectedAction) -> Result<(), String> {
        if !action.validate() {
            return Err("invalid action request".into());
        }
        if self.requests.len() >= 32 || self.requests.contains_key(&action.request_id) {
            return Err("action request overload or duplicate".into());
        }
        self.requests.insert(action.request_id, (action, None));
        Ok(())
    }
    pub fn receive(&mut self, event: EnvelopeV2, root: [u8; 32]) -> bool {
        match event {
            EnvelopeV2::Delivered {
                action,
                source_user,
            } => {
                let Some((request, response)) = self.requests.get_mut(&action.request_id) else {
                    return false;
                };
                if response.is_some()
                    || source_user != root
                    || action.room != request.room
                    || action.source != request.target
                    || action.target != request.source
                    || !action.validate()
                {
                    return false;
                }
                let expected = request.message.invocation();
                let valid = match &action.message {
                    ActionMessage::BrokerCatalogue { host, catalogue } => {
                        matches!(request.message, ActionMessage::CatalogueRefresh { .. })
                            && !host.is_nil()
                            && ActionMessage::Catalogue {
                                catalogue: catalogue.clone(),
                            }
                            .validate()
                    }
                    ActionMessage::Observation {
                        host,
                        invocation,
                        update,
                        ..
                    } => {
                        !host.is_nil()
                            && expected == Some(*invocation)
                            && update
                                .as_ref()
                                .is_none_or(|u| u.invocation() == *invocation)
                            && match &request.message {
                                ActionMessage::InvokeBound { host: bound, .. } => bound == host,
                                _ => true,
                            }
                    }
                    ActionMessage::CancellationRequested { invocation } => {
                        matches!(request.message, ActionMessage::Cancel { .. })
                            && expected == Some(*invocation)
                    }
                    ActionMessage::Rejected { .. } => true,
                    _ => false,
                };
                if !valid {
                    return false;
                }
                *response = Some(action.message);
                true
            }
            EnvelopeV2::Unavailable {
                room,
                request_id,
                target,
                invocation,
                reason,
            } => {
                let Some((request, response)) = self.requests.get_mut(&request_id) else {
                    return false;
                };
                if response.is_some()
                    || request.room != room
                    || request.target != target
                    || request.message.invocation() != invocation
                {
                    return false;
                }
                *response = Some(ActionMessage::Rejected {
                    code: format!("{reason:?}"),
                });
                true
            }
            _ => false,
        }
    }
    pub fn take(&mut self, id: Uuid) -> Option<ActionMessage> {
        if self
            .requests
            .get(&id)
            .is_some_and(|(_, response)| response.is_some())
        {
            self.requests.remove(&id).and_then(|(_, response)| response)
        } else {
            None
        }
    }
    pub fn forget(&mut self, id: Uuid) {
        self.requests.remove(&id);
    }
}
/// Preserve exact u64 route/sequence identities across JavaScript's number boundary.
pub fn browser_response(message: ActionMessage) -> serde_json::Value {
    let mut value = serde_json::to_value(message).expect("private protocol JSON");
    if let Some(surfaces) = value
        .get_mut("catalogue")
        .and_then(|v| v.get_mut("surfaces"))
        .and_then(|v| v.as_array_mut())
    {
        for surface in surfaces {
            if let Some(peer) = surface.get_mut("peer_id") {
                if let Some(id) = peer.as_u64() {
                    *peer = serde_json::Value::String(id.to_string());
                }
            }
        }
    }
    if let Some(sequence) = value.get_mut("update").and_then(|v| v.get_mut("sequence")) {
        if let Some(id) = sequence.as_u64() {
            *sequence = serde_json::Value::String(id.to_string());
        }
    }
    value
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> DirectedAction {
        DirectedAction {
            room: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            source: [3; 32],
            target: [4; 32],
            message: ActionMessage::CatalogueRefresh { scope: None },
        }
    }
    fn response(a: &DirectedAction) -> DirectedAction {
        DirectedAction {
            source: a.target,
            target: a.source,
            message: ActionMessage::BrokerCatalogue {
                host: Uuid::from_u128(5),
                catalogue: serde_json::json!({"version":1,"surfaces":[]}),
            },
            ..a.clone()
        }
    }
    #[test]
    fn javascript_routes_preserve_all_u64_bits() {
        let response = browser_response(ActionMessage::BrokerCatalogue {
            host: Uuid::from_u128(1),
            catalogue: serde_json::json!({"version":1,"surfaces":[{"peer_id":9_007_199_254_740_993u64}]}),
        });
        assert_eq!(
            response["catalogue"]["surfaces"][0]["peer_id"],
            "9007199254740993"
        );
    }
    #[test]
    fn catalogue_above_effect_node_budget_reaches_browser_correlation() {
        let mut pending = PendingActions::default();
        let request = request();
        pending.insert(request.clone()).unwrap();
        let mut reply = response(&request);
        reply.message = ActionMessage::BrokerCatalogue {
            host: Uuid::from_u128(5),
            catalogue: serde_json::json!({"version":1,"surfaces":[{"actions":vec![serde_json::json!({"resource":"document","action":"edit","params":{}});1500]}]}),
        };
        assert!(pending.receive(
            EnvelopeV2::Delivered {
                action: reply,
                source_user: [9; 32]
            },
            [9; 32]
        ));
        assert!(matches!(
            pending.take(request.request_id),
            Some(ActionMessage::BrokerCatalogue { .. })
        ));
    }

    #[test]
    fn authentic_exact_response_and_duplicate() {
        let mut p = PendingActions::default();
        let mut a = request();
        a.message = ActionMessage::CatalogueRefresh {
            scope: Some(tilo_relay::peer_actions::ExactActionRoute {
                app_id: "dev.qos.desktop".into(),
                record_id: String::new(),
                resource: "desktop:session".into(),
                action: "command".into(),
            }),
        };
        p.insert(a.clone()).unwrap();
        let r = response(&a);
        assert!(p.receive(
            EnvelopeV2::Delivered {
                action: r.clone(),
                source_user: [9; 32]
            },
            [9; 32]
        ));
        assert!(!p.receive(
            EnvelopeV2::Delivered {
                action: r,
                source_user: [9; 32]
            },
            [9; 32]
        ));
        assert!(matches!(
            p.take(a.request_id),
            Some(ActionMessage::BrokerCatalogue { .. })
        ));
    }
    #[test]
    fn rejects_wrong_root_room_target_source_and_request() {
        let mut p = PendingActions::default();
        let a = request();
        p.insert(a.clone()).unwrap();
        for field in 0..5 {
            let mut r = response(&a);
            let mut root = [9; 32];
            match field {
                0 => root = [8; 32],
                1 => r.room = Uuid::from_u128(7),
                2 => r.target = [8; 32],
                3 => r.source = [8; 32],
                _ => r.request_id = Uuid::from_u128(7),
            };
            assert!(!p.receive(
                EnvelopeV2::Delivered {
                    action: r,
                    source_user: root
                },
                [9; 32]
            ));
        }
        assert!(p.take(a.request_id).is_none());
    }
    #[test]
    fn bounded_requests_never_evict() {
        let mut p = PendingActions::default();
        for i in 1..=32 {
            let mut a = request();
            a.request_id = Uuid::from_u128(i);
            p.insert(a).unwrap();
        }
        let mut a = request();
        a.request_id = Uuid::from_u128(33);
        assert!(p.insert(a).is_err());
        assert_eq!(p.requests.len(), 32);
    }
}
