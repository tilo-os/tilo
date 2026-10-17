//! Governor-custodied person conversations, never room publications or bootstrap data.
//!
//! Call these methods while holding the governed room's lock. Subscribing under
//! that same lock closes the snapshot/subscription race. A notification is only
//! a hint: the transport must reauthorize with `private_command` before replying,
//! including after a membership change or certificate expiry while waiting.
use super::GovernedRoom;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tilo_relay_session::rooms::private::{
    PrivateContact, PrivateConversation, PrivateMessage, PrivateOperation, PrivateReply,
};
use tilo_user_identity::DeviceCert;
use tokio::sync::watch;
use uuid::Uuid;
use web_time::SystemTime;

const MAX_PAIRS: usize = 1024;
const MAX_MESSAGES: usize = 4096;
const MAX_TEXT: usize = 16_384;
const MAX_PAGE: u16 = 100;
const MAX_FILE_BYTES: u64 = 72 * 1024 * 1024;

// Deliberately no Debug on any private custody structures. The wire types also
// need redacted Debug implementations; neither requests nor replies may be logged.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u16,
    room: String,
    pairs: BTreeMap<String, Pair>,
    invocations: BTreeMap<Uuid, Invocation>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    people: [String; 2],
    messages: Vec<Message>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    id: String,
    sender: String,
    sequence: u64,
    received_at: i64,
    text: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    pair: String,
    source: [u8; 32],
    sender: String,
    body_hash: [u8; 32],
    message_id: String,
}

type Channels = BTreeMap<(PathBuf, String), watch::Sender<u64>>;
fn channels() -> &'static Mutex<Channels> {
    static CHANNELS: OnceLock<Mutex<Channels>> = OnceLock::new();
    CHANNELS.get_or_init(Mutex::default)
}
fn valid_key(key: &str) -> Result<(), String> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("person key must be canonical lowercase public-key hex".into());
    }
    Ok(())
}
fn valid_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > MAX_TEXT {
        return Err("private message is empty or exceeds UTF-8 byte bound".into());
    }
    Ok(())
}
// Same domain and tuple encoding as the native owner's person-pair store.
// Authority is nevertheless the governor's StatePeer, not a local owner's view.
fn pair_id(room: &str, people: &[String; 2]) -> Result<String, String> {
    let bytes = serde_json::to_vec(&("private-person-pair-v1", room, people))
        .map_err(|_| "cannot encode private conversation identity")?;
    Ok(format!("direct_{}", blake3::hash(&bytes).to_hex()))
}
fn message_id(pair: &str, invocation: Uuid) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"private-person-message-v1");
    hash.update(pair.as_bytes());
    hash.update(invocation.as_bytes());
    format!("dm_{}", hash.finalize().to_hex())
}

