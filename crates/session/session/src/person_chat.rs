//! A person's private conversations with the other people of their
//! workspaces: what the Chat app reads and writes on the action door.
//!
//! The bodies never enter a workspace's records. The workspaces' owner keeps
//! them beside its own record, in the private person state it alone writes,
//! and decides every call itself: the caller is the certificate the broker
//! attested on the app's connection, never something an app said about
//! itself, and the owner refuses one that is not a current device of a person
//! here, and every call on a workspace this device only enrolled in rather
//! than hosts.
//!
//! Live updates are the owner's own watch (one per conversation, signalled
//! after the message was published durably), never a poll: the broker holds
//! it on the app's channel and answers its next read when it changes.

use std::sync::Arc;
use std::time::SystemTime;

use tilo_action_broker::services::person_chat::{PersonChatCommand, PersonChatServices};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::caller_authority::CallerAuthority;
use tilo_peer_state::owner::{
    DirectConversation, DirectConversationPage, DirectMessageSend, NativeWorkspaceOwner,
};
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
    fn subscribe(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        peer: &str,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        let (workspace, owner) = self.owner(workspace)?;
        owner.subscribe_direct_conversation(
            workspace,
            peer,
            &CallerAuthority::Person {
                caller: caller.clone(),
            },
            SystemTime::now(),
        )
    }

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
            PersonChatCommand::Contacts => serde_json::json!(
                owner
                    .direct_contacts(workspace, &authority, now)?
                    .into_iter()
                    .map(|contact| serde_json::json!({
                        "public_key": contact.public_key,
                        "display_name": contact.display_name.unwrap_or_default(),
                        "conversation": contact.conversation,
                    }))
                    .collect::<Vec<_>>()
            ),
            PersonChatCommand::Open { public_key } => {
                owner.open_direct_conversation(workspace, &public_key, &authority, now)?;
                conversation(whole(owner, workspace, &public_key, &authority, now)?)
            }
            PersonChatCommand::Read { public_key } => {
                conversation(whole(owner, workspace, &public_key, &authority, now)?)
            }
            PersonChatCommand::Send {
                public_key,
                text,
                invocation,
            } => {
                owner.send_direct_message(
                    workspace,
                    &public_key,
                    &authority,
                    DirectMessageSend { invocation, text },
                    now,
                )?;
                conversation(whole(owner, workspace, &public_key, &authority, now)?)
            }
            // The one write a conversation needs of its host: the pair the
            // two people share, so their devices read and change its record.
            // What it holds is the record's own, and stays off this door.
            PersonChatCommand::Announce { public_key } => {
                use tilo_action_broker::owner_commits::OwnerCustody as _;
                let other = peer_key(&public_key)?;
                if !owner
                    .direct_contacts(workspace, &authority, now)?
                    .iter()
                    .any(|contact| contact.public_key == public_key)
                {
                    return Err("a conversation is opened with a person of this workspace".into());
                }
                let key = self.engine.signing_key(owner, workspace)?;
                owner.commit_direct_pair(workspace, caller, &other, &key, now)?;
                serde_json::json!({})
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

/// The whole conversation, page by page, held to the bound the workspace
/// carries a conversation at: Chat shows it whole, and a page is how the
/// owner hands one over.
fn whole(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    peer: &str,
    authority: &CallerAuthority,
    now: SystemTime,
) -> Result<DirectConversation, String> {
    let mut result = owner.read_direct_conversation(
        workspace,
        peer,
        authority,
        DirectConversationPage {
            after_sequence: 0,
            limit: PAGE,
        },
        now,
    )?;
    let mut bytes = text_bytes(&result, 0)?;
    check_bound(result.messages.len(), bytes)?;
    while result.has_more {
        let next = owner.read_direct_conversation(
            workspace,
            peer,
            authority,
            DirectConversationPage {
                after_sequence: result.next_sequence,
                limit: PAGE,
            },
            now,
        )?;
        // The owner's pages are of one conversation, each after the one
        // before: a page of another pair, or one that repeats, is refused
        // instead of concatenated.
        if next.id != result.id || next.next_sequence <= result.next_sequence {
            return Err("private conversation page binding changed".into());
        }
        bytes = text_bytes(&next, bytes)?;
        let messages = result
            .messages
            .len()
            .checked_add(next.messages.len())
            .ok_or("private conversation snapshot exceeds bound")?;
        check_bound(messages, bytes)?;
        result.next_sequence = next.next_sequence;
        result.has_more = next.has_more;
        result.messages.extend(next.messages);
    }
    Ok(result)
}

/// The text of `page`'s messages in bytes, added to what `before` carried.
fn text_bytes(page: &DirectConversation, before: usize) -> Result<usize, String> {
    page.messages
        .iter()
        .try_fold(before, |bytes, message| {
            bytes.checked_add(message.text.len())
        })
        .ok_or_else(|| "private conversation snapshot exceeds bound".into())
}

fn check_bound(messages: usize, bytes: usize) -> Result<(), String> {
    use tilo_relay_protocol::workspaces::private::{MAX_WHOLE_MESSAGES, MAX_WHOLE_TEXT_BYTES};
    if bytes > MAX_WHOLE_TEXT_BYTES || messages > MAX_WHOLE_MESSAGES {
        return Err("private conversation snapshot exceeds bound".into());
    }
    Ok(())
}

/// The conversation as Chat reads it: who it is with, and its messages.
fn conversation(value: DirectConversation) -> serde_json::Value {
    serde_json::json!({
        "peer": value.peer,
        "messages": value.messages.into_iter().map(|message| serde_json::json!({
            "id": message.id,
            "author": message.sender,
            "text": message.text,
        })).collect::<Vec<_>>(),
    })
}
