//! Receipt-scoped reply publication. The guest supplies no document, channel,
//! author, parent or signing key: the installed host constructs the only edit.
use super::{AgentMessageHostService, source_attention};
use crate::{
    NativeCommittedView,
    agent_message::{MessageOutcome, MessagePhase, StoredAgentMessage, TerminalMessage},
    agent_message_history, publication,
};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_peer_state::owner_commit::CommitBinding;

/// Historical acceptance of one canonical reply, not live Agent authority.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentMessageReplyReceipt {
    pub source: uuid::Uuid,
    pub reply: uuid::Uuid,
    pub document: CommitBinding,
    pub generation: u64,
}

impl AgentMessageHostService {
    /// Publish the already prepared output with the actual installed host key.
    /// The key never crosses the Agent channel. Exact replays observe the same
    /// durable reply; they neither append another row nor regenerate output.
    ///
    /// # Errors
    /// Refuses changed source, live process or signing custody, an unprepared
    /// reply, malformed canonical Chat, and failed durable publication.
    pub fn commit_reply(
        &self,
        invocation: uuid::Uuid,
        source_digest: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<AgentMessageReplyReceipt, String> {
        let _turn = self.owner.0.mutation.lock();
        let mut candidate = self
            .owner
            .with_current(self.session.room, |view| Ok(view.clone()))?;
        let execution = self.execution(&candidate, now)?;
        if candidate.device() != &self.attestor
            || candidate.host().unwrap_or_else(|| candidate.governor()) != &self.attestor
            || key.verifying_key().to_bytes() != self.attestor.device_public_key
        {
            return Err("Agent Message reply lacks current pinned-host signer custody".into());
        }
        if let Some(terminal) = agent_message_history::lookup(
            self.owner.0.store.as_ref(),
            candidate.record.agent_message_history_root,
            invocation,
        )? {
            if terminal.source_digest != source_digest {
                return Err("Agent Message reply replay has another original source".into());
            }
            let MessageOutcome::Replied {
                invocation: reply,
                document,
                execution: original_execution,
            } = terminal.outcome
            else {
                return Err("Agent Message terminated without a canonical reply".into());
            };
            if original_execution.as_ref() != &execution {
                return Err("Agent Message receipt belongs to another executing launch".into());
            }
            return Ok(AgentMessageReplyReceipt {
                source: invocation,
                reply,
                document,
                generation: terminal.generation,
            });
        }
        let mut message = candidate
            .record
            .agent_messages
            .get(&invocation)
            .cloned()
            .ok_or("Agent Message reply has no retained input")?;
        if message.source.agent != self.session.agent || message.source.digest()? != source_digest {
            return Err("Agent Message reply belongs to another addressed input".into());
        }
        if let Some(reason) = source_attention(&candidate, &message, now) {
            if message.needs_attention(reason) {
                self.publish_message(candidate, message)?;
            }
            return Err("Agent Message reply source needs attention".into());
        }
        if message.execution.as_ref() != Some(&execution) {
            return Err("Agent Message reply belongs to another executing launch".into());
        }
        let (reply, document_bytes) = reply_document(&candidate, &message, now)?;
        let outgoing =
            candidate
                .peer
                .publish_document("so.tilo.chat", "", document_bytes, key, now)?;
        let request = publication::request(&outgoing)?;
        let document = request.binding()?;
        let outgoing = publication::prepare(&request)?;
        publication::reserve(&candidate, &outgoing)?;
        publication::append_local(&mut candidate, outgoing)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("Agent Message reply owner generation exhausted")?;
        let generation = candidate.generation();
        candidate.record.agent_message_history_root = Some(agent_message_history::insert(
            self.owner.0.store.as_ref(),
            candidate.record.agent_message_history_root,
            TerminalMessage {
                invocation,
                source_digest,
                outcome: MessageOutcome::Replied {
                    invocation: reply,
                    document: document.clone(),
                    execution: Box::new(execution),
                },
                generation,
            },
        )?);
        candidate.record.agent_messages.remove(&invocation);
        self.owner
            .publish_room(self.session.room, candidate, None)?;
        Ok(AgentMessageReplyReceipt {
            source: invocation,
            reply,
            document,
            generation,
        })
    }
}

fn reply_document(
    view: &NativeCommittedView,
    message: &StoredAgentMessage,
    now: SystemTime,
) -> Result<(uuid::Uuid, Vec<u8>), String> {
    let MessagePhase::ReplyPrepared { invocation, text } = &message.phase else {
        return Err("Agent Message output was not prepared by its accepted execution".into());
    };
    let bytes = view
        .peer()
        .document("so.tilo.chat", "")
        .ok_or("Agent Message canonical Chat is absent")?;
    if view
        .peer()
        .plaintext_complete_heads("so.tilo.chat", "")
        .is_none()
    {
        return Err("Agent Message canonical Chat is not complete plaintext".into());
    }
    let mut doc = AutoCommit::load(&bytes).map_err(|_| "Agent Message Chat cannot be decoded")?;
    let actor = format!("agent:{}", message.source.agent);
    doc.set_actor(automerge::ActorId::from(actor.as_bytes().to_vec()));
    let messages = object(&doc, &ROOT, "messages", ObjType::Map)?;
    let list = object(&doc, &messages, &message.source.channel, ObjType::List)?;
    let row_id = format!("m_{}", invocation.simple());
    refuse_existing_reply(&doc, &messages, &row_id)?;
    let timestamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "Agent Message reply clock precedes Unix epoch")?
        .as_secs();
    let timestamp =
        i64::try_from(timestamp).map_err(|_| "Agent Message reply timestamp overflow")?;
    let row = doc
        .insert_object(&list, doc.length(&list), ObjType::Map)
        .map_err(edit_error)?;
    doc.put(&row, "id", row_id.as_str()).map_err(edit_error)?;
    doc.put(&row, "author", actor.as_str())
        .map_err(edit_error)?;
    doc.put(&row, "parent", message.source.request_row.as_str())
        .map_err(edit_error)?;
    doc.put(&row, "ts", timestamp).map_err(edit_error)?;
    doc.put(&row, "edited", false).map_err(edit_error)?;
    let body = doc
        .put_object(&row, "text", ObjType::Text)
        .map_err(edit_error)?;
    doc.splice_text(&body, 0, 0, text).map_err(edit_error)?;
    doc.put_object(&row, "reactions", ObjType::Map)
        .map_err(edit_error)?;
    doc.put_object(&row, "mentions", ObjType::List)
        .map_err(edit_error)?;
    doc.put_object(&row, "history", ObjType::List)
        .map_err(edit_error)?;
    let context = tilo_actions::ActionContext {
        invoker: Some(tilo_actions::ActionInvoker::actor(actor)),
        source_invocation: Some(tilo_actions::InvocationId(*invocation)),
        executed_action: Some(tilo_actions::ActionId(*invocation)),
        ..Default::default()
    };
    let context = context
        .change_message()
        .ok_or("Agent Message reply context cannot be encoded")?;
    doc.commit_with(automerge::transaction::CommitOptions::default().with_message(context));
    Ok((*invocation, doc.save()))
}