impl Store {
    fn validate(&self, room: &str) -> Result<(), String> {
        if self.version != 1
            || self.room != room
            || self.pairs.len() > MAX_PAIRS
            || self.invocations.len() > MAX_MESSAGES
        {
            return Err("invalid private conversation envelope".into());
        }
        let mut messages = BTreeMap::new();
        for (id, pair) in &self.pairs {
            for person in &pair.people {
                valid_key(person)?;
            }
            if pair.people.first() >= pair.people.get(1) || pair_id(room, &pair.people)? != *id {
                return Err("private conversation identity mismatch".into());
            }
            for (index, message) in pair.messages.iter().enumerate() {
                valid_text(&message.text)?;
                let sequence = u64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(1))
                    .ok_or("private message sequence overflow")?;
                if message.sequence != sequence
                    || message.received_at < 0
                    || !pair.people.contains(&message.sender)
                    || messages
                        .insert(message.id.as_str(), (id, message))
                        .is_some()
                {
                    return Err("invalid private conversation message".into());
                }
            }
            if messages.len() > MAX_MESSAGES {
                return Err("private message capacity exhausted; no messages dropped".into());
            }
        }
        if messages.len() != self.invocations.len() {
            return Err("private invocation count mismatch".into());
        }
        for (id, receipt) in &self.invocations {
            let (pair, message) = messages
                .remove(receipt.message_id.as_str())
                .ok_or("private invocation message absent")?;
            if id.is_nil()
                || receipt.message_id != message_id(&receipt.pair, *id)
                || *pair != receipt.pair
                || receipt.sender != message.sender
                || receipt.body_hash != *blake3::hash(message.text.as_bytes()).as_bytes()
            {
                return Err("private invocation binding mismatch".into());
            }
        }
        Ok(())
    }
    fn ensure_pair(&mut self, id: &str, people: [String; 2]) -> Result<(), String> {
        if !self.pairs.contains_key(id) {
            if self.pairs.len() >= MAX_PAIRS {
                return Err("private conversation capacity exhausted".into());
            }
            self.pairs.insert(
                id.into(),
                Pair {
                    people,
                    messages: Vec::new(),
                },
            );
        }
        Ok(())
    }
    fn count(&self, id: &str) -> Result<u64, String> {
        Ok(self
            .pairs
            .get(id)
            .ok_or("private conversation is not open")?
            .messages
            .len() as u64)
    }
    fn conversation(
        &self,
        id: &str,
        peer: &str,
        after: u64,
        limit: u16,
    ) -> Result<PrivateReply, String> {
        if limit == 0 || limit > MAX_PAGE {
            return Err("private conversation page limit out of bounds".into());
        }
        let count = self.count(id)?;
        if after > count {
            return Err("private conversation cursor out of bounds".into());
        }
        let pair = self
            .pairs
            .get(id)
            .ok_or("private conversation is not open")?;
        let messages: Vec<_> = pair
            .messages
            .iter()
            .filter(|m| m.sequence > after)
            .take(usize::from(limit))
            .map(|m| PrivateMessage {
                id: m.id.clone(),
                sender: m.sender.clone(),
                sequence: m.sequence,
                received_at: m.received_at,
                text: m.text.clone(),
            })
            .collect();
        let next_sequence = messages.last().map_or(after, |m| m.sequence);
        Ok(PrivateReply::Conversation(PrivateConversation {
            id: id.into(),
            peer: peer.into(),
            messages,
            next_sequence,
            has_more: next_sequence < count,
        }))
    }
}

impl GovernedRoom {
    fn private_path(&self) -> PathBuf {
        self.path.with_extension("private-person-pairs-v1.json")
    }

