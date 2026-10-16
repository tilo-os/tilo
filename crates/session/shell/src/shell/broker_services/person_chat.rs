//! The native private conversation bridge has no document-backed fallback.
use super::NativeOwnerCommitService;
#[path = "person_chat_remote.rs"]
mod remote;
#[cfg(test)]
#[path = "person_chat_tests.rs"]
mod tests;
use tilo_action_broker::services::person_chat::{PersonChatCommand, PersonChatServices};
use tilo_joined_room::{DirectConversation, DirectConversationPage, DirectMessageSend};
use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;

impl PersonChatServices for NativeOwnerCommitService {
    fn subscribe(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        peer: &str,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        let room =
            tilo_desktop_model::RoomId::parse(room).ok_or("invalid private conversation room")?;
        let owner = self.opened_owner("private conversation owner unavailable")?;
        let route = self
            .network
            .as_ref()
            .map(|network| network.private_chat_route(room, caller))
            .transpose()?
            .flatten();
        if let Some(route) = route {
            return remote::subscribe(route, peer);
        }
        drop(route);
        check_local_governor(&owner, room, caller)?;
        owner.subscribe_direct_conversation(
            room,
            peer,
            &WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            std::time::SystemTime::now(),
        )
    }

    fn command(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        command: &[u8],
    ) -> Result<Vec<u8>, String> {
        if command.len() > 24 * 1024 {
            return Err("private conversation request too large".into());
        }
        let command: PersonChatCommand =
            serde_json::from_slice(command).map_err(|_| "invalid private conversation command")?;
        let room =
            tilo_desktop_model::RoomId::parse(room).ok_or("invalid private conversation room")?;
        let owner = self.opened_owner("private conversation owner unavailable")?;
        let route = self
            .network
            .as_ref()
            .map(|network| network.private_chat_route(room, caller))
            .transpose()?
            .flatten();
        if let Some(route) = route {
            return remote::command(route, command);
        }
        drop(route);
        check_local_governor(&owner, room, caller)?;
        let authority = WorkspaceTaskAuthority::Person {
            caller: caller.clone(),
        };
        let now = std::time::SystemTime::now();
        let value = match command {
            PersonChatCommand::Contacts => serde_json::json!(owner.direct_contacts(room, &authority, now)?
                .into_iter().map(|contact| serde_json::json!({
                    "public_key": contact.public_key, "display_name": contact.display_name.unwrap_or_default()
                })).collect::<Vec<_>>()),
            PersonChatCommand::Open { public_key } => {
                owner.open_direct_conversation(room, &public_key, &authority, now)?;
                conversation(read_all(&owner, room, &public_key, &authority, now)?)
            }
            PersonChatCommand::Read { public_key } => conversation(read_all(&owner, room, &public_key, &authority, now)?),
            PersonChatCommand::Send { public_key, text, invocation } => {
                owner.send_direct_message(room, &public_key, &authority, DirectMessageSend { invocation, text }, now)?;
                conversation(read_all(&owner, room, &public_key, &authority, now)?)
            }
        };
        let mut reply = BoundedReply(Vec::new());
        serde_json::to_writer(&mut reply, &value)
            .map_err(|_| "private conversation reply encoding failed or exceeds bound")?;
        Ok(reply.0)
    }
}

fn check_local_governor(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    room: tilo_desktop_model::RoomId,
    caller: &tilo_user_identity::DeviceCert,
) -> Result<(), String> {
    owner.with_current(room, |view| {
        if caller != view.device() || view.governor() != view.device() {
            return Err(
                "private conversations require the actual governor or authenticated RPC".into(),
            );
        }
        Ok(())
    })
}

// Reject expansion (including JSON escaping) before allocating beyond the wire bound.
struct BoundedReply(Vec<u8>);
impl std::io::Write for BoundedReply {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > (1024 * 1024usize).saturating_sub(self.0.len()) {
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

fn check_snapshot_bound(messages: usize, bytes: usize) -> Result<(), String> {
    if bytes > 512 * 1024 || messages > 2000 {
        return Err("private conversation snapshot exceeds bound".into());
    }
    Ok(())
}

fn read_all(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    room: tilo_desktop_model::RoomId,
    peer: &str,
    authority: &WorkspaceTaskAuthority,
    now: std::time::SystemTime,
) -> Result<DirectConversation, String> {
    let mut result = owner.read_direct_conversation(
        room,
        peer,
        authority,
        DirectConversationPage {
            after_sequence: 0,
            limit: 100,
        },
        now,
    )?;
    let mut bytes = result
        .messages
        .iter()
        .try_fold(0usize, |bytes, message| {
            bytes.checked_add(message.text.len())
        })
        .ok_or("private conversation snapshot exceeds bound")?;
    check_snapshot_bound(result.messages.len(), bytes)?;
    while result.has_more {
        let next = owner.read_direct_conversation(
            room,
            peer,
            authority,
            DirectConversationPage {
                after_sequence: result.next_sequence,
                limit: 100,
            },
            now,
        )?;
        if next.id != result.id || next.next_sequence <= result.next_sequence {
            return Err("private conversation page binding changed".into());
        }
        bytes = next
            .messages
            .iter()
            .try_fold(bytes, |bytes, message| {
                bytes.checked_add(message.text.len())
            })
            .ok_or("private conversation snapshot exceeds bound")?;
        let messages = result
            .messages
            .len()
            .checked_add(next.messages.len())
            .ok_or("private conversation snapshot exceeds bound")?;
        check_snapshot_bound(messages, bytes)?;
        result.next_sequence = next.next_sequence;
        result.has_more = next.has_more;
        result.messages.extend(next.messages);
    }
    Ok(result)
}

fn conversation(value: DirectConversation) -> serde_json::Value {
    serde_json::json!({"peer":value.peer,"messages":value.messages.into_iter().map(|message|
        serde_json::json!({"id":message.id,"author":message.sender,"text":message.text})
    ).collect::<Vec<_>>()})
}
