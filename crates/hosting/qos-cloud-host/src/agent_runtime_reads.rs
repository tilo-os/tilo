//! Caller-scoped private projections. Reads never admit a control or reconstruct custody.
use crate::{
    Store,
    agent_runtime_owner::{AgentControlId, AgentRuntimePhase},
};
use qos_agent_control_protocol::*;
use qos_peer_state::owner_commit::{
    AgentRuntimeFact, PrivateAgentState, TaskOutcomeV2, VerifiedAgentCaller,
};
use qos_relay::{
    agent_reads::{AgentReadBinding, MAX_RESULT_BYTES},
    peer_actions::ActionMessage,
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(crate) fn read(
    store: &Store,
    caller: &VerifiedAgentCaller,
    invocation: Uuid,
    request: &AgentRequest,
    binding: AgentReadBinding,
    offers: Vec<GrantOffer>,
) -> Result<ActionMessage, String> {
    ReadBasis::capture(store, caller)?.answer(invocation, request, binding, offers)
}

/// Everything a private read of one caller takes from the Store, captured
/// under one Store lock: the refreshed caller, the caller-scoped projection
/// and the cursor key. It is a read-only projection, never an authorization
/// verdict; answering from it opens no Store. The reply streaming in for an
/// unfinished task (owner memory, not Store) is joined at answer time.
pub(crate) struct ReadBasis {
    machine: String,
    room: String,
    host: Uuid,
    current: VerifiedAgentCaller,
    cursor_key: [u8; 32],
    /// `None` while no private agent ledger is selected.
    ledger: Option<LedgerView>,
}
struct LedgerView {
    revision: u64,
    /// `Err` when the retained ledger cannot be projected (InternalError).
    rows: Result<BTreeMap<String, (AgentSnapshot, Vec<Entry>)>, ()>,
}
enum Entry {
    Message(ChatMessage),
    /// The streaming reply of this unfinished task, when one exists then.
    Live {
        agent: String,
        invocation: String,
    },
}
type Rows = BTreeMap<String, (AgentSnapshot, Vec<ChatMessage>)>;
impl ReadBasis {
    /// Call with the Store open (locked); nothing is written.
    pub(crate) fn capture(store: &Store, caller: &VerifiedAgentCaller) -> Result<Self, String> {
        let current = store.refresh_agent_caller(caller)?;
        let host: Uuid = store
            .state
            .room
            .parse()
            .map_err(|_| "invalid private room")?;
        let ledger = store.state.private_agent_ledger.as_ref().map(|ledger| {
            // Count only immutable facts owned by this caller. Aggregate generations expose
            // unrelated private activity and must not become a caller's page revision.
            let legacy_revision = ledger
                .legacy()
                .mutations
                .values()
                .filter(|receipt| receipt.change.caller == *current.certificate())
                .count() as u64;
            let revision = match ledger {
                PrivateAgentState::V1(_) => legacy_revision,
                PrivateAgentState::V2(v) => {
                    let controls: std::collections::BTreeSet<_> = v
                        .facts
                        .values()
                        .filter_map(|receipt| match &receipt.change.fact {
                            AgentRuntimeFact::Admitted { intent, .. }
                                if intent.scope.caller == *current.certificate() =>
                            {
                                Some(receipt.change.invocation.as_str())
                            }
                            _ => None,
                        })
                        .collect();
                    legacy_revision
                        + v.facts
                            .values()
                            .filter(|receipt| controls.contains(receipt.change.invocation.as_str()))
                            .count() as u64
                }
            };
            let began = std::time::Instant::now();
            let rows = project(store, &current).map_err(drop);
            qos_action_broker::agent_timing::slow(
                "agent_read_project",
                None,
                began,
                &[("rows", rows.as_ref().map_or(0, |rows| rows.len() as u64))],
            );
            LedgerView { revision, rows }
        });
        Ok(Self {
            machine: store.state.machine_id.clone(),
            room: store.state.room.clone(),
            host,
            current,
            cursor_key: cursor_key(store),
            ledger,
        })
    }
    /// The certificate of the caller this basis was captured for.
    pub(crate) fn certificate(&self) -> &qos_user_identity::DeviceCert {
        self.current.certificate()
    }
    /// The caller-scoped rows with the current streaming replies joined in.
    fn rows(&self) -> Result<Rows, AgentErrorCode> {
        let rows = self
            .ledger
            .as_ref()
            .ok_or(AgentErrorCode::NotConfigured)?
            .rows
            .as_ref()
            .map_err(|()| AgentErrorCode::InternalError)?;
        let mut joined = BTreeMap::new();
        for (id, (agent, entries)) in rows {
            let mut messages = Vec::with_capacity(entries.len());
            for entry in entries {
                match entry {
                    Entry::Message(message) => messages.push(message.clone()),
                    Entry::Live { agent, invocation } => {
                        // Lane H: the reply streaming in for this caller's unfinished task
                        // (owner-private; replaced by the durable result once it exists).
                        if let Some((id, text, at_ms)) =
                            crate::agent_live::live_reply_message(agent, invocation)
                        {
                            messages.push(ChatMessage {
                                id,
                                role: MessageRole::Agent,
                                text,
                                status: MessageStatus::Sent,
                                timestamp: timestamp(at_ms)
                                    .map_err(|_| AgentErrorCode::InternalError)?,
                            });
                        }
                    }
                }
            }
            joined.insert(id.clone(), (agent.clone(), messages));
        }
        Ok(joined)
    }
    pub(crate) fn answer(
        &self,
        invocation: Uuid,
        request: &AgentRequest,
        binding: AgentReadBinding,
        offers: Vec<GrantOffer>,
    ) -> Result<ActionMessage, String> {
        let current = &self.current;
        if invocation.is_nil()
            || !qos_relay::agent_reads::valid_request(request)
            || binding.agent.machine != self.machine
            || binding.agent.room != self.room
            || binding.agent.host != self.room
            || binding.agent.device != hex::encode(current.device())
        {
            return Err("private read caller/binding/request mismatch".into());
        }
        let host = self.host;
        let error = |code, message: &str| AgentResponse::Error {
            version: 1,
            invocation_id: invocation.to_string(),
            binding: binding.agent.clone(),
            error: AgentError {
                code,
                message: message.into(),
            },
        };
        let response = (|| -> Result<AgentResponse, AgentErrorCode> {
            let revision = self
                .ledger
                .as_ref()
                .ok_or(AgentErrorCode::NotConfigured)?
                .revision;
            if matches!(request, AgentRequest::Capabilities { .. }) {
                let available = binding.spawn.is_some() && binding.message.is_some();
                return Ok(AgentResponse::Capabilities {
                    version: 1,
                    invocation_id: invocation.to_string(),
                    binding: binding.agent.clone(),
                    capabilities: AgentCapabilities {
                        list: true,
                        spawn: available,
                        message: available,
                        grant_offers: if available { offers.clone() } else { vec![] },
                        unavailable_reasons: if available {
                            vec![]
                        } else {
                            vec![AgentUnavailableReason::NotConfigured]
                        },
                    },
                });
            }
            let rows = self.rows()?;
            match request {
                AgentRequest::List { cursor, .. } => {
                    let scope = self.scope(None);
                    let offset =
                        decode_cursor(&self.cursor_key, &scope, revision, cursor.as_deref())
                            .map_err(|_| AgentErrorCode::InvalidCursor)?;
                    let all: Vec<_> = rows.into_values().map(|row| row.0).collect();
                    if offset > all.len() {
                        return Err(AgentErrorCode::InvalidCursor);
                    }
                    let mut count = (all.len() - offset).min(MAX_AGENT_PAGE_ITEMS);
                    loop {
                        let response = AgentResponse::List {
                            version: 1,
                            invocation_id: invocation.to_string(),
                            binding: binding.agent.clone(),
                            agents: all[offset..offset + count].to_vec(),
                            next_cursor: (offset + count < all.len()).then(|| {
                                encode_cursor(&self.cursor_key, &scope, revision, offset + count)
                            }),
                            revision: revision.to_string(),
                        };
                        if fits(host, invocation, &binding, &response) {
                            return Ok(response);
                        }
                        if count == 0 {
                            return Err(AgentErrorCode::InternalError);
                        }
                        count -= 1;
                    }
                }
                AgentRequest::Conversation {
                    agent_id, cursor, ..
                } => {
                    let (_, all) = rows.get(agent_id).ok_or(AgentErrorCode::AgentNotFound)?;
                    let scope = self.scope(Some(agent_id));
                    let offset =
                        decode_cursor(&self.cursor_key, &scope, revision, cursor.as_deref())
                            .map_err(|_| AgentErrorCode::InvalidCursor)?;
                    if offset > all.len() {
                        return Err(AgentErrorCode::InvalidCursor);
                    }
                    let mut count = (all.len() - offset).min(MAX_CONVERSATION_PAGE_ITEMS);
                    loop {
                        let response = AgentResponse::Conversation {
                            version: 1,
                            invocation_id: invocation.to_string(),
                            binding: binding.agent.clone(),
                            agent_id: agent_id.clone(),
                            messages: all[offset..offset + count].to_vec(),
                            next_cursor: (offset + count < all.len()).then(|| {
                                encode_cursor(&self.cursor_key, &scope, revision, offset + count)
                            }),
                            revision: revision.to_string(),
                        };
                        if fits(host, invocation, &binding, &response) {
                            return Ok(response);
                        }
                        if count == 0 {
                            return Err(AgentErrorCode::InternalError);
                        }
                        count -= 1;
                    }
                }
                _ => Err(AgentErrorCode::InvalidRequest),
            }
        })()
        .unwrap_or_else(|code| {
            error(
                code,
                match code {
                    AgentErrorCode::InvalidCursor => "Private page changed or cursor scope differs",
                    AgentErrorCode::AgentNotFound => "Private agent unavailable for this caller",
                    AgentErrorCode::NotConfigured => "Private agent storage is not selected",
                    _ => "Private read unavailable",
                },
            )
        });
        if !qos_relay::agent_reads::valid_result(host, invocation, &binding, &response)
            || !qos_relay::agent_reads::matches_request(request, &response)
            || !fits(host, invocation, &binding, &response)
        {
            return Err("private read exceeded canonical carrier contract".into());
        }
        Ok(ActionMessage::AgentReadResultV1 {
            host,
            invocation,
            binding,
            response,
        })
    }
    fn scope(&self, agent: Option<&str>) -> [u8; 16] {
        let hash = blake3::hash(
            &serde_json::to_vec(&(
                self.machine.as_str(),
                self.room.as_str(),
                self.current.certificate(),
                agent,
            ))
            .expect("serializable private scope"),
        );
        hash.as_bytes()[..16].try_into().expect("fixed hash length")
    }
}
fn fits(
    host: Uuid,
    invocation: Uuid,
    binding: &AgentReadBinding,
    response: &AgentResponse,
) -> bool {
    serde_json::to_vec(&ActionMessage::AgentReadResultV1 {
        host,
        invocation,
        binding: binding.clone(),
        response: response.clone(),
    })
    .is_ok_and(|bytes| bytes.len() <= MAX_RESULT_BYTES)
}
fn project(
    store: &Store,
    caller: &VerifiedAgentCaller,
) -> Result<BTreeMap<String, (AgentSnapshot, Vec<Entry>)>, String> {
    let state = store
        .state
        .private_agent_ledger
        .as_ref()
        .ok_or("private ledger unavailable")?;
    let mut rows = BTreeMap::new();
    for record in state
        .legacy()
        .records
        .values()
        .filter(|record| record.caller == *caller.certificate())
    {
        let AgentRequest::Spawn { name, task, .. } =
            decode_request(&record.request).map_err(|e| e.to_string())?
        else {
            continue;
        };
        // V1 retained no timestamps or recipient evidence. Do not invent conversation events.
        rows.insert(
            record.ids.agent.clone(),
            (
                AgentSnapshot {
                    id: record.ids.agent.clone(),
                    name,
                    description: task,
                    status: if record.phase
                        == qos_peer_state::owner_commit::AgentControlPhase::Failed
                    {
                        AgentStatus::Failed
                    } else {
                        AgentStatus::Waiting
                    },
                    conversation: vec![],
                },
                vec![],
            ),
        );
    }
    let PrivateAgentState::V2(ledger) = state else {
        return Ok(rows);
    };
    let mut facts: Vec<_> = ledger.facts.values().collect();
    facts.sort_by_key(|r| r.generation);
    // Agents whose managed process has a retained exit. A finished task of an
    // agent without one leaves that agent idle and able to take a message.
    let exited: std::collections::BTreeSet<&str> = facts
        .iter()
        .filter_map(|receipt| match &receipt.change.fact {
            AgentRuntimeFact::ProcessExited { exit, .. } => Some(exit.binding.agent.as_str()),
            _ => None,
        })
        .collect();
    for receipt in &facts {
        let AgentRuntimeFact::Admitted {
            ids,
            intent,
            parent_invocation: None,
            ..
        } = &receipt.change.fact
        else {
            continue;
        };
        if intent.scope.caller != *caller.certificate() {
            continue;
        }
        let AgentRequest::Spawn { name, task, .. } =
            decode_request(intent.request.as_bytes()).map_err(|e| e.to_string())?
        else {
            return Err("invalid retained Spawn".into());
        };
        rows.insert(
            ids.agent_id.clone(),
            (
                AgentSnapshot {
                    id: ids.agent_id.clone(),
                    name,
                    description: task,
                    status: AgentStatus::Starting,
                    conversation: vec![],
                },
                vec![],
            ),
        );
    }
    for receipt in facts {
        let AgentRuntimeFact::Admitted {
            ids,
            intent,
            timestamp_ms,
            ..
        } = &receipt.change.fact
        else {
            continue;
        };
        if intent.scope.caller != *caller.certificate() {
            continue;
        }
        let Some((agent, messages)) = rows.get_mut(&ids.agent_id) else {
            continue;
        };
        let request = decode_request(intent.request.as_bytes()).map_err(|e| e.to_string())?;
        let text = match request {
            AgentRequest::Spawn { task, .. } => task,
            AgentRequest::Message { text, .. } => text,
            _ => return Err("invalid retained task".into()),
        };
        let id = AgentControlId::new(
            receipt
                .change
                .invocation
                .parse()
                .map_err(|_| "invalid retained invocation")?,
        )
        .map_err(|e| e.to_string())?;
        // Phase only: a full observation also hashes the whole serialized
        // Store for its receipt, once per retained control, under the lock.
        let (phase, _) = store
            .agent_runtime_phase(caller, &id)
            .map_err(|e| e.to_string())?;
        agent.status = match phase {
            AgentRuntimePhase::Admitted
            | AgentRuntimePhase::Launching
            | AgentRuntimePhase::Bound => AgentStatus::Starting,
            AgentRuntimePhase::Dispatching | AgentRuntimePhase::Completed => AgentStatus::Running,
            AgentRuntimePhase::Failed => AgentStatus::Failed,
            AgentRuntimePhase::Unknown => AgentStatus::Waiting,
        };
        messages.push(Entry::Message(ChatMessage {
            id: ids.message_id.clone(),
            role: MessageRole::User,
            text,
            status: if phase == AgentRuntimePhase::Failed {
                MessageStatus::Failed
            } else {
                MessageStatus::Sent
            },
            timestamp: timestamp(*timestamp_ms)?,
        }));
        if let Some(finished) = ledger
            .facts
            .get(&format!("{}:task_finished", receipt.change.invocation))
        {
            let AgentRuntimeFact::TaskFinished {
                outcome,
                text,
                timestamp_ms,
                ..
            } = &finished.change.fact
            else {
                return Err("invalid retained outcome".into());
            };
            agent.status = if *outcome == TaskOutcomeV2::Succeeded {
                if exited.contains(ids.agent_id.as_str()) {
                    AgentStatus::Completed
                } else {
                    AgentStatus::Waiting
                }
            } else if exited.contains(ids.agent_id.as_str()) {
                AgentStatus::Failed
            } else {
                // The process is alive and took the terminal report: it can
                // take another message; the result text says what failed.
                AgentStatus::Waiting
            };
            if !text.is_empty() {
                messages.push(Entry::Message(ChatMessage {
                    id: format!("{}:result", ids.task_id),
                    role: MessageRole::Agent,
                    text: text.clone(),
                    status: MessageStatus::Sent,
                    timestamp: timestamp(*timestamp_ms)?,
                }));
            }
        } else if ledger
            .facts
            .contains_key(&format!("{}:task_unknown", receipt.change.invocation))
        {
            agent.status = AgentStatus::Waiting;
        } else {
            // Joined when answering (owner memory, not Store): see ReadBasis::rows.
            messages.push(Entry::Live {
                agent: ids.agent_id.clone(),
                invocation: receipt.change.invocation.clone(),
            });
        }
    }
    Ok(rows)
}
fn timestamp(ms: u64) -> Result<String, String> {
    chrono::DateTime::from_timestamp_millis(i64::try_from(ms).map_err(|_| "invalid retained time")?)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .ok_or_else(|| "invalid retained time".into())
}
fn cursor_key(store: &Store) -> [u8; 32] {
    blake3::derive_key("qos-cloud-agent-read-cursor-v1", &store.key().to_bytes())
}
fn encode_cursor(key: &[u8; 32], scope: &[u8; 16], revision: u64, offset: usize) -> String {
    let mut bytes = Vec::with_capacity(44);
    bytes.extend(revision.to_be_bytes());
    bytes.extend((offset as u32).to_be_bytes());
    bytes.extend(scope);
    let tag = blake3::keyed_hash(key, &bytes);
    bytes.extend(&tag.as_bytes()[..16]);
    hex::encode(bytes)
}
fn decode_cursor(
    key: &[u8; 32],
    scope: &[u8; 16],
    revision: u64,
    cursor: Option<&str>,
) -> Result<usize, String> {
    let Some(cursor) = cursor else { return Ok(0) };
    if cursor.len() != 88
        || !cursor
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid private cursor".into());
    }
    let bytes = hex::decode(cursor).map_err(|_| "invalid private cursor")?;
    let tag = blake3::keyed_hash(key, &bytes[..28]);
    let mismatch = bytes[28..]
        .iter()
        .zip(&tag.as_bytes()[..16])
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if mismatch != 0 || bytes[..8] != revision.to_be_bytes() || bytes[12..28] != *scope {
        return Err("stale or cross-scope cursor".into());
    }
    Ok(u32::from_be_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|_| "invalid private cursor")?,
    ) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_peer_state::owner_commit::*;
    fn add(store: &mut Store, device: [u8; 32], id: &str, text: &str) {
        let (caller, mut request) = crate::agent_owner_tests::prepare(store, device, id);
        let CommitOperation::Agent { change } = &mut request.operation else {
            unreachable!()
        };
        let AgentMutation::Prepare {
            request: body,
            grants,
            ..
        } = change.mutation.clone()
        else {
            unreachable!()
        };
        let AgentRequest::Spawn {
            version,
            name,
            grant_set_id,
            ..
        } = decode_request(&body).unwrap()
        else {
            unreachable!()
        };
        **change = AgentChange::new(
            &caller,
            change.ids.clone(),
            AgentMutation::Prepare {
                invocation: id.into(),
                request: canonical_request(&AgentRequest::Spawn {
                    version,
                    name,
                    task: text.into(),
                    grant_set_id,
                })
                .unwrap(),
                grants,
            },
        )
        .unwrap();
        store.commit_agent_request(&request, &caller).unwrap();
        // Historical failed attempts retain their rows without occupying live capacity.
        let caller = store.agent_caller(device).unwrap();
        let CommitOperation::Agent { change } = &request.operation else {
            unreachable!()
        };
        let mut ids = change.ids.clone();
        ids.mutation = format!("failed:{id}");
        let change = AgentChange::new(
            &caller,
            ids,
            AgentMutation::Transition {
                phase: qos_peer_state::owner_commit::AgentControlPhase::Failed,
                error: Some(AgentError {
                    code: AgentErrorCode::NotConfigured,
                    message: "No executor installed".into(),
                }),
            },
        )
        .unwrap();
        store
            .commit_agent_request(
                &OwnerCommitRequest {
                    room: store.state.room.clone(),
                    operation: CommitOperation::Agent {
                        change: Box::new(change),
                    },
                },
                &caller,
            )
            .unwrap();
    }
    fn binding(store: &Store, caller: &VerifiedAgentCaller) -> AgentReadBinding {
        AgentReadBinding {
            action_host: store.state.room.clone(),
            agent: AgentBinding {
                machine: store.state.machine_id.clone(),
                room: store.state.room.clone(),
                device: hex::encode(caller.device()),
                host: store.state.room.clone(),
            },
            spawn: None,
            message: None,
        }
    }
    /// Lane H: the streaming reply appears in the owner's conversation only,
    /// only for the unfinished current task, and disappears once it ends.
    #[test]
    fn agent_runtime_reads_include_live_reply_only_for_the_owner_while_running() {
        let _serial = crate::agent_live::tests::SERIAL
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (_dir, mut store, device) = crate::agent_runtime_owner::launch_stage_test_enrolled();
        let caller = store.agent_caller(device).unwrap();
        store.activate_private_agent_runtime_v2(&caller).unwrap();
        let invocation = Uuid::from_u128(0x2468);
        let intent = crate::agent_runtime_owner::launch_stage_test_intent(&store, &caller);
        let crate::agent_runtime_owner::AgentAdmissionResult::Fresh(admitted) =
            crate::agent_runtime_owner::launch_stage_test_admit(
                &mut store, &caller, invocation, &intent,
            )
            .unwrap()
        else {
            panic!("fresh admission required")
        };
        let agent = admitted.ids.agent_id.clone();
        let conversation = |store: &Store, caller: &VerifiedAgentCaller| {
            read(
                store,
                caller,
                Uuid::new_v4(),
                &AgentRequest::Conversation {
                    version: 1,
                    agent_id: agent.clone(),
                    cursor: None,
                },
                binding(store, caller),
                vec![],
            )
            .unwrap()
        };
        let messages = |message: ActionMessage| match message {
            ActionMessage::AgentReadResultV1 {
                response: AgentResponse::Conversation { messages, .. },
                ..
            } => messages,
            other => panic!("conversation expected: {other:?}"),
        };
        assert_eq!(
            messages(conversation(&store, &caller)).len(),
            1,
            "user task only"
        );
        let update = crate::agent_live::parse_update(
            r#"tilo-live-v1:{"v":1,"seq":3,"state":"replying","reply":{"text":"Sure — writing","done":false}}"#,
        )
        .unwrap();
        crate::agent_live::ingest(&agent, "Private worker", invocation, update, |_, _| None);
        let owner = messages(conversation(&store, &caller));
        assert_eq!(owner.len(), 2);
        assert_eq!(owner[1].id, format!("live-reply-{agent}"));
        assert_eq!(owner[1].role, MessageRole::Agent);
        assert_eq!(owner[1].status, MessageStatus::Sent);
        assert_eq!(owner[1].text, "Sure — writing");
        // Another enrolled member never sees the owner's agent or its reply.
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let second = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let enrollment = crate::tests::approved(store.inspect(20).unwrap(), &root, &second);
        store.apply(&enrollment, 21).unwrap();
        let other = store
            .agent_caller(second.verifying_key().to_bytes())
            .unwrap();
        assert!(matches!(
            conversation(&store, &other),
            ActionMessage::AgentReadResultV1 {
                response: AgentResponse::Error {
                    error: AgentError {
                        code: AgentErrorCode::AgentNotFound,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
        // The task ended: the live reply is gone (the durable result replaces it).
        crate::agent_live::ended(&agent, invocation, true);
        let caller = store.agent_caller(device).unwrap();
        assert_eq!(messages(conversation(&store, &caller)).len(), 1);
    }
    #[test]
    fn agent_runtime_reads_page_under_carrier_bound_and_scope_cursor_without_writing() {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        for n in 0..10 {
            add(&mut store, device, &format!("wide{n}"), &"🦊".repeat(8000));
        }
        let caller = store.agent_caller(device).unwrap();
        let binding = binding(&store, &caller);
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        let request = AgentRequest::List {
            version: 1,
            cursor: None,
        };
        let response = read(
            &store,
            &caller,
            Uuid::new_v4(),
            &request,
            binding.clone(),
            vec![],
        )
        .unwrap();
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_RESULT_BYTES);
        let ActionMessage::AgentReadResultV1 {
            response:
                AgentResponse::List {
                    agents,
                    next_cursor: Some(cursor),
                    ..
                },
            ..
        } = response
        else {
            panic!("expected bounded first page")
        };
        assert!(!agents.is_empty() && agents.len() < 10);
        let response = read(
            &store,
            &caller,
            Uuid::new_v4(),
            &AgentRequest::List {
                version: 1,
                cursor: Some(cursor.clone()),
            },
            binding.clone(),
            vec![],
        )
        .unwrap();
        let ActionMessage::AgentReadResultV1 {
            response:
                AgentResponse::List {
                    agents: last,
                    next_cursor: None,
                    ..
                },
            ..
        } = response
        else {
            panic!("expected remaining page")
        };
        assert_eq!(agents.len() + last.len(), 10);
        let changed = AgentRequest::Conversation {
            version: 1,
            agent_id: agents[0].id.clone(),
            cursor: Some(cursor.clone()),
        };
        assert!(matches!(
            read(
                &store,
                &caller,
                Uuid::new_v4(),
                &changed,
                binding.clone(),
                vec![]
            )
            .unwrap(),
            ActionMessage::AgentReadResultV1 {
                response: AgentResponse::Error {
                    error: AgentError {
                        code: AgentErrorCode::InvalidCursor,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        // Unrelated aggregate bookkeeping does not change private page revision.
        store.commit(store.state.clone()).unwrap();
        assert!(matches!(
            read(
                &store,
                &caller,
                Uuid::new_v4(),
                &AgentRequest::List {
                    version: 1,
                    cursor: Some(cursor.clone())
                },
                binding.clone(),
                vec![]
            )
            .unwrap(),
            ActionMessage::AgentReadResultV1 {
                response: AgentResponse::List { .. },
                ..
            }
        ));
        add(&mut store, device, "later", "new private task");
        assert!(matches!(
            read(
                &store,
                &caller,
                Uuid::new_v4(),
                &AgentRequest::List {
                    version: 1,
                    cursor: Some(cursor)
                },
                binding,
                vec![]
            )
            .unwrap(),
            ActionMessage::AgentReadResultV1 {
                response: AgentResponse::Error {
                    error: AgentError {
                        code: AgentErrorCode::InvalidCursor,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
    }
    #[test]
    fn agent_runtime_reads_list_never_serializes_the_store_per_control() {
        let (_dir, mut store, device) = crate::agent_runtime_owner::launch_stage_test_enrolled();
        let caller = store.agent_caller(device).unwrap();
        store.activate_private_agent_runtime_v2(&caller).unwrap();
        let intent = crate::agent_runtime_owner::launch_stage_test_intent(&store, &caller);
        let crate::agent_runtime_owner::AgentAdmissionResult::Fresh(_) =
            crate::agent_runtime_owner::launch_stage_test_admit(
                &mut store,
                &caller,
                Uuid::from_u128(0x1357),
                &intent,
            )
            .unwrap()
        else {
            panic!("fresh admission required")
        };
        let receipts = || crate::agent_runtime_owner::RUNTIME_RECEIPTS.with(std::cell::Cell::get);
        let before = receipts();
        let response = read(
            &store,
            &caller,
            Uuid::new_v4(),
            &AgentRequest::List {
                version: 1,
                cursor: None,
            },
            binding(&store, &caller),
            vec![],
        )
        .unwrap();
        // The agents panel polls this under the Store lock: a full-state
        // receipt per retained control made each read hundreds of ms and
        // starved the owner's command queue (messages refused as busy).
        assert_eq!(receipts(), before, "list read computed runtime receipts");
        let ActionMessage::AgentReadResultV1 {
            response: AgentResponse::List { agents, .. },
            ..
        } = response
        else {
            panic!("expected a list page")
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, AgentStatus::Starting);
    }
    #[test]
    fn agent_runtime_reads_other_enrolled_device_cannot_observe_private_rows_or_revision() {
        let (_dir, mut store, first) = crate::agent_owner_tests::enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(first).unwrap())
            .unwrap();
        add(&mut store, first, "private", "secret");
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let second = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let enrollment = crate::tests::approved(store.inspect(20).unwrap(), &root, &second);
        store.apply(&enrollment, 21).unwrap();
        let caller = store
            .agent_caller(second.verifying_key().to_bytes())
            .unwrap();
        let response = read(
            &store,
            &caller,
            Uuid::new_v4(),
            &AgentRequest::List {
                version: 1,
                cursor: None,
            },
            binding(&store, &caller),
            vec![],
        )
        .unwrap();
        let ActionMessage::AgentReadResultV1 {
            response: AgentResponse::List {
                agents, revision, ..
            },
            ..
        } = response
        else {
            panic!("list missing")
        };
        assert!(agents.is_empty());
        assert_eq!(revision, "0", "another caller leaked private revision");
    }
}