    fn private_load(&self) -> Result<Store, String> {
        use std::io::Read as _;
        let path = self.private_path();
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Store {
                    version: 1,
                    room: self.record.room.clone(),
                    ..Store::default()
                });
            }
            Err(_) => return Err("cannot open private conversation custody file".into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read private conversation custody file")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("private conversation custody file exceeds capacity".into());
        }
        let store: Store = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid private conversation custody file")?;
        store.validate(&self.record.room)?;
        Ok(store)
    }
    fn private_persist(&self, store: &Store) -> Result<(), String> {
        store.validate(&self.record.room)?;
        let bytes =
            serde_json::to_vec(store).map_err(|_| "cannot encode private conversation custody")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("private conversation custody file exceeds capacity".into());
        }
        tilo_files::native::atomic::write_private(&self.private_path(), &bytes)
            .map_err(|_| "cannot persist private conversation custody file".into())
    }
    fn private_caller(&self, caller: &DeviceCert, now: SystemTime) -> Result<String, String> {
        if !self.peer.certificates().iter().any(|cert| cert == caller) {
            return Err("private conversation caller certificate is not current".into());
        }
        let member = self.peer.permissions(caller.device_public_key, now)?;
        let person = hex::encode(caller.user_public_key);
        if member.foreign {
            return Err("foreign devices cannot access person conversations".into());
        }
        if member.delegate.is_some() || member.person != person {
            return Err("private conversations require a current nondelegated person".into());
        }
        Ok(person)
    }
    fn private_contacts(&self, own: &str, now: SystemTime) -> Result<Vec<PrivateContact>, String> {
        Ok(self
            .peer
            .governance()
            .governed_members(now)?
            .into_iter()
            .filter(|m| !m.removed && hex::encode(m.user_public_key) != own)
            .filter(|m| {
                m.active_device_keys.iter().any(|device| {
                    self.peer.permissions(*device, now).is_ok_and(|p| {
                        !p.foreign
                            && p.delegate.is_none()
                            && p.person == hex::encode(m.user_public_key)
                    })
                })
            })
            .map(|m| PrivateContact {
                public_key: hex::encode(m.user_public_key),
                display_name: m.display_name,
            })
            .collect())
    }
    fn private_pair(
        &self,
        caller: &DeviceCert,
        peer: &str,
        now: SystemTime,
    ) -> Result<(String, [String; 2]), String> {
        valid_key(peer)?;
        let own = self.private_caller(caller, now)?;
        if !self
            .private_contacts(&own, now)?
            .iter()
            .any(|c| c.public_key == peer)
        {
            return Err("private conversation peer is not another current certified person".into());
        }
        let mut people = [own, peer.into()];
        people.sort();
        Ok((pair_id(&self.record.room, &people)?, people))
    }

    pub(super) fn private_command(
        &self,
        caller: &DeviceCert,
        operation: PrivateOperation,
    ) -> Result<PrivateReply, String> {
        let now = SystemTime::now();
        let own = self.private_caller(caller, now)?;
        if matches!(&operation, PrivateOperation::Contacts) {
            return Ok(PrivateReply::Contacts(self.private_contacts(&own, now)?));
        }
        let (PrivateOperation::Open { peer }
        | PrivateOperation::Read { peer, .. }
        | PrivateOperation::Send { peer, .. }
        | PrivateOperation::Watch { peer, .. }) = &operation
        else {
            return Err("invalid private operation".into());
        };
        let (id, people) = self.private_pair(caller, peer, now)?;
        let mut store = self.private_load()?;
        match operation {
            PrivateOperation::Open { peer } => {
                if !store.pairs.contains_key(&id) {
                    store.ensure_pair(&id, people)?;
                    self.private_persist(&store)?;
                }
                store.conversation(&id, &peer, 0, 50)
            }
            PrivateOperation::Read {
                peer,
                after_sequence,
                limit,
            } => store.conversation(&id, &peer, after_sequence, limit),
            PrivateOperation::Watch { after_sequence, .. } => {
                let sequence = store.count(&id)?;
                if after_sequence > sequence {
                    return Err("private conversation cursor out of bounds".into());
                }
                // Waiting is the endpoint's job, outside the room lock. An equal
                // sequence means no change yet, not permission to broadcast.
                Ok(PrivateReply::Changed(sequence))
            }
            PrivateOperation::Send {
                invocation, text, ..
            } => {
                valid_text(&text)?;
                if invocation.is_nil() {
                    return Err("nil private message invocation".into());
                }
                let body_hash = *blake3::hash(text.as_bytes()).as_bytes();
                if let Some(old) = store.invocations.get(&invocation) {
                    if old.pair != id
                        || old.source != caller.device_public_key
                        || old.sender != own
                        || old.body_hash != body_hash
                    {
                        return Err(
                            "private message UUID reused with changed source, pair or body".into(),
                        );
                    }
                    return Ok(PrivateReply::Sent);
                }
                if store.invocations.len() >= MAX_MESSAGES {
                    return Err("private message capacity exhausted; no messages dropped".into());
                }
                store.ensure_pair(&id, people)?;
                let sequence = store
                    .count(&id)?
                    .checked_add(1)
                    .ok_or("private sequence exhausted")?;
                let received_at = i64::try_from(
                    now.duration_since(SystemTime::UNIX_EPOCH)
                        .map_err(|_| "private message clock predates Unix epoch")?
                        .as_secs(),
                )
                .map_err(|_| "private message timestamp overflow")?;
                let message_id = message_id(&id, invocation);
                store
                    .pairs
                    .get_mut(&id)
                    .ok_or("private pair absent")?
                    .messages
                    .push(Message {
                        id: message_id.clone(),
                        sender: own.clone(),
                        sequence,
                        received_at,
                        text,
                    });
                store.invocations.insert(
                    invocation,
                    Invocation {
                        pair: id.clone(),
                        source: caller.device_public_key,
                        sender: own,
                        body_hash,
                        message_id,
                    },
                );
                // No peer mutation, settle, room generation or public delta.
                self.private_persist(&store)?;
                self.private_notify(id, sequence);
                Ok(PrivateReply::Sent)
            }
            PrivateOperation::Contacts => Err("invalid private operation".into()),
        }
    }

    fn private_notify(&self, id: String, sequence: u64) {
        let path = self.private_path();
        {
            let mut channels = channels()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            channels.retain(|_, sender| sender.receiver_count() > 0);
            if let Some(sender) = channels.get(&(path, id)) {
                sender.send_replace(sequence);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn private_watcher_count(
        &self,
        caller: &DeviceCert,
        peer: &str,
    ) -> Result<usize, String> {
        let (id, _) = self.private_pair(caller, peer, SystemTime::now())?;
        let channels = channels()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(channels
            .get(&(self.private_path(), id))
            .map_or(0, watch::Sender::receiver_count))
    }

    /// Pair-specific wakeups; possession of a receiver is not lasting read authority.
    /// Subscribe and inspect the current sequence while holding the room lock,
    /// then release it before waiting. Reauthorize each reply, even on timeout.
    pub(super) fn private_watch(
        &self,
        caller: &DeviceCert,
        peer: &str,
    ) -> Result<watch::Receiver<u64>, String> {
        let (id, _) = self.private_pair(caller, peer, SystemTime::now())?;
        let sequence = self.private_load()?.count(&id)?;
        let mut channels = channels()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        channels.retain(|_, sender| sender.receiver_count() > 0);
        Ok(channels
            .entry((self.private_path(), id))
            .or_insert_with(|| watch::channel(sequence).0)
            .subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::join;
    use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};

    fn member(room: &mut GovernedRoom, key: &SigningKey, seed: u8) -> DeviceCert {
        member_device(room, key, seed, seed.checked_add(1).unwrap(), false)
    }

    fn member_device(
        room: &mut GovernedRoom,
        key: &SigningKey,
        seed: u8,
        device_seed: u8,
        foreign: bool,
    ) -> DeviceCert {
        let person_key = SigningKey::from_bytes(&[seed; 32]);
        let device = SigningKey::from_bytes(&[device_seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: person_key.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Person".into(),
            capabilities: DeviceCapabilities {
                foreign,
                ..DeviceCapabilities::member()
            },
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &person_key).unwrap();
        let now = SystemTime::now();
        let ns = u64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap();
        let signed = join::issue_invitation(
            join::InviteGrant {
                workspace_id: room.record.workspace_id,
                room: room.record.room.clone(),
                authority: room.record.authority.clone(),
                issuer: room.record.authority.clone(),
                delegation: None,
                role: join::MemberRole::Editor,
                nonce: [device_seed; 16],
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some("Person".into()),
                expires_at_ns: ns.checked_add(60_000_000_000).unwrap(),
                transport: Vec::new(),
            },
            key,
        )
        .unwrap();
        let verified = join::decode_and_verify_invitation(
            &signed,
            Some(cert.user_public_key),
            &RevocationList::new(),
            ns,
        )
        .unwrap();
        let challenge = join::JoinChallenge::new(
            &verified,
            &cert,
            [seed; 32],
            [seed.checked_add(1).unwrap(); 32],
            ns.checked_add(30_000_000_000).unwrap(),
            ns,
        )
        .unwrap();
        let proof = join::sign_join_proof(challenge.clone(), &device).unwrap();
        let redemption =
            join::verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), ns).unwrap();
        assert!(
            room.peer
                .governance_mut()
                .admit_invited_member(&redemption, key, now)
                .unwrap()
        );
        cert
    }

    fn send(
        room: &GovernedRoom,
        caller: &DeviceCert,
        peer: &DeviceCert,
        invocation: Uuid,
        text: &str,
    ) -> Result<PrivateReply, String> {
        room.private_command(
            caller,
            PrivateOperation::Send {
                peer: hex::encode(peer.user_public_key),
                invocation,
                text: text.into(),
            },
        )
    }

    #[test]
    fn durable_pair_is_shared_by_both_people_but_not_publication_or_other_pairs() {
        let directory = tempfile::tempdir().unwrap();
        let (key, governor) = super::super::tests::governor();
        let mut room = GovernedRoom::create(
            directory.path(),
            "Conversations",
            &governor,
            &key,
            SystemTime::now(),
        )
        .unwrap();
        let alice = member(&mut room, &key, 31);
        let bob = member(&mut room, &key, 41);
        let charlie = member(&mut room, &key, 51);
        room.store_peer().unwrap();
        room.persist().unwrap();
        let alice_key = hex::encode(alice.user_public_key);
        let bob_key = hex::encode(bob.user_public_key);
        // A host-only governor device is not a person's messaging endpoint.
        let third_person_key = hex::encode(charlie.user_public_key);
        // No special local-owner shortcut: these are two remote people.
        room.private_command(
            &alice,
            PrivateOperation::Open {
                peer: bob_key.clone(),
            },
        )
        .unwrap();
        room.private_command(
            &alice,
            PrivateOperation::Open {
                peer: third_person_key.clone(),
            },
        )
        .unwrap();
        let mut pair_watch = room.private_watch(&alice, &bob_key).unwrap();
        let reverse_watch = room.private_watch(&bob, &alice_key).unwrap();
        let unrelated = room.private_watch(&alice, &third_person_key).unwrap();
        let public_peer = room.peer.durable().unwrap();
        let public_record = std::fs::read(&room.path).unwrap();
        let invocation = Uuid::new_v4();
        assert!(matches!(
            send(&room, &alice, &bob, invocation, "Private text").unwrap(),
            PrivateReply::Sent
        ));
        assert!(pair_watch.has_changed().unwrap());
        assert!(reverse_watch.has_changed().unwrap());
        assert!(!unrelated.has_changed().unwrap());
        assert_eq!(*pair_watch.borrow_and_update(), 1);
        assert!(matches!(
            send(&room, &alice, &bob, invocation, "Private text").unwrap(),
            PrivateReply::Sent
        ));
        assert!(!pair_watch.has_changed().unwrap());
        assert_eq!(
            room.private_load()
                .unwrap()
                .count(
                    &pair_id(&room.record.room, &{
                        let mut p = [alice_key.clone(), bob_key];
                        p.sort();
                        p
                    })
                    .unwrap()
                )
                .unwrap(),
            1
        );
        assert!(send(&room, &alice, &bob, invocation, "Changed").is_err());
        assert!(send(&room, &bob, &alice, invocation, "Private text").is_err());
        assert!(send(&room, &alice, &governor, invocation, "Private text").is_err());
        let PrivateReply::Conversation(conversation) = room
            .private_command(
                &bob,
                PrivateOperation::Read {
                    peer: alice_key.clone(),
                    after_sequence: 0,
                    limit: 1,
                },
            )
            .unwrap()
        else {
            panic!("expected conversation")
        };
        assert_eq!(conversation.messages.len(), 1);
        assert_eq!(conversation.messages[0].sender, alice_key);
        assert_eq!(conversation.messages[0].text, "Private text");
        assert_eq!(conversation.next_sequence, 1);
        assert!(!conversation.has_more);
        assert_eq!(public_peer, room.peer.durable().unwrap());
        assert_eq!(public_record, std::fs::read(&room.path).unwrap());
        // Custody loads from disk, not a hidden in-memory native owner.
        let reopened = GovernedRoom::open(room.path.clone(), &governor, &key).unwrap();
        assert_restored_conversation(&reopened, &alice, &bob, &conversation);
    }

    fn assert_restored_conversation(
        reopened: &GovernedRoom,
        alice: &DeviceCert,
        bob: &DeviceCert,
        conversation: &PrivateConversation,
    ) {
        let alice_key = hex::encode(alice.user_public_key);
        let bob_key = hex::encode(bob.user_public_key);
        let PrivateReply::Conversation(restored) = reopened
            .private_command(
                bob,
                PrivateOperation::Read {
                    peer: alice_key,
                    after_sequence: 0,
                    limit: 100,
                },
            )
            .unwrap()
        else {
            panic!("expected restored conversation")
        };
        assert_eq!(restored.id, conversation.id);
        assert_eq!(restored.messages[0].text, "Private text");
        assert!(
            reopened
                .private_command(
                    alice,
                    PrivateOperation::Read {
                        peer: bob_key.clone(),
                        after_sequence: 2,
                        limit: 1,
                    }
                )
                .is_err()
        );
        assert!(
            reopened
                .private_command(
                    alice,
                    PrivateOperation::Read {
                        peer: bob_key,
                        after_sequence: 0,
                        limit: 0,
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn current_foreign_person_cannot_use_retained_private_custody() {
        let directory = tempfile::tempdir().unwrap();
        let (key, governor) = super::super::tests::governor();
        let mut room = GovernedRoom::create(
            directory.path(),
            "Private device bounds",
            &governor,
            &key,
            SystemTime::now(),
        )
        .unwrap();
        let alice = member(&mut room, &key, 31);
        let bob = member(&mut room, &key, 41);
        let invocation = Uuid::new_v4();
        send(&room, &alice, &bob, invocation, "retained private text").unwrap();
        let foreign = member_device(&mut room, &key, 31, 35, true);
        let member = room
            .peer
            .permissions(foreign.device_public_key, SystemTime::now())
            .unwrap();
        assert!(member.foreign);
        assert!(member.delegate.is_none());
        assert_eq!(member.role, collab_types::Role::Editor);
        assert_eq!(foreign.user_public_key, alice.user_public_key);
        assert!(room.peer.certificates().contains(&foreign));
        room.store_peer().unwrap();
        room.persist().unwrap();
        let before = std::fs::read(room.private_path()).unwrap();
        let reopened = GovernedRoom::open(room.path.clone(), &governor, &key).unwrap();
        let peer = hex::encode(bob.user_public_key);
        for operation in [
            PrivateOperation::Contacts,
            PrivateOperation::Open { peer: peer.clone() },
            PrivateOperation::Read {
                peer: peer.clone(),
                after_sequence: 0,
                limit: 50,
            },
            PrivateOperation::Watch {
                peer: peer.clone(),
                after_sequence: 1,
            },
            PrivateOperation::Send {
                peer: peer.clone(),
                invocation,
                text: "retained private text".into(),
            },
            PrivateOperation::Send {
                peer: peer.clone(),
                invocation: Uuid::new_v4(),
                text: "new private text".into(),
            },
        ] {
            assert!(
                reopened
                    .private_command(&foreign, operation)
                    .err()
                    .unwrap()
                    .contains("foreign devices")
            );
        }
        assert!(
            reopened
                .private_watch(&foreign, &peer)
                .err()
                .unwrap()
                .contains("foreign devices")
        );
        // The ordinary local Editor of the same person retains access.
        assert!(
            reopened
                .private_command(
                    &alice,
                    PrivateOperation::Read {
                        peer: peer.clone(),
                        after_sequence: 0,
                        limit: 50,
                    }
                )
                .is_ok()
        );
        assert!(reopened.private_watch(&alice, &peer).is_ok());
        send(&reopened, &alice, &bob, invocation, "retained private text").unwrap();
        assert_eq!(before, std::fs::read(reopened.private_path()).unwrap());
    }

    #[test]
    fn current_certified_membership_is_required_even_for_contacts() {
        let directory = tempfile::tempdir().unwrap();
        let (key, governor) = super::super::tests::governor();
        let mut room = GovernedRoom::create(
            directory.path(),
            "Membership",
            &governor,
            &key,
            SystemTime::now(),
        )
        .unwrap();
        let alice = member(&mut room, &key, 51);
        let alice_key = hex::encode(alice.user_public_key);
        let mut forged = alice.clone();
        forged.device_label = "Changed certificate".into();
        assert!(
            room.private_command(&forged, PrivateOperation::Contacts)
                .is_err()
        );
        assert!(
            room.private_watch(&forged, &hex::encode(governor.user_public_key))
                .is_err()
        );
        assert!(
            room.private_command(
                &alice,
                PrivateOperation::Open {
                    peer: alice_key.clone()
                }
            )
            .is_err()
        );
        assert!(
            room.private_command(
                &governor,
                PrivateOperation::Open {
                    peer: alice_key.to_uppercase()
                }
            )
            .is_err()
        );
        room.peer
            .governance_mut()
            .remove_member(alice.user_public_key, &key, SystemTime::now())
            .unwrap();
        assert!(
            room.private_command(&alice, PrivateOperation::Contacts)
                .is_err()
        );
        assert!(
            room.private_command(&governor, PrivateOperation::Open { peer: alice_key })
                .is_err()
        );
    }
}
