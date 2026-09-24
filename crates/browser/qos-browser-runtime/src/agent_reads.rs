//! Disposable read correlation; current native/browser identity supplies the context.
use std::collections::BTreeMap;
use tilo_relay::{
    agent_reads::AgentReadBinding,
    peer_actions::{ActionMessage, DirectedAction},
    transport_v2::EnvelopeV2,
};
use uuid::Uuid;

struct Pending {
    action: DirectedAction,
    root: [u8; 32],
    binding: AgentReadBinding,
    identity: serde_json::Value,
    generation: u64,
    response: Option<ActionMessage>,
}
/// In-memory requests only. The wasm owner supplies its current authenticated context.
#[derive(Default)]
pub struct PendingAgentReads {
    entries: BTreeMap<Uuid, Pending>,
    publication: Option<(AgentReadBinding, serde_json::Value, u64)>,
}
impl PendingAgentReads {
    /// Called only after ordinary action correlation accepts a certified catalogue.
    pub fn observe_catalogue(
        &mut self,
        message: &ActionMessage,
        identity: &serde_json::Value,
        generation: u64,
    ) {
        let ActionMessage::BrokerCatalogue { host, catalogue } = message else {
            return;
        };
        let publication = catalogue
            .get("agentReadsV1")
            .cloned()
            .and_then(|value| serde_json::from_value::<AgentReadBinding>(value).ok())
            .filter(|binding| {
                tilo_relay::agent_reads::valid_binding(binding, *host)
                    && identity.get("room").and_then(|value| value.as_str())
                        == Some(binding.agent.room.as_str())
                    && identity.get("sender").and_then(|value| value.as_str())
                        == Some(binding.agent.device.as_str())
            })
            .map(|binding| (binding, identity.clone(), generation));
        if self.publication != publication {
            self.entries.clear();
        }
        self.publication = publication;
    }
    /// A JS-provided DTO cannot replace the publication received by Rust.
    pub fn binding(
        &self,
        identity: &serde_json::Value,
        generation: u64,
    ) -> Option<AgentReadBinding> {
        self.publication
            .as_ref()
            .filter(|(_, current, epoch)| *current == *identity && *epoch == generation)
            .map(|(binding, _, _)| binding.clone())
    }
    /// Reserve one bounded disposable correlation before network dispatch.
    pub fn insert(
        &mut self,
        action: DirectedAction,
        root: [u8; 32],
        binding: AgentReadBinding,
        identity: serde_json::Value,
        generation: u64,
    ) -> Result<(), String> {
        let ActionMessage::AgentReadV1 { host, .. } = &action.message else {
            return Err("unsupported agent read".into());
        };
        let source = action
            .source
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect::<String>();
        if !action.validate()
            || !tilo_relay::agent_reads::valid_binding(&binding, *host)
            || binding.agent.room != action.room.to_string()
            || binding.agent.device != source
        {
            return Err("agent read binding mismatch".into());
        }
        if self.entries.len() >= 32 || self.entries.contains_key(&action.request_id) {
            return Err("agent read capacity or duplicate".into());
        }
        self.entries.insert(
            action.request_id,
            Pending {
                action,
                root,
                binding,
                identity,
                generation,
                response: None,
            },
        );
        Ok(())
    }
    /// Accept a result only from the same current identity and certified source.
    pub fn receive(
        &mut self,
        event: &EnvelopeV2,
        identity: &serde_json::Value,
        generation: u64,
    ) -> bool {
        let id = match event {
            EnvelopeV2::Delivered { action, .. } => action.request_id,
            EnvelopeV2::Unavailable { request_id, .. } => *request_id,
            _ => return false,
        };
        let Some(pending) = self.entries.get_mut(&id) else {
            return false;
        };
        if pending.response.is_some()
            || pending.identity != *identity
            || pending.generation != generation
        {
            return false;
        }
        match event {
            EnvelopeV2::Delivered {
                action,
                source_user,
            } => {
                if *source_user != pending.root
                    || action.room != pending.action.room
                    || action.source != pending.action.target
                    || action.target != pending.action.source
                    || !action.validate()
                {
                    return false;
                }
                let ActionMessage::AgentReadV1 {
                    host: expected_host,
                    invocation: expected_invocation,
                    request,
                } = &pending.action.message
                else {
                    return false;
                };
                match &action.message {
                    ActionMessage::AgentReadResultV1 {
                        host,
                        invocation,
                        binding,
                        response,
                    } if host == expected_host
                        && invocation == expected_invocation
                        && *binding == pending.binding
                        && tilo_relay::agent_reads::matches_request(request, response) =>
                    {
                        ()
                    }
                    ActionMessage::Rejected { .. } => (),
                    _ => return false,
                }
                pending.response = Some(action.message.clone());
                true
            }
            EnvelopeV2::Unavailable {
                room,
                target,
                invocation,
                reason,
                ..
            } if *room == pending.action.room
                && *target == pending.action.target
                && invocation.is_none() =>
            {
                pending.response = Some(ActionMessage::Rejected {
                    code: format!("{reason:?}"),
                });
                true
            }
            _ => false,
        }
    }
    /// Consume once. A changed context forgets the stale request even if a response exists.
    pub fn take(
        &mut self,
        id: Uuid,
        identity: &serde_json::Value,
        generation: u64,
    ) -> Option<ActionMessage> {
        let pending = self.entries.get(&id)?;
        if pending.identity != *identity || pending.generation != generation {
            self.entries.remove(&id);
            return None;
        }
        if pending.response.is_none() {
            return None;
        }
        self.entries
            .remove(&id)
            .and_then(|pending| pending.response)
    }
    /// Cancellation and disconnect discard correlation without retaining a durable row.
    pub fn forget(&mut self, id: Uuid) {
        self.entries.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_agent_control_protocol::{AgentBinding, AgentRequest, AgentResponse};
    use serde_json::json;
    use tilo_relay::{
        agent_reads::AgentReadBinding,
        peer_actions::{ActionMessage, DirectedAction},
        transport_v2::EnvelopeV2,
    };
    use uuid::Uuid;

    fn fixture() -> (
        DirectedAction,
        AgentReadBinding,
        serde_json::Value,
        EnvelopeV2,
    ) {
        let room = Uuid::from_u128(1);
        let host = Uuid::from_u128(2);
        let invocation = Uuid::from_u128(3);
        let binding = AgentReadBinding {
            action_host: host.to_string(),
            agent: AgentBinding {
                machine: "machine".into(),
                room: room.to_string(),
                device: "0a".repeat(32),
                host: host.to_string(),
            },
            spawn: None,
            message: None,
        };
        let action = DirectedAction {
            room,
            request_id: Uuid::from_u128(4),
            source: [10; 32],
            target: [20; 32],
            message: ActionMessage::AgentReadV1 {
                host,
                invocation,
                request: AgentRequest::Conversation {
                    version: 1,
                    agent_id: "agent".into(),
                    cursor: None,
                },
            },
        };
        let identity = json!({"room":room,"sender":"0a".repeat(32),"senderRoot":"0b".repeat(32),"target":"14".repeat(32),"targetRoot":"15".repeat(32),"certificate":"current"});
        let reply = EnvelopeV2::Delivered {
            source_user: [21; 32],
            action: DirectedAction {
                room,
                request_id: action.request_id,
                source: action.target,
                target: action.source,
                message: ActionMessage::AgentReadResultV1 {
                    host,
                    invocation,
                    binding: binding.clone(),
                    response: AgentResponse::Conversation {
                        version: 1,
                        invocation_id: invocation.to_string(),
                        binding: binding.agent.clone(),
                        agent_id: "agent".into(),
                        messages: vec![],
                        next_cursor: None,
                        revision: "1".into(),
                    },
                },
            },
        };
        (action, binding, identity, reply)
    }
    #[test]
    fn agent_read_correlates_exact_identity_and_consumes_once() {
        let (action, binding, identity, reply) = fixture();
        let mut pending = PendingAgentReads::default();
        pending
            .insert(action.clone(), [21; 32], binding, identity.clone(), 7)
            .unwrap();
        assert!(pending.receive(&reply, &identity, 7));
        assert!(!pending.receive(&reply, &identity, 7));
        assert!(pending.take(action.request_id, &identity, 7).is_some());
        assert!(pending.take(action.request_id, &identity, 7).is_none());
        assert!(!pending.receive(&reply, &identity, 7));
    }
    #[test]
    fn agent_read_rejects_forged_scope_and_wrong_conversation() {
        for dimension in 0..10 {
            let (action, binding, identity, mut reply) = fixture();
            let mut pending = PendingAgentReads::default();
            pending
                .insert(action, [21; 32], binding, identity.clone(), 7)
                .unwrap();
            let EnvelopeV2::Delivered {
                source_user,
                action,
            } = &mut reply
            else {
                unreachable!()
            };
            match dimension {
                0 => *source_user = [22; 32],
                1 => action.room = Uuid::from_u128(99),
                2 => action.source = [22; 32],
                3 => action.target = [22; 32],
                4 => action.request_id = Uuid::from_u128(99),
                _ => {
                    let ActionMessage::AgentReadResultV1 {
                        host,
                        invocation,
                        binding,
                        response,
                    } = &mut action.message
                    else {
                        unreachable!()
                    };
                    match dimension {
                        5 => *host = Uuid::from_u128(99),
                        6 => *invocation = Uuid::from_u128(99),
                        7 => binding.agent.machine = "other".into(),
                        8 => {
                            let AgentResponse::Conversation { agent_id, .. } = response else {
                                unreachable!()
                            };
                            *agent_id = "other".into();
                        }
                        _ => {
                            let AgentResponse::Conversation { invocation_id, .. } = response else {
                                unreachable!()
                            };
                            *invocation_id = "other".into();
                        }
                    }
                }
            }
            assert!(
                !pending.receive(&reply, &identity, 7),
                "dimension {dimension}"
            );
        }
    }
    #[test]
    fn agent_read_does_not_cross_generation_or_current_certificate_changes() {
        let (action, binding, identity, reply) = fixture();
        let mut pending = PendingAgentReads::default();
        pending
            .insert(action.clone(), [21; 32], binding, identity.clone(), 7)
            .unwrap();
        assert!(!pending.receive(&reply, &identity, 8));
        let mut other = identity.clone();
        other["certificate"] = json!("changed");
        assert!(!pending.receive(&reply, &other, 7));
        assert!(pending.receive(&reply, &identity, 7));
        assert!(pending.take(action.request_id, &other, 7).is_none());
        assert!(pending.take(action.request_id, &identity, 7).is_none());
    }
    #[test]
    fn agent_read_pending_bound_and_forget_release_correlation() {
        let (mut action, binding, identity, reply) = fixture();
        let mut pending = PendingAgentReads::default();
        for i in 1..=32 {
            action.request_id = Uuid::from_u128(i);
            pending
                .insert(
                    action.clone(),
                    [21; 32],
                    binding.clone(),
                    identity.clone(),
                    7,
                )
                .unwrap();
        }
        action.request_id = Uuid::from_u128(33);
        assert!(
            pending
                .insert(
                    action.clone(),
                    [21; 32],
                    binding.clone(),
                    identity.clone(),
                    7
                )
                .is_err()
        );
        pending.forget(Uuid::from_u128(4));
        assert!(!pending.receive(&reply, &identity, 7));
        pending
            .insert(action, [21; 32], binding, identity, 7)
            .unwrap();
    }

    #[test]
    fn agent_read_publication_is_pinned_to_current_context_and_generation() {
        let (_, binding, identity, _) = fixture();
        let mut pending = PendingAgentReads::default();
        assert!(pending.binding(&identity, 7).is_none());
        let message = ActionMessage::BrokerCatalogue {
            host: Uuid::from_u128(2),
            catalogue: json!({"version":1,"surfaces":[],"agentReadsV1":binding}),
        };
        pending.observe_catalogue(&message, &identity, 7);
        assert_eq!(pending.binding(&identity, 7), Some(binding));
        assert!(pending.binding(&identity, 8).is_none());
        let mut changed = identity.clone();
        changed["certificate"] = json!("replacement");
        assert!(pending.binding(&changed, 7).is_none());
        pending.observe_catalogue(
            &ActionMessage::BrokerCatalogue {
                host: Uuid::from_u128(2),
                catalogue: json!({"version":1,"surfaces":[]}),
            },
            &identity,
            7,
        );
        assert!(pending.binding(&identity, 7).is_none());
    }

    #[test]
    fn agent_read_new_publication_cancels_prior_pending_binding() {
        let (action, binding, identity, reply) = fixture();
        let mut pending = PendingAgentReads::default();
        let catalogue = |binding| ActionMessage::BrokerCatalogue {
            host: Uuid::from_u128(2),
            catalogue: json!({"version":1,"surfaces":[],"agentReadsV1":binding}),
        };
        pending.observe_catalogue(&catalogue(binding.clone()), &identity, 7);
        pending
            .insert(
                action.clone(),
                [21; 32],
                binding.clone(),
                identity.clone(),
                7,
            )
            .unwrap();
        let mut next = binding;
        next.agent.machine = "replacement".into();
        pending.observe_catalogue(&catalogue(next), &identity, 7);
        assert!(!pending.receive(&reply, &identity, 7));
        assert!(pending.take(action.request_id, &identity, 7).is_none());
    }
}
