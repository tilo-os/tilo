//! The page's door to its room's private person conversations: the
//! toolkit's `ui::person_chat` asks the room's governor over this page's
//! session (`tilo_relay_protocol::rooms::private`), the same door and
//! governor code a native device reaches over iroh, authenticated by the
//! device certificate the session proved. Nothing waits on the page thread:
//! each answer comes back through the toolkit's callbacks.

use tilo_browser_peer::PeerHandle;
use tilo_relay_protocol::rooms::private::{
    PrivateCall, PrivateOperation, PrivateReply, read_whole,
};
use ui::person_chat::{Answer, AnswerDone, Contact, Conversation, Message, Request, WatchDone};

/// Most contacts a room's answer may carry.
const MAX_CONTACTS: usize = 2000;

/// The room `room` this page's peer `handle` is in, as its private door.
#[derive(Clone)]
struct PagePrivate {
    handle: PeerHandle,
    room: String,
}

/// Make the room `room`, through `handle`, the page's door to private
/// conversations.
pub(crate) fn install(handle: PeerHandle, room: &str) {
    ui::person_chat::install_transport(Some(std::sync::Arc::new(PagePrivate {
        handle,
        room: room.to_owned(),
    })));
}

/// The page left its room: no door, and nothing kept from it.
pub(crate) fn uninstall() {
    ui::person_chat::install_transport(None);
}

impl PagePrivate {
    /// One operation; a refusal is an error that says nothing of why.
    async fn ask(&self, operation: PrivateOperation) -> Result<PrivateReply, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.handle.private_then(
            PrivateCall {
                room: self.room.clone(),
                operation,
            },
            Box::new(move |answer| {
                let _ = reply.send(answer);
            }),
        )?;
        match answer.await.map_err(|_| "peer session closed")?? {
            PrivateReply::Refused => Err("the room refused the private operation".into()),
            reply => Ok(reply),
        }
    }

    async fn whole(&self, peer: &str) -> Result<Answer, String> {
        let conversation = read_whole(|operation| self.ask(operation), peer).await?;
        Ok(Answer::Conversation(Conversation {
            peer: conversation.peer,
            messages: conversation
                .messages
                .into_iter()
                .map(|message| Message {
                    id: message.id,
                    author: message.sender,
                    text: message.text,
                })
                .collect(),
        }))
    }

    async fn answer(&self, request: Request) -> Result<Answer, String> {
        match request {
            Request::Contacts => {
                let PrivateReply::Contacts(contacts) = self.ask(PrivateOperation::Contacts).await?
                else {
                    return Err("unexpected private contacts reply".into());
                };
                if contacts.len() > MAX_CONTACTS {
                    return Err("private contacts exceed bound".into());
                }
                Ok(Answer::Contacts(
                    contacts
                        .into_iter()
                        .map(|contact| Contact {
                            public_key: contact.public_key,
                            display_name: contact.display_name.unwrap_or_default(),
                            conversation: contact.conversation,
                        })
                        .collect(),
                ))
            }
            Request::Open { public_key } => {
                peer_key(&public_key)?;
                let PrivateReply::Conversation(opened) = self
                    .ask(PrivateOperation::Open {
                        peer: public_key.clone(),
                    })
                    .await?
                else {
                    return Err("unexpected private open reply".into());
                };
                if opened.peer != public_key {
                    return Err("private open peer changed".into());
                }
                self.whole(&public_key).await
            }
            Request::Read { public_key } => {
                peer_key(&public_key)?;
                self.whole(&public_key).await
            }
            Request::Send {
                public_key,
                text,
                invocation,
            } => {
                peer_key(&public_key)?;
                let sent = self
                    .ask(PrivateOperation::Send {
                        peer: public_key.clone(),
                        invocation,
                        text,
                    })
                    .await?;
                if !matches!(sent, PrivateReply::Sent) {
                    return Err("unexpected private send reply".into());
                }
                self.whole(&public_key).await
            }
        }
    }
}

fn peer_key(peer: &str) -> Result<(), String> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(peer, &mut bytes).map_err(|_| "invalid private peer key".into())
}

impl ui::person_chat::Transport for PagePrivate {
    fn call(&self, request: Request, done: AnswerDone) {
        let door = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            done(door.answer(request).await);
            // What came in shows in the next frame of every desktop.
            crate::browser::wake_desktops();
        });
    }

    fn watch(&self, peer: String, after: u64, done: WatchDone) {
        let door = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let changed = door
                .ask(PrivateOperation::Watch {
                    peer,
                    after_sequence: after,
                })
                .await
                .and_then(|reply| match reply {
                    PrivateReply::Changed(sequence) => Ok(sequence),
                    _ => Err("unexpected private watch reply".into()),
                });
            done(changed);
            crate::browser::wake_desktops();
        });
    }
}
