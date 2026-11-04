//! What only a device hosting a workspace can say about a person's old
//! private conversations: the store their history is imported from once and
//! then emptied.
//!
//! A conversation is an ordinary record of the workspace, which the app reads
//! and writes itself; nothing of it passes this door. What does is decided
//! here: the caller is the certificate the broker attested on the app's
//! connection, never something an app said about itself, and the owner refuses
//! one that is not a current device of a person here, and every call on a
//! workspace this device only enrolled in rather than hosts.

use std::sync::Arc;
use std::time::SystemTime;

use tilo_action_broker::services::person_chat::{PersonChatCommand, PersonChatServices};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::caller_authority::CallerAuthority;
use tilo_peer_state::owner::{NativeWorkspaceOwner, OldConversation, OldConversationPage};
use tilo_user_identity::DeviceCert;

use crate::engine::Engine;

/// How large a Chat request may be: the broker's door admits the same bound,
/// and this is where the command is parsed.
const MAX_REQUEST_BYTES: usize = 24 * 1024;
/// How large an answer may grow: what the broker copies onto the channel.
const MAX_REPLY_BYTES: usize = 1024 * 1024;
/// The messages one page of a conversation carries.
const PAGE: usize = 100;

/// The workspaces' own owner, as Chat reaches the person's private
/// conversations through it.
pub(crate) struct PersonChat {
    engine: Arc<Engine>,
}

impl PersonChat {
    pub(crate) fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }

    /// The owner every call is decided by, and the workspace as the owner
    /// names it.
    fn owner(&self, workspace: &str) -> Result<(WorkspaceId, &NativeWorkspaceOwner), String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("invalid private conversation room")?;
        Ok((workspace, self.engine.owner()))
    }
}

impl PersonChatServices for PersonChat {
    fn command(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        command: &[u8],
    ) -> Result<Vec<u8>, String> {
        if command.len() > MAX_REQUEST_BYTES {
            return Err("private conversation request too large".into());
        }
        let command: PersonChatCommand =
            serde_json::from_slice(command).map_err(|_| "invalid private conversation command")?;
        let (workspace, owner) = self.owner(workspace)?;
        let authority = CallerAuthority::Person {
            caller: caller.clone(),
        };
        let now = SystemTime::now();
        let value = match command {
            // The old store's conversations are in their records: one
            // publication empties it, and nothing writes there again.
            PersonChatCommand::Clear => {
                owner.clear_old_direct_conversations(workspace, &authority, now)?;
                serde_json::json!({})
            }
            // One page of a conversation still in the old private store, so
            // the pair's record can take it over. Reading only: an empty
            // answer means this person holds no such conversation.
            PersonChatCommand::Import {
                public_key,
                after_sequence,
            } => {
                let page = OldConversationPage {
                    after_sequence,
                    limit: PAGE,
                };
                // The door names people as the workspace does; the old store
                // that kept their conversations spells them by their hash.
                let stored = tilo_peer_state::owner::person_store_key(peer_key(&public_key)?);
                let read = owner.old_direct_conversation(workspace, &stored, &authority, page, now);
                match read {
                    Ok(Some(value)) => import_page(&public_key, value),
                    Ok(None) => serde_json::json!({
                        "peer": public_key,
                        "messages": [],
                        "has_more": false,
                        "next_sequence": after_sequence,
                    }),
                    Err(error) => return Err(error),
                }
            }
        };
        let mut reply = BoundedReply(Vec::new());
        serde_json::to_writer(&mut reply, &value)
            .map_err(|_| "private conversation reply encoding failed or exceeds bound")?;
        Ok(reply.0)
    }
}

/// A reply that refuses to grow past the bound the broker carries, including
/// what JSON escaping adds, before it allocates it.
struct BoundedReply(Vec<u8>);

impl std::io::Write for BoundedReply {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_REPLY_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "private conversation reply exceeds bound",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The 32 bytes of a person's root public key, as the person door spells it.
fn peer_key(public_key: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(public_key).map_err(|_| "invalid conversation peer")?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "invalid conversation peer".into())
}

/// How much text one import page carries, so a page of long messages stays
/// well inside the reply bound once JSON escapes it.
const IMPORT_TEXT_BYTES: usize = 128 * 1024;

/// One import page as Chat reads it: the stored messages it carries, and
/// where the next page starts.
fn import_page(peer: &str, value: OldConversation) -> serde_json::Value {
    let mut carried = 0usize;
    let mut next_sequence = value.next_sequence;
    let mut has_more = value.has_more;
    let mut messages = Vec::new();
    for message in value.messages {
        let grown = carried.saturating_add(message.text.len());
        if !messages.is_empty() && grown > IMPORT_TEXT_BYTES {
            // Stop at the last message that fits; the rest is the next page.
            has_more = true;
            break;
        }
        carried = grown;
        next_sequence = message.sequence;
        messages.push(serde_json::json!({
            "id": message.id,
            "author": message.sender,
            "text": message.text,
            "at": message.received_at,
        }));
    }
    serde_json::json!({
        "peer": peer,
        "messages": messages,
        "has_more": has_more,
        "next_sequence": next_sequence,
    })
}
