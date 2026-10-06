//! Atomic canonical Chat publication and private addressed-input admission.
//! A valid pinned-host signature is necessary, not sufficient: the resulting
//! admitted document must contain the exact unique canonical request row.
use crate::{
    AgentMessageAdmissionAttestation, NativeCommittedView, NativeWorkspaceOwner, RoomId,
    agent_message::{
        self, MessageOutcome, MessagePhase, MessageSource, StoredAgentMessage, TerminalMessage,
    },
    agent_message_history, publication,
};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use std::{collections::BTreeSet, time::SystemTime};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};

fn object(doc: &AutoCommit, parent: &ObjId, key: &str, kind: ObjType) -> Result<ObjId, String> {
    let values = doc
        .get_all(parent, key)
        .map_err(|_| "Agent Message object cannot be read")?;
    match values.as_slice() {
        [(Value::Object(actual), id)] if *actual == kind => Ok(id.clone()),
        _ => Err("Agent Message object is missing or conflicted".into()),
    }
}

fn scalar(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<automerge::ScalarValue, String> {
    let values = doc
        .get_all(parent, key)
        .map_err(|_| "Agent Message scalar cannot be read")?;
    match values.as_slice() {
        [(Value::Scalar(value), _)] => Ok(value.as_ref().clone()),
        _ => Err("Agent Message scalar is missing or conflicted".into()),
    }
}

fn equal_text(doc: &AutoCommit, parent: &ObjId, key: &str, expected: &str) -> Result<(), String> {
    if scalar(doc, parent, key)?.as_str() != Some(expected) {
        return Err("Agent Message canonical row differs from typed source".into());
    }
    Ok(())
}

fn document(view: &NativeCommittedView) -> Result<AutoCommit, String> {
    if view
        .peer()
        .plaintext_complete_heads("so.tilo.chat", "")
        .is_none()
    {
        return Err("Agent Message canonical document is not complete plaintext".into());
    }
    AutoCommit::load(
        &view
            .peer()
            .document("so.tilo.chat", "")
            .ok_or("Agent Message document is absent")?,
    )
    .map_err(|_| "Agent Message canonical document cannot be decoded".into())
}

fn matching_rows(doc: &AutoCommit, source: &MessageSource) -> Result<Vec<(String, ObjId)>, String> {
    if doc
        .get_all(ROOT, "messages")
        .map_err(|_| "Agent Message table cannot be read")?
        .is_empty()
    {
        return Ok(Vec::new());
    }
    let messages = object(doc, &ROOT, "messages", ObjType::Map)?;
    let mut rows = Vec::new();
    for channel in doc.keys(&messages) {
        let list = object(doc, &messages, &channel, ObjType::List)?;
        for index in 0..doc.length(&list) {
            let entries = doc
                .get_all(&list, index)
                .map_err(|_| "Agent Message row cannot be read")?;
            let [(Value::Object(ObjType::Map), row)] = entries.as_slice() else {
                return Err("Agent Message row is malformed or conflicted".into());
            };
            if scalar(doc, row, "id")?.as_str() == Some(source.request_row.as_str()) {
                rows.push((channel.clone(), row.clone()));
            }
        }
    }
    Ok(rows)
}

pub(crate) fn verify_row(
    view: &NativeCommittedView,
    source: &MessageSource,
    now: SystemTime,
) -> Result<(), String> {
    let doc = document(view)?;
    for hash in &source.changes {
        let change = ReadDoc::get_change_by_hash(&doc, &automerge::ChangeHash(*hash))
            .ok_or("Agent Message contribution is absent from admitted document")?;
        if change.actor_id().to_bytes() != source.executed_actor {
            return Err("Agent Message contribution includes another action actor".into());
        }
        let context = change
            .message()
            .and_then(tilo_actions::ActionContext::from_change_message);
        if context.as_ref().and_then(|context| context.executed_action)
            != Some(source.executed_action)
        {
            return Err("Agent Message contribution belongs to another action or lacks executor correlation".into());
        }
        if context
            .as_ref()
            .and_then(|context| context.source_invocation)
            != Some(tilo_actions::InvocationId(source.invocation))
        {
            return Err("Agent Message contribution has another original invocation or lacks admission correlation".into());
        }
    }
    let channels = object(&doc, &ROOT, "channels", ObjType::Map)?;
    let channel = object(&doc, &channels, &source.channel, ObjType::Map)?;
    equal_text(&doc, &channel, "workspace", &source.room.to_string())?;
    equal_text(&doc, &channel, "conversation_with", &source.agent)?;
    equal_text(&doc, &channel, "kind", "text")?;
    let rows = matching_rows(&doc, source)?;
    let [(actual_channel, row)] = rows.as_slice() else {
        return Err("Agent Message request row is absent or duplicated".into());
    };
    if actual_channel != &source.channel {
        return Err("Agent Message request row belongs to another channel".into());
    }
    verify_operation(&doc, row, source)?;
    for key in [
        "id",
        "author",
        "parent",
        "agent_intent",
        "ts",
        "edited",
        "text",
    ] {
        let fields = doc
            .get_all(row, key)
            .map_err(|_| "Agent Message row cannot be read")?;
        let [(_, operation)] = fields.as_slice() else {
            return Err("Agent Message identity is missing or conflicted".into());
        };
        verify_operation(&doc, operation, source)?;
    }
    let (_, _, person) = view
        .peer()
        .permissions(source.caller.device_public_key, now)?;
    equal_text(&doc, row, "author", &person)?;
    equal_text(&doc, row, "parent", "")?;
    equal_text(&doc, row, "agent_intent", "message")?;
    if scalar(&doc, row, "ts")?.as_i64() != Some(source.payload.ts)
        || scalar(&doc, row, "edited")?.as_bool() != Some(false)
    {
        return Err("Agent Message row identity differs from typed source".into());
    }
    let text = object(&doc, row, "text", ObjType::Text)?;
    for index in 0..doc.length(&text) {
        let characters = doc
            .get_all(&text, index)
            .map_err(|_| "Agent Message character cannot be read")?;
        let [(_, operation)] = characters.as_slice() else {
            return Err("Agent Message text is incomplete or conflicted".into());
        };
        verify_operation(&doc, operation, source)?;
    }
    if doc
        .text(&text)
        .map_err(|_| "Agent Message text cannot be read")?
        != source.payload.text
    {
        return Err("Agent Message committed text differs from typed source".into());
    }
    // Rich/attachment/reference extras remain inert original typed bytes. They
    // do not grant this Agent permission to read attachments or other rows.
    Ok(())
}

fn verify_operation(
    doc: &AutoCommit,
    operation: &ObjId,
    source: &MessageSource,
) -> Result<(), String> {
    let ObjId::Id(counter, actor, _) = operation else {
        return Err("Agent Message fact has no original operation".into());
    };
    if actor.to_bytes() != source.executed_actor {
        return Err("Agent Message fact was created by another action contributor".into());
    }
    for hash in &source.changes {
        let change = ReadDoc::get_change_by_hash(doc, &automerge::ChangeHash(*hash))
            .ok_or("Agent Message action contribution is absent from actual document")?;
        if change.actor_id() != actor {
            return Err("Agent Message contribution range contains another action actor".into());
        }
        if *counter >= change.start_op().get() && *counter <= change.max_op() {
            return Ok(());
        }
    }
    Err("Agent Message winning operation is outside its typed contribution range".into())
}

fn validated_sources(
    view: &NativeCommittedView,
    request: &OwnerCommitRequest,
    attestations: &[AgentMessageAdmissionAttestation],
    now: SystemTime,
) -> Result<Vec<MessageSource>, String> {
    if attestations.is_empty() || attestations.len() > agent_message::MAX_PENDING {
        return Err("Agent Message admission batch is empty or oversized".into());
    }
    // The SDK's retained-action budget is aggregate, not per contributor.
    // Recheck it before decoding/cloning any signed batch into private input.
    if attestations
        .iter()
        .try_fold(0usize, |sum, attestation| {
            sum.checked_add(attestation.data.action.len())
                .and_then(|bytes| bytes.checked_add(attestation.data.executed_actor.len()))
        })
        .is_none_or(|bytes| bytes > collab_types::MAX_DOCUMENT_TRANSFER_BYTES)
    {
        return Err("Agent Message typed admission batch exceeds document byte budget".into());
    }
    let mut ids = BTreeSet::new();
    let mut sources = Vec::new();
    for attestation in attestations {
        let source = attestation.verify(view, request, now)?;
        if !ids.insert(source.invocation) {
            return Err("duplicate Agent Message source invocation in admission batch".into());
        }
        sources.push(source);
    }
    Ok(sources)
}

impl NativeWorkspaceOwner {
    /// Publish canonical Chat requests and their private addressed-input facts
    /// together. Attestations are inert signed data minted only by the private
    /// host broker after validating actual original invocation/contribution
    /// custody. This API cannot launch an Agent or grant document permission.
    ///
    /// # Errors
    /// Refuses stale/signature/source/row/membership mismatches, duplicates, or
    /// failed durable publication without installing a partial candidate.
    pub fn commit_agent_messages(
        &self,
        request: OwnerCommitRequest,
        attestations: &[AgentMessageAdmissionAttestation],
        publisher: &crate::AgentMessagePublisherLease,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let outgoing = publication::prepare(&request)?;
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid Agent Message room")?;
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let sources = validated_sources(&candidate, &request, attestations, now)?;
        for source in &sources {
            publisher.check_identity(self, room, &source.publisher_digest)?;
        }
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = request.operation
        else {
            return Err("Agent Message admission requires its exact Chat document envelope".into());
        };
        if app != "so.tilo.chat"
            || !record.is_empty()
            || envelope.author.as_bytes() != &candidate.device().device_public_key
        {
            return Err("Agent Message publication lacks canonical local signer custody".into());
        }
        let exact = candidate
            .peer
            .lookup_exact_local_document(&app, &record, &envelope, now)
            .map_err(|error| error.to_string())?
            .is_some();
        if exact {
            for source in &sources {
                if let Some(pending) = candidate.record.agent_messages.get(&source.invocation) {
                    pending.check_replay(source)?;
                } else {
                    agent_message_history::lookup(
                        self.0.store.as_ref(),
                        candidate.record.agent_message_history_root,
                        source.invocation,
                    )?
                    .ok_or("exact Chat publication has no retained Agent Message admission")?
                    .check_replay(source)?;
                }
            }
            if !publication::has_exact(&candidate, &outgoing)?
                && !publication::retry_covered(&candidate, &outgoing)?
            {
                return Err("Agent Message retry lacks retained publication custody".into());
            }
            return Ok(OwnerCommitReceipt {
                binding,
                generation: candidate.generation(),
                aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
            });
        }
        // A reused UUID or pre-existing row must not be adopted as a new
        // invocation, even when another valid signed envelope repeats it.
        for source in &sources {
            if candidate
                .record
                .agent_messages
                .contains_key(&source.invocation)
                || agent_message_history::lookup(
                    self.0.store.as_ref(),
                    candidate.record.agent_message_history_root,
                    source.invocation,
                )?
                .is_some()
            {
                return Err(
                    "Agent Message source UUID was already admitted in another publication".into(),
                );
            }
            if candidate.peer().document("so.tilo.chat", "").is_some() {
                let before = document(&candidate)?;
                if !matching_rows(&before, source)?.is_empty() {
                    return Err(
                        "Agent Message source row already existed before this publication".into(),
                    );
                }
            }
        }
        publication::reserve(&candidate, &outgoing)?;
        candidate
            .peer
            .admit_new_local_document(&app, &record, *envelope, now)?;
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("Agent Message owner generation exhausted")?;
        publication::append_local(&mut candidate, outgoing)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = generation;
        for source in sources {
            self.retain_message_source(&mut candidate, source, now)?;
        }
        let receipt = OwnerCommitReceipt {
            binding,
            generation,
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        // Exact replay above observes its immutable receipt without admitting
        // another input. Only new publication reserves current live custody.
        let _reservation = publisher.reserve()?;
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }

    fn retain_message_source(
        &self,
        candidate: &mut NativeCommittedView,
        source: MessageSource,
        now: SystemTime,
    ) -> Result<(), String> {
        let room = candidate.record.room;
        let generation = candidate.generation();
        verify_row(candidate, &source, now)?;
        let (_, caps, _) = candidate
            .peer()
            .permissions(source.caller.device_public_key, now)?;
        let mut workspace = tilo_desktop_model::Workspace::new(
            tilo_desktop_model::WorkspaceId(candidate.workspace()),
            "Agent Message admission",
        );
        workspace.room = room;
        workspace.capabilities = caps;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "Agent Message clock precedes Unix epoch")?
            .as_secs();
        if workspace
            .role_of_agent_in_room_at(&source.agent, &room.to_string(), seconds)
            .is_none_or(|role| role == collab_types::Role::Denied)
        {
            return Err("Agent Message target is not a live exact-room member".into());
        }
        let incarnation = agent_message::membership_incarnation(candidate.peer(), &source.agent)?;
        let terminal = TerminalMessage {
            invocation: source.invocation,
            source_digest: source.digest()?,
            outcome: MessageOutcome::DeliveryUnavailable,
            generation,
        };
        let mut retained = false;
        if candidate.record.agent_messages.len() < agent_message::MAX_PENDING {
            candidate.record.agent_messages.insert(
                source.invocation,
                StoredAgentMessage {
                    source,
                    execution: None,
                    membership_incarnation: incarnation,
                    phase: MessagePhase::AwaitingAgent,
                    admitted_generation: generation,
                },
            );
            // Count the actual owner representation (raw typed bytes,
            // decoded inert extras, document/outbox and recovery reserve)
            // against the existing whole-owner cap. No smaller per-message
            // limit silently rejects legitimate rich Chat payloads.
            let mut next = self.0.authority.read().clone();
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or("native storage generation exhausted")?;
            next.rooms.insert(room, candidate.record.clone());
            match publication::preflight_storage(&next) {
                Ok(()) => retained = true,
                Err(error) if error.contains("native encoded state refusal cap exceeded") => {
                    candidate.record.agent_messages.remove(&terminal.invocation);
                }
                Err(error) => return Err(error),
            }
        }
        if !retained {
            candidate.record.agent_message_history_root = Some(agent_message_history::insert(
                self.0.store.as_ref(),
                candidate.record.agent_message_history_root,
                terminal,
            )?);
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_message_commit_tests.rs"]
pub(crate) mod tests;