fn refuse_existing_reply(doc: &AutoCommit, messages: &ObjId, row_id: &str) -> Result<(), String> {
    // A pre-existing id cannot be adopted, even with matching display text.
    for channel in doc.keys(messages) {
        let rows = object(doc, messages, &channel, ObjType::List)?;
        for index in 0..doc.length(&rows) {
            let entries = doc
                .get_all(&rows, index)
                .map_err(|_| "Agent Message row cannot be read")?;
            let [(Value::Object(ObjType::Map), row)] = entries.as_slice() else {
                return Err("Agent Message row is malformed or conflicted".into());
            };
            if doc
                .get_all(row, "id")
                .map_err(|_| "Agent Message row identity cannot be read")?
                .iter()
                .any(|(value, _)| value.as_str() == Some(row_id))
            {
                return Err("Agent Message reply id existed before its publication".into());
            }
        }
    }
    Ok(())
}

fn object(doc: &AutoCommit, parent: &ObjId, key: &str, kind: ObjType) -> Result<ObjId, String> {
    match doc
        .get_all(parent, key)
        .map_err(|_| "Agent Message object cannot be read")?
        .as_slice()
    {
        [(Value::Object(actual), id)] if *actual == kind => Ok(id.clone()),
        _ => Err("Agent Message object is missing or conflicted".into()),
    }
}

fn edit_error(_: automerge::AutomergeError) -> String {
    "Agent Message canonical reply edit failed".into()
}
