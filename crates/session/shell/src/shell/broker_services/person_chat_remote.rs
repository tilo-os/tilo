//! Bounded private snapshots and one outstanding change stream per subscription.
use super::BoundedReply;
use crate::shell::network::PrivateChatRoute;
use std::time::Duration;
use tilo_action_broker::services::person_chat::PersonChatCommand;
use tilo_relay_protocol::rooms::private::{PrivateOperation, PrivateReply, read_whole};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
static WATCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(64);
static COMMANDS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);

fn peer_key(peer: &str) -> Result<(), String> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(peer, &mut bytes).map_err(|_| "invalid private peer key".into())
}

pub(super) fn command(
    route: PrivateChatRoute,
    command: PersonChatCommand,
) -> Result<Vec<u8>, String> {
    let permit = COMMANDS
        .try_acquire()
        .map_err(|_| "private command capacity exhausted")?;
    let runtime = route.runtime()?;
    let reply_timeout = COMMAND_TIMEOUT
        .checked_add(Duration::from_secs(1))
        .ok_or("private command timeout exceeds bound")?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let task = runtime.spawn(async move {
        let result = tokio::time::timeout(COMMAND_TIMEOUT, execute(&route, command))
            .await
            .map_err(|_| "private command timed out; outcome unknown".to_string())
            .and_then(|result| result);
        let _ = sender.send(result);
    });
    let result = receiver
        .recv_timeout(reply_timeout)
        .map_err(|_| "private command ended; outcome unknown".to_string());
    task.abort();
    drop(permit);
    result?
}

async fn execute(route: &PrivateChatRoute, command: PersonChatCommand) -> Result<Vec<u8>, String> {
    let value = match command {
        PersonChatCommand::Contacts => {
            let PrivateReply::Contacts(contacts) = route.send(PrivateOperation::Contacts).await?
            else {
                return Err("unexpected private contacts reply".into());
            };
            if contacts.len() > 2000 {
                return Err("private contacts exceed bound".into());
            }
            serde_json::json!(contacts.into_iter().map(|contact| serde_json::json!({
                "public_key": contact.public_key, "display_name": contact.display_name.unwrap_or_default()
            })).collect::<Vec<_>>())
        }
        PersonChatCommand::Open { public_key } => {
            peer_key(&public_key)?;
            let PrivateReply::Conversation(opened) = route
                .send(PrivateOperation::Open {
                    peer: public_key.clone(),
                })
                .await?
            else {
                return Err("unexpected private open reply".into());
            };
            if opened.peer != public_key {
                return Err("private open peer changed".into());
            }
            snapshot(route, &public_key).await?
        }
        PersonChatCommand::Read { public_key } => {
            peer_key(&public_key)?;
            snapshot(route, &public_key).await?
        }
        PersonChatCommand::Send {
            public_key,
            invocation,
            text,
        } => {
            peer_key(&public_key)?;
            if !matches!(
                route
                    .send(PrivateOperation::Send {
                        peer: public_key.clone(),
                        invocation,
                        text
                    })
                    .await?,
                PrivateReply::Sent
            ) {
                return Err("unexpected private send reply".into());
            }
            snapshot(route, &public_key).await?
        }
    };
    let mut reply = BoundedReply(Vec::new());
    serde_json::to_writer(&mut reply, &value)
        .map_err(|_| "private reply encoding failed or exceeds bound")?;
    Ok(reply.0)
}

async fn snapshot(route: &PrivateChatRoute, peer: &str) -> Result<serde_json::Value, String> {
    let result = read_whole(|operation| route.send(operation), peer).await?;
    Ok(
        serde_json::json!({"peer":result.peer,"messages":result.messages.into_iter().map(|message|
        serde_json::json!({"id":message.id,"author":message.sender,"text":message.text})
    ).collect::<Vec<_>>()}),
    )
}

pub(super) fn subscribe(
    route: PrivateChatRoute,
    peer: &str,
) -> Result<tokio::sync::watch::Receiver<u64>, String> {
    peer_key(peer)?;
    let permit = WATCHES
        .try_acquire()
        .map_err(|_| "private watch capacity exhausted")?;
    let runtime = route.runtime()?;
    let peer = peer.to_owned();
    let (sender, receiver) = tokio::sync::watch::channel(0);
    runtime.spawn(async move {
        let _permit = permit;
        let mut after_sequence = 0;
        loop {
            let reply = tokio::select! {
                () = sender.closed() => break,
                reply = route.send(PrivateOperation::Watch { peer: peer.clone(), after_sequence }) => reply,
            };
            let Ok(PrivateReply::Changed(sequence)) = reply else { break; };
            // Never spin on an unchanged cursor, including a refused/expired watch.
            if sequence <= after_sequence { break; }
            after_sequence = sequence;
            if sender.send(sequence).is_err() { break; }
        }
    });
    Ok(receiver)
}
