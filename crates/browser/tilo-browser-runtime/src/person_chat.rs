//! The page's door to what only the workspace can say about a person's direct
//! conversations: which pairs they are in, the announcement that makes a
//! pair's record theirs, and the old private store their history is imported
//! from once. A conversation's body is its pair's record, which the app reads
//! and writes through the page's replica like any other record; the page's
//! pairs come from that replica, and the announcement goes to the host, which
//! records the proof the page's own device signed. Nothing waits on the page
//! thread: each answer comes back through the toolkit's callbacks.
//!
//! The workspace's private conversations ([`ask_private`]) are the page's
//! remaining read of the old store and the door the page shell's people
//! overlay opens a conversation through; they are read-only here, and go with
//! the store.

use tilo_browser_peer::PeerHandle;
use tilo_relay_protocol::workspaces::private::{
    PrivateCall, PrivateOperation, PrivateReply, read_whole,
};
use ui::person_chat::{
    Answer, AnswerDone, Contact, Conversation, ImportPage, Message, Pair, Request, StoredMessage,
    WatchDone,
};

/// Most contacts a workspace's answer may carry.
const MAX_CONTACTS: usize = 2000;
/// The messages one import page of the old private store carries: the bound
/// the workspace's private door serves a page in.
const PAGE: u16 = 100;

/// The workspace `workspace` this page's peer `handle` is in, replicating as
/// page `page`, whose person is `me` (their root public key, hex), as its door
/// to their direct conversations.
#[derive(Clone)]
struct PagePrivate {
    handle: PeerHandle,
    workspace: String,
    page: u32,
    me: String,
}

/// The page's door while it is in a workspace: what [`ask_private`] asks.
static DOOR: std::sync::RwLock<Option<PagePrivate>> = std::sync::RwLock::new(None);

/// Make the workspace `workspace`, through `handle`, the page's door to its
/// person `me`'s direct conversations.
pub(crate) fn install(handle: PeerHandle, workspace: &str, page: u32, me: &str) {
    let door = PagePrivate {
        handle,
        workspace: workspace.to_owned(),
        page,
        me: me.to_owned(),
    };
    set_door(Some(door.clone()));
    ui::person_chat::install_transport(Some(std::sync::Arc::new(door)));
}

/// The page left its workspace: no door, and nothing kept from it.
pub(crate) fn uninstall() {
    set_door(None);
    ui::person_chat::install_transport(None);
}

fn set_door(door: Option<PagePrivate>) {
    use tilo_lock::RwLockRecover as _;
    *DOOR.write_recover() = door;
}

/// What the workspace answered one private operation.
pub type PrivateAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<PrivateReply, String>> + Send>>;

/// Ask the workspace the page is in for one private operation, as the device
/// its session proved: the door a service the bundle lends an app
/// (Messages' Tilo messages) carries its conversations through. Fails
/// while the page is in no workspace.
pub fn ask_private(operation: PrivateOperation) -> PrivateAnswer {
    use tilo_lock::RwLockRecover as _;
    let door = DOOR.read_recover().clone();
    Box::pin(async move {
        let door = door.ok_or("the page is in no workspace")?;
        let answer = door.exchange(operation).await;
        // What came of it shows in the next frame of every desktop.
        crate::browser::wake_desktops();
        answer
    })
}

impl PagePrivate {
    /// One operation, answered as the workspace answered it.
    async fn exchange(&self, operation: PrivateOperation) -> Result<PrivateReply, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.handle.private_then(
            PrivateCall {
                workspace: self.workspace.clone(),
                operation,
            },
            Box::new(move |answer| {
                let _ = reply.send(answer);
            }),
        )?;
        answer.await.map_err(|_| "peer session closed")?
    }

    /// One operation; a refusal is an error that says nothing of why.
    async fn ask(&self, operation: PrivateOperation) -> Result<PrivateReply, String> {
        match self.exchange(operation).await? {
            PrivateReply::Refused => Err("the workspace refused the private operation".into()),
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

    /// The pairs this page's person is in, as the page's replica holds the
    /// workspace's proofs: only their own, so who else talks with whom stays
    /// the workspace's.
    async fn pairs(&self) -> Result<Answer, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        crate::replication::direct_pairs(
            self.page,
            Box::new(move |pairs| {
                let _ = reply.send(pairs);
            }),
        )?;
        let pairs = answer.await.map_err(|_| "peer session closed")??;
        let mut pairs: Vec<Pair> = pairs
            .into_iter()
            .filter(|(_, people)| people.contains(&self.me))
            .map(|(pair, people)| Pair { pair, people })
            .collect();
        pairs.sort_by(|left, right| left.pair.cmp(&right.pair));
        Ok(Answer::Pairs(pairs))
    }

    /// One page of the conversation with `peer` as the old private store keeps
    /// it: read only, so the migration takes it over and nothing is written
    /// back there.
    async fn import(&self, peer: &str, after_sequence: u64) -> Result<Answer, String> {
        let page = match self
            .ask(PrivateOperation::Read {
                peer: peer.to_owned(),
                after_sequence,
                limit: PAGE,
            })
            .await?
        {
            PrivateReply::Conversation(page) => page,
            _ => return Err("unexpected private read reply".into()),
        };
        if page.peer != peer {
            return Err("private conversation peer binding changed".into());
        }
        Ok(Answer::ImportPage(ImportPage {
            peer: peer.to_owned(),
            // The workspace's store names a person by their root key; the
            // conversation's record, which takes the message in, names them by
            // the key their conversations keep them under.
            messages: page
                .messages
                .into_iter()
                .map(|message| StoredMessage {
                    id: message.id,
                    author: ui::person_chat::person_key(&message.sender),
                    text: message.text,
                    at: message.received_at.saturating_mul(1000),
                })
                .collect(),
            has_more: page.has_more,
            next_sequence: page.next_sequence,
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
            // The page signs its pair where its key is kept, and the host
            // records it in the workspace's governance.
            Request::Announce { public_key } => {
                let other = peer_key(&public_key)?;
                let (reply, answer) = tokio::sync::oneshot::channel();
                crate::replication::announce_pair(
                    self.page,
                    other,
                    Box::new(move |announced| {
                        let _ = reply.send(announced);
                    }),
                )?;
                answer.await.map_err(|_| "peer session closed")??;
                Ok(Answer::Announced)
            }
            Request::Pairs => self.pairs().await,
            Request::Import {
                public_key,
                after_sequence,
            } => {
                peer_key(&public_key)?;
                self.import(&public_key, after_sequence).await
            }
            // A page holds no old private store of its own: the store its
            // conversations are imported from is the host's, and stays there
            // read only until it goes.
            Request::Clear => Ok(Answer::Cleared),
        }
    }
}

fn peer_key(peer: &str) -> Result<[u8; 32], String> {
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
