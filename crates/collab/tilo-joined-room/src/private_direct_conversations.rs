//! Two-person text retained exclusively by the trusted native workspace owner.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::SystemTime};
use tilo_peer_state::caller_authority::CallerAuthority;

const MAX_CONVERSATIONS: usize = 1024;
const MAX_MESSAGES: usize = 4096;
const MAX_TEXT_BYTES: usize = 16_384;
const MAX_PAGE: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectContact {
    pub public_key: String,
    /// The name the room keeps for the person; never one of their devices'
    /// labels.
    pub display_name: Option<String>,
    /// Whether a conversation between the caller and this person holds
    /// messages.
    pub conversation: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectMessageSend {
    pub invocation: uuid::Uuid,
    pub text: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectConversationPage {
    pub after_sequence: u64,
    pub limit: usize,
}
impl Default for DirectConversationPage {
    fn default() -> Self {
        Self {
            after_sequence: 0,
            limit: 50,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectMessage {
    pub id: String,
    pub sender: String,
    pub sequence: u64,
    /// Unix seconds assigned by the owner, never supplied by the sender.
    pub received_at: i64,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectConversation {
    pub id: String,
    pub peer: String,
    pub messages: Vec<DirectMessage>,
    pub next_sequence: u64,
    pub has_more: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectMessageStatus {
    Saved,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectMessageReceipt {
    pub status: DirectMessageStatus,
    pub conversation_id: String,
    pub message: DirectMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    room: RoomId,
    people: [String; 2],
    messages: Vec<DirectMessage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    conversation: String,
    source: [u8; 32],
    body_hash: [u8; 32],
    message_id: String,
}
/// Private owner-file envelope. Never include this in a room document or relay.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateDirectConversationStore {
    version: u16,
    pairs: BTreeMap<String, Pair>,
    invocations: BTreeMap<uuid::Uuid, Invocation>,
}
impl Default for PrivateDirectConversationStore {
    fn default() -> Self {
        Self {
            version: 1,
            pairs: BTreeMap::new(),
            invocations: BTreeMap::new(),
        }
    }
}
fn key(bytes: [u8; 32]) -> String {
    blake3::Hash::from_bytes(bytes).to_hex().to_string()
}
fn validate_key(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("person key must be canonical lowercase public-key hex".into());
    }
    Ok(())
}
fn text_valid(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err("private message is empty or exceeds UTF-8 byte bound".into());
    }
    Ok(())
}
fn pair_id(room: RoomId, people: &[String; 2]) -> Result<String, String> {
    let bytes =
        serde_json::to_vec(&("private-person-pair-v1", room, people)).map_err(|e| e.to_string())?;
    Ok(format!("direct_{}", blake3::hash(&bytes).to_hex()))
}
fn message_id(conversation: &str, invocation: uuid::Uuid) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"private-person-message-v1");
    hash.update(conversation.as_bytes());
    hash.update(invocation.as_bytes());
    format!("dm_{}", hash.finalize().to_hex())
}
impl PrivateDirectConversationStore {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.pairs.len() > MAX_CONVERSATIONS
            || self.invocations.len() > MAX_MESSAGES
        {
            return Err("invalid private conversation version or capacity".into());
        }
        let mut count = 0usize;
        let mut retained = BTreeMap::new();
        for (id, pair) in &self.pairs {
            for person in &pair.people {
                validate_key(person)?;
            }
            if pair.people[0] >= pair.people[1] || pair_id(pair.room, &pair.people)? != *id {
                return Err("private conversation identity mismatch".into());
            }
            count = count
                .checked_add(pair.messages.len())
                .ok_or("private message count overflow")?;
            if count > MAX_MESSAGES {
                return Err("private conversation message capacity exhausted".into());
            }
            for (index, message) in pair.messages.iter().enumerate() {
                text_valid(&message.text)?;
                if message.sequence
                    != (index as u64)
                        .checked_add(1)
                        .ok_or("private sequence overflow")?
                    || !pair.people.contains(&message.sender)
                    || message.received_at < 0
                {
                    return Err("invalid private conversation message".into());
                }
                if retained
                    .insert(message.id.as_str(), (id, message))
                    .is_some()
                {
                    return Err("duplicate private message identity".into());
                }
            }
        }
        if count != self.invocations.len() {
            return Err("private message receipt count mismatch".into());
        }
        for (uuid, invocation) in &self.invocations {
            if uuid.is_nil() || invocation.message_id != message_id(&invocation.conversation, *uuid)
            {
                return Err("private message invocation identity mismatch".into());
            }
            let (pair, message) = retained
                .remove(invocation.message_id.as_str())
                .ok_or("private invocation message absent")?;
            if *pair != invocation.conversation
                || invocation.body_hash != *blake3::hash(message.text.as_bytes()).as_bytes()
            {
                return Err("private message receipt binding mismatch".into());
            }
        }
        Ok(())
    }
    fn conversation(
        &self,
        id: &str,
        peer: &str,
        page: DirectConversationPage,
    ) -> Result<DirectConversation, String> {
        if page.limit == 0 || page.limit > MAX_PAGE {
            return Err("private conversation page limit out of bounds".into());
        }
        let pair = self
            .pairs
            .get(id)
            .ok_or("private conversation not opened")?;
        if page.after_sequence > pair.messages.len() as u64 {
            return Err("private conversation cursor out of bounds".into());
        }
        let messages: Vec<_> = pair
            .messages
            .iter()
            .filter(|m| m.sequence > page.after_sequence)
            .take(page.limit)
            .cloned()
            .collect();
        let next_sequence = messages.last().map_or(page.after_sequence, |m| m.sequence);
        Ok(DirectConversation {
            id: id.into(),
            peer: peer.into(),
            has_more: next_sequence < pair.messages.len() as u64,
            next_sequence,
            messages,
        })
    }
}

fn caller<'a>(
    view: &NativeCommittedView,
    authority: &'a CallerAuthority,
    now: SystemTime,
) -> Result<&'a tilo_user_identity::DeviceCert, String> {
    let trusted = view.device() == view.governor()
        || view
            .host()
            .is_some_and(|host| host == view.device() && host.capabilities.can_act_as_host);
    if !trusted {
        return Err("private conversations require trusted workspace host custody".into());
    }
    view.peer()
        .permissions(view.device().device_public_key, now)?;
    let CallerAuthority::Person { caller } = authority else {
        return Err("private conversations require authenticated person transport".into());
    };
    if !view.peer().certificates().iter().any(|cert| cert == caller) {
        return Err("private conversation caller certificate is not current".into());
    }
    let member = view.peer().permissions(caller.device_public_key, now)?;
    if member.foreign {
        return Err("foreign devices cannot access person conversations".into());
    }
    if member.delegate.is_some() {
        return Err("delegated agents cannot read person conversations".into());
    }
    Ok(caller)
}
fn contacts(
    view: &NativeCommittedView,
    own: [u8; 32],
    now: SystemTime,
) -> Result<Vec<DirectContact>, String> {
    let certificates = view.peer().certificates();
    let own_key = key(own);
    let pairs = &view.record.private_direct_conversations.pairs;
    Ok(view
        .peer()
        .governance()
        .governed_members(now)?
        .into_iter()
        .filter(|m| !m.removed && m.user_public_key != own)
        .filter(|m| {
            m.active_device_keys.iter().any(|device| {
                view.peer()
                    .permissions(*device, now)
                    .is_ok_and(|p| !p.foreign && p.delegate.is_none())
            })
        })
        .map(|m| {
            let person = key(m.user_public_key);
            // A device's label ("Browser", "Primary device") never names
            // its person, even where the room recorded it as their name.
            let display_name = m.display_name.filter(|name| {
                !certificates.iter().any(|cert| {
                    cert.user_public_key == m.user_public_key
                        && cert.device_label.trim() == name.trim()
                })
            });
            let conversation = pairs.values().any(|pair| {
                !pair.messages.is_empty()
                    && pair.people.contains(&person)
                    && pair.people.contains(&own_key)
            });
            DirectContact {
                public_key: person,
                display_name,
                conversation,
            }
        })
        .collect())
}
fn authorized_pair(
    view: &NativeCommittedView,
    room: RoomId,
    peer: &str,
    authority: &CallerAuthority,
    now: SystemTime,
) -> Result<(String, [String; 2]), String> {
    validate_key(peer)?;
    let cert = caller(view, authority, now)?;
    if !contacts(view, cert.user_public_key, now)?
        .iter()
        .any(|c| c.public_key == peer)
    {
        return Err("private conversation peer is not another current certified person".into());
    }
    let mut people = [key(cert.user_public_key), peer.into()];
    people.sort();
    Ok((pair_id(room, &people)?, people))
}
fn ensure_pair(
    store: &mut PrivateDirectConversationStore,
    room: RoomId,
    id: &str,
    people: [String; 2],
) -> Result<(), String> {
    if !store.pairs.contains_key(id) {
        if store.pairs.len() >= MAX_CONVERSATIONS {
            return Err("private conversation capacity exhausted".into());
        }
        store.pairs.insert(
            id.into(),
            Pair {
                room,
                people,
                messages: Vec::new(),
            },
        );
    }
    Ok(())
}
impl NativeWorkspaceOwner {
    /// Subscribe only to an existing, freshly authorized certified pair.
    /// The mutation guard closes the snapshot/subscription race with persistence.
    ///
    /// # Errors
    /// Refuses stale certificates, unavailable custody, or an unauthorized pair.
    pub fn subscribe_direct_conversation(
        &self,
        room: RoomId,
        peer: &str,
        authority: &CallerAuthority,
        now: SystemTime,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        let _turn = self.0.mutation.lock();
        let (id, count) = self.with_current(room, |view| {
            let (id, _) = authorized_pair(view, room, peer, authority, now)?;
            let pair = view
                .record
                .private_direct_conversations
                .pairs
                .get(&id)
                .ok_or("private conversation is not open")?;
            Ok((id, pair.messages.len() as u64))
        })?;
        let mut channels = self.0.direct_changes.lock();
        // Closed pairs need no retained process-local channel.
        channels.retain(|_, sender| sender.receiver_count() > 0);
        Ok(channels
            .entry(id)
            .or_insert_with(|| tokio::sync::watch::channel(count).0)
            .subscribe())
    }

    /// Caller authority must be derived from real native transport, never app arguments.
    ///
    /// # Errors
    /// Refuses stale or delegated callers and unavailable current membership.
    pub fn direct_contacts(
        &self,
        room: RoomId,
        authority: &CallerAuthority,
        now: SystemTime,
    ) -> Result<Vec<DirectContact>, String> {
        self.with_current(room, |view| {
            let cert = caller(view, authority, now)?;
            view.record.private_direct_conversations.validate()?;
            contacts(view, cert.user_public_key, now)
        })
    }
    /// Persist the unordered certified pair before returning its stable owner ID.
    ///
    /// # Errors
    /// Refuses unauthorized pairs, invalid keys, or unavailable durable custody.
    pub fn open_direct_conversation(
        &self,
        room: RoomId,
        peer_public_key: &str,
        authority: &CallerAuthority,
        now: SystemTime,
    ) -> Result<DirectConversation, String> {
        let _turn = self.0.mutation.lock();
        let (mut candidate, id, people) = self.with_current(room, |view| {
            let (id, people) = authorized_pair(view, room, peer_public_key, authority, now)?;
            view.record.private_direct_conversations.validate()?;
            Ok((view.clone(), id, people))
        })?;
        if !candidate
            .record
            .private_direct_conversations
            .pairs
            .contains_key(&id)
        {
            ensure_pair(
                &mut candidate.record.private_direct_conversations,
                room,
                &id,
                people,
            )?;
            candidate.record.generation = candidate
                .record
                .generation
                .checked_add(1)
                .ok_or("owner generation exhausted")?;
            self.publish_room(room, candidate, None)?;
        }
        self.read_direct_conversation(
            room,
            peer_public_key,
            authority,
            DirectConversationPage::default(),
            now,
        )
    }
    ///
    /// # Errors
    /// Refuses unauthorized pairs, invalid cursors, or unavailable custody.
    pub fn read_direct_conversation(
        &self,
        room: RoomId,
        peer_public_key: &str,
        authority: &CallerAuthority,
        page: DirectConversationPage,
        now: SystemTime,
    ) -> Result<DirectConversation, String> {
        self.with_current(room, |view| {
            let (id, _) = authorized_pair(view, room, peer_public_key, authority, now)?;
            let store = &view.record.private_direct_conversations;
            store.validate()?;
            store.conversation(&id, peer_public_key, page)
        })
    }
    /// Saved means the owner atomic persistence barrier completed, not delivery.
    ///
    /// # Errors
    /// Refuses unauthorized pairs, invalid payloads, conflicting invocation IDs, or full custody.
    pub fn send_direct_message(
        &self,
        room: RoomId,
        peer_public_key: &str,
        authority: &CallerAuthority,
        request: DirectMessageSend,
        now: SystemTime,
    ) -> Result<DirectMessageReceipt, String> {
        text_valid(&request.text)?;
        if request.invocation.is_nil() {
            return Err("nil private message invocation".into());
        }
        let _turn = self.0.mutation.lock();
        let (mut candidate, id, people) = self.with_current(room, |view| {
            let (id, people) = authorized_pair(view, room, peer_public_key, authority, now)?;
            Ok((view.clone(), id, people))
        })?;
        let store = &mut candidate.record.private_direct_conversations;
        store.validate()?;
        let source = authority.certificate().device_public_key;
        let body_hash = *blake3::hash(request.text.as_bytes()).as_bytes();
        if let Some(old) = store.invocations.get(&request.invocation) {
            if old.conversation != id || old.source != source || old.body_hash != body_hash {
                return Err("private message UUID reused with changed source, pair or body".into());
            }
            let message = store
                .pairs
                .get(&id)
                .and_then(|pair| pair.messages.iter().find(|m| m.id == old.message_id))
                .ok_or("private message receipt absent")?
                .clone();
            return Ok(DirectMessageReceipt {
                status: DirectMessageStatus::Saved,
                conversation_id: id,
                message,
            });
        }
        if store.invocations.len() >= MAX_MESSAGES {
            return Err("private message capacity exhausted; no messages dropped".into());
        }
        ensure_pair(store, room, &id, people)?;
        let pair = store.pairs.get_mut(&id).ok_or("private pair absent")?;
        let message = DirectMessage {
            id: message_id(&id, request.invocation),
            sender: key(authority.certificate().user_public_key),
            sequence: (pair.messages.len() as u64)
                .checked_add(1)
                .ok_or("private sequence overflow")?,
            received_at: i64::try_from(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_secs(),
            )
            .map_err(|e| e.to_string())?,
            text: request.text,
        };
        pair.messages.push(message.clone());
        store.invocations.insert(
            request.invocation,
            Invocation {
                conversation: id.clone(),
                source,
                body_hash,
                message_id: message.id.clone(),
            },
        );
        store.validate()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        // A failed durable publication, or an idempotent retry, never signals.
        if let Some(sender) = self.0.direct_changes.lock().get(&id) {
            sender.send_replace(message.sequence);
        }
        Ok(DirectMessageReceipt {
            status: DirectMessageStatus::Saved,
            conversation_id: id,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::join::{self, MemberRole};
    use tilo_user_identity::{DeviceCert, RevocationList};

    struct Fixture {
        _directory: tempfile::TempDir,
        path: std::path::PathBuf,
        owner: NativeWorkspaceOwner,
        room: RoomId,
        governor: DeviceCert,
        key: SigningKey,
        now: SystemTime,
    }
    impl Fixture {
        fn new() -> Self {
            let directory =
                tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let path = directory.path().join("owner.json");
            let owner = crate::open_file(&path).unwrap();
            let key = SigningKey::from_bytes(&[61; 32]);
            let governor =
                crate::testing::certificate(&SigningKey::from_bytes(&[62; 32]), &key, "Owner");
            crate::testing::enroll_interactive_governor(&owner, &key, &governor).unwrap();
            Self {
                _directory: directory,
                path,
                owner,
                room: RoomId::parse(crate::testing::ROOM).unwrap(),
                governor,
                key,
                now: SystemTime::now(),
            }
        }
        fn person(&self) -> CallerAuthority {
            CallerAuthority::Person {
                caller: self.governor.clone(),
            }
        }
        fn mutate(&self, change: impl FnOnce(&mut tilo_peer_state::StatePeer)) {
            let _turn = self.owner.0.mutation.lock();
            let mut candidate = self
                .owner
                .with_current(self.room, |view| Ok(view.clone()))
                .unwrap();
            change(&mut candidate.peer);
            candidate.record.peer = candidate.peer.durable().unwrap();
            candidate.record.generation = candidate.record.generation.checked_add(1).unwrap();
            self.owner.publish_room(self.room, candidate, None).unwrap();
        }
        fn member(&self, seed: u8, role: MemberRole) -> DeviceCert {
            self.member_device(seed, seed.checked_add(1).unwrap(), role, None)
        }
        fn member_device(
            &self,
            seed: u8,
            device_seed: u8,
            role: MemberRole,
            expires_at_ns: Option<u64>,
        ) -> DeviceCert {
            self.member_device_bound(seed, device_seed, role, expires_at_ns, false)
        }
        fn member_device_bound(
            &self,
            seed: u8,
            device_seed: u8,
            role: MemberRole,
            expires_at_ns: Option<u64>,
            foreign: bool,
        ) -> DeviceCert {
            let root = SigningKey::from_bytes(&[seed; 32]);
            let device = SigningKey::from_bytes(&[device_seed; 32]);
            let mut cert = crate::testing::certificate(&root, &device, "Person");
            cert.expires_at_ns = expires_at_ns;
            cert.capabilities.foreign = foreign;
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            let ns = crate::testing::ns(self.now).unwrap();
            let hint = self
                .owner
                .with_current(self.room, |v| Ok(v.transport_hint()?.clone()))
                .unwrap();
            let signed = join::issue_invitation(
                join::InviteGrant {
                    workspace_id: tilo_relay_protocol::room_workspace_id(crate::testing::ROOM)
                        .unwrap(),
                    room: crate::testing::ROOM.into(),
                    authority: self.governor.clone(),
                    issuer: self.governor.clone(),
                    delegation: None,
                    role,
                    nonce: [device_seed; 16],
                    recipient_user_public_key: Some(cert.user_public_key),
                    invitee_name: Some("Person".into()),
                    expires_at_ns: ns.checked_add(60_000_000_000).unwrap(),
                    transport: vec![hint],
                },
                &self.key,
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
                [device_seed; 32],
                [device_seed.wrapping_add(1); 32],
                ns.checked_add(30_000_000_000).unwrap(),
                ns,
            )
            .unwrap();
            let proof = join::sign_join_proof(challenge.clone(), &device).unwrap();
            let redemption =
                join::verify_join_proof(&proof, &challenge, &cert, &RevocationList::new(), ns)
                    .unwrap();
            self.mutate(|peer| {
                assert!(
                    peer.governance_mut()
                        .admit_invited_member(&redemption, &self.key, self.now)
                        .unwrap()
                );
            });
            cert
        }
        fn send(
            &self,
            peer: &DeviceCert,
            invocation: uuid::Uuid,
            text: &str,
        ) -> Result<DirectMessageReceipt, String> {
            self.owner.send_direct_message(
                self.room,
                &key(peer.user_public_key),
                &self.person(),
                DirectMessageSend {
                    invocation,
                    text: text.into(),
                },
                self.now,
            )
        }
    }

    #[test]
    fn private_change_lane_is_pair_scoped_and_only_signals_durable_new_sends() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
        let fixture = Fixture::new();
        let peer = fixture.member(71, MemberRole::Editor);
        let other = fixture.member(73, MemberRole::Editor);
        let public_key = key(peer.user_public_key);
        assert!(
            fixture
                .owner
                .subscribe_direct_conversation(
                    fixture.room,
                    &public_key,
                    &fixture.person(),
                    fixture.now
                )
                .is_err()
        );
        fixture
            .owner
            .open_direct_conversation(fixture.room, &public_key, &fixture.person(), fixture.now)
            .unwrap();
        let mut changes = fixture
            .owner
            .subscribe_direct_conversation(
                fixture.room,
                &public_key,
                &fixture.person(),
                fixture.now,
            )
            .unwrap();
        assert_eq!(*changes.borrow_and_update(), 0);
        fixture
            .send(&other, uuid::Uuid::new_v4(), "another pair")
            .unwrap();
        assert!(!changes.has_changed().unwrap());
        assert!(
            with_failure(Checkpoint::BeforePublish, || fixture.send(
                &peer,
                uuid::Uuid::new_v4(),
                "not persisted"
            ))
            .is_err()
        );
        assert!(!changes.has_changed().unwrap());
        let id = uuid::Uuid::new_v4();
        fixture.send(&peer, id, "persisted").unwrap();
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), 1);
        fixture.send(&peer, id, "persisted").unwrap();
        assert!(!changes.has_changed().unwrap());
        // The other endpoint sees the same count, not a public room event.
        let reverse = fixture
            .owner
            .subscribe_direct_conversation(
                fixture.room,
                &key(fixture.governor.user_public_key),
                &CallerAuthority::Person { caller: peer },
                fixture.now,
            )
            .unwrap();
        assert_eq!(*reverse.borrow(), 1);
        assert!(
            fixture
                .owner
                .subscribe_direct_conversation(
                    fixture.room,
                    &public_key,
                    &CallerAuthority::Person { caller: other },
                    fixture.now
                )
                .is_err()
        );
    }

    #[test]
    fn certified_pair_is_symmetric_durable_private_and_idempotent() {
        let fixture = Fixture::new();
        let peer = fixture.member(71, MemberRole::Editor);
        let authority = CallerAuthority::Person {
            caller: peer.clone(),
        };
        let public_before = fixture
            .owner
            .with_current(fixture.room, |v| {
                Ok((
                    v.peer.durable()?,
                    crate::testing::entries(v)?,
                    crate::testing::projection(v)?,
                ))
            })
            .unwrap();
        let invocation = uuid::Uuid::new_v4();
        let marker = "private-direct-body-canary-unique";
        let receipt = fixture.send(&peer, invocation, marker).unwrap();
        assert_eq!(receipt.status, DirectMessageStatus::Saved);
        assert_eq!(
            receipt.message.sender,
            key(fixture.governor.user_public_key)
        );
        assert_eq!(
            receipt.message.received_at,
            i64::try_from(
                fixture
                    .now
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            )
            .unwrap()
        );
        assert_eq!(
            fixture.send(&peer, invocation, marker).unwrap().message,
            receipt.message
        );
        assert!(fixture.send(&peer, invocation, "changed").is_err());
        let opened = fixture
            .owner
            .open_direct_conversation(
                fixture.room,
                &key(peer.user_public_key),
                &fixture.person(),
                fixture.now,
            )
            .unwrap();
        let reverse = fixture
            .owner
            .read_direct_conversation(
                fixture.room,
                &key(fixture.governor.user_public_key),
                &authority,
                DirectConversationPage::default(),
                fixture.now,
            )
            .unwrap();
        assert_eq!(opened.id, reverse.id);
        assert_eq!(reverse.messages, vec![receipt.message.clone()]);
        let public_after = fixture
            .owner
            .with_current(fixture.room, |v| {
                Ok((
                    v.peer.durable()?,
                    crate::testing::entries(v)?,
                    crate::testing::projection(v)?,
                ))
            })
            .unwrap();
        assert_eq!(public_before, public_after);
        for bytes in public_after
            .1
            .iter()
            .chain(public_after.2.iter())
            .chain(std::iter::once(&public_after.0))
        {
            assert!(!bytes.windows(marker.len()).any(|w| w == marker.as_bytes()));
        }
        let reopened = crate::open_file(&fixture.path).unwrap();
        let retained = reopened
            .read_direct_conversation(
                fixture.room,
                &key(peer.user_public_key),
                &fixture.person(),
                DirectConversationPage::default(),
                fixture.now,
            )
            .unwrap();
        assert_eq!(retained.messages, vec![receipt.message]);
    }

    #[test]
    fn unrelated_editor_host_agent_and_guessed_ids_do_not_reveal_pair() {
        let fixture = Fixture::new();
        let peer = fixture.member(73, MemberRole::Editor);
        let stranger = fixture.member(75, MemberRole::Editor);
        let receipt = fixture
            .send(&peer, uuid::Uuid::new_v4(), "pair secret")
            .unwrap();
        let unrelated = CallerAuthority::Person { caller: stranger };
        assert!(
            fixture
                .owner
                .read_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &unrelated,
                    DirectConversationPage::default(),
                    fixture.now
                )
                .is_err()
        );
        assert!(
            fixture
                .owner
                .read_direct_conversation(
                    fixture.room,
                    &receipt.conversation_id,
                    &unrelated,
                    DirectConversationPage::default(),
                    fixture.now
                )
                .is_err()
        );
        for authority in [
            CallerAuthority::Host {
                attestor: fixture.governor.clone(),
            },
            CallerAuthority::Agent {
                attestor: fixture.governor.clone(),
                agent_id: "scribe".into(),
            },
        ] {
            assert!(
                fixture
                    .owner
                    .direct_contacts(fixture.room, &authority, fixture.now)
                    .is_err()
            );
            assert!(
                fixture
                    .owner
                    .open_direct_conversation(
                        fixture.room,
                        &key(peer.user_public_key),
                        &authority,
                        fixture.now
                    )
                    .is_err()
            );
            assert!(
                fixture
                    .owner
                    .read_direct_conversation(
                        fixture.room,
                        &key(peer.user_public_key),
                        &authority,
                        DirectConversationPage::default(),
                        fixture.now
                    )
                    .is_err()
            );
            assert!(
                fixture
                    .owner
                    .send_direct_message(
                        fixture.room,
                        &key(peer.user_public_key),
                        &authority,
                        DirectMessageSend {
                            invocation: uuid::Uuid::new_v4(),
                            text: "secret".into()
                        },
                        fixture.now
                    )
                    .is_err()
            );
        }
        assert!(
            fixture
                .owner
                .direct_contacts(fixture.room, &fixture.person(), fixture.now)
                .unwrap()
                .iter()
                .any(|c| c.public_key == key(peer.user_public_key))
        );
    }

    #[test]
    fn certified_native_person_roles_do_not_require_editor_write_authority() {
        let fixture = Fixture::new();
        for (seed, role) in [
            (87, MemberRole::Editor),
            (89, MemberRole::Viewer),
            (91, MemberRole::Commenter),
            (93, MemberRole::Moderator),
        ] {
            let cert = fixture.member(seed, role);
            let person = CallerAuthority::Person {
                caller: cert.clone(),
            };
            let opened = fixture
                .owner
                .open_direct_conversation(
                    fixture.room,
                    &key(fixture.governor.user_public_key),
                    &person,
                    fixture.now,
                )
                .unwrap();
            let saved = fixture
                .owner
                .send_direct_message(
                    fixture.room,
                    &key(fixture.governor.user_public_key),
                    &person,
                    DirectMessageSend {
                        invocation: uuid::Uuid::new_v4(),
                        text: "private person message".into(),
                    },
                    fixture.now,
                )
                .unwrap();
            assert_eq!(saved.conversation_id, opened.id);
            assert_eq!(saved.message.sender, key(cert.user_public_key));
        }
    }

    fn assert_person_doors_refused(
        fixture: &Fixture,
        authority: &CallerAuthority,
        peer: &str,
        saved_invocation: uuid::Uuid,
        reason: &str,
    ) {
        let owner = &fixture.owner;
        let room = fixture.room;
        let now = fixture.now;
        assert!(
            owner
                .direct_contacts(room, authority, now)
                .err()
                .unwrap()
                .contains(reason)
        );
        assert!(
            owner
                .open_direct_conversation(room, peer, authority, now)
                .err()
                .unwrap()
                .contains(reason)
        );
        assert!(
            owner
                .read_direct_conversation(
                    room,
                    peer,
                    authority,
                    DirectConversationPage::default(),
                    now,
                )
                .err()
                .unwrap()
                .contains(reason)
        );
        assert!(
            owner
                .subscribe_direct_conversation(room, peer, authority, now)
                .err()
                .unwrap()
                .contains(reason)
        );
        for invocation in [saved_invocation, uuid::Uuid::new_v4()] {
            assert!(
                owner
                    .send_direct_message(
                        room,
                        peer,
                        authority,
                        DirectMessageSend {
                            invocation,
                            text: "retained private text".into()
                        },
                        now,
                    )
                    .err()
                    .unwrap()
                    .contains(reason)
            );
        }
    }

    #[test]
    fn foreign_current_person_is_denied_at_owner_even_for_retained_pair_and_retries() {
        let mut fixture = Fixture::new();
        let local = fixture.member(77, MemberRole::Editor);
        let local_authority = CallerAuthority::Person { caller: local };
        let peer = key(fixture.governor.user_public_key);
        let invocation = uuid::Uuid::new_v4();
        fixture
            .owner
            .send_direct_message(
                fixture.room,
                &peer,
                &local_authority,
                DirectMessageSend {
                    invocation,
                    text: "retained private text".into(),
                },
                fixture.now,
            )
            .unwrap();
        let foreign = fixture.member_device_bound(77, 80, MemberRole::Editor, None, true);
        fixture
            .owner
            .with_current(fixture.room, |view| {
                assert!(view.peer().certificates().contains(&foreign));
                let member = view
                    .peer()
                    .permissions(foreign.device_public_key, fixture.now)?;
                assert!(member.foreign);
                assert!(member.delegate.is_none());
                assert_eq!(member.role, collab_types::Role::Editor);
                Ok(())
            })
            .unwrap();
        let before = std::fs::read(&fixture.path).unwrap();
        fixture.owner = crate::open_file(&fixture.path).unwrap();
        assert_person_doors_refused(
            &fixture,
            &CallerAuthority::Person { caller: foreign },
            &peer,
            invocation,
            "foreign devices",
        );
        assert!(
            fixture
                .owner
                .read_direct_conversation(
                    fixture.room,
                    &peer,
                    &local_authority,
                    DirectConversationPage::default(),
                    fixture.now,
                )
                .is_ok()
        );
        assert!(
            fixture
                .owner
                .subscribe_direct_conversation(fixture.room, &peer, &local_authority, fixture.now,)
                .is_ok()
        );
        assert!(
            fixture
                .owner
                .send_direct_message(
                    fixture.room,
                    &peer,
                    &local_authority,
                    DirectMessageSend {
                        invocation,
                        text: "retained private text".into()
                    },
                    fixture.now,
                )
                .is_ok()
        );
        assert_eq!(before, std::fs::read(&fixture.path).unwrap());
    }

    #[test]
    fn delegated_person_certificate_cannot_use_person_conversation_doors() {
        use tilo_relay_protocol::delegation::{Delegate, DelegateScope, Delegation};
        let fixture = Fixture::new();
        let local = fixture.member(77, MemberRole::Editor);
        let invocation = uuid::Uuid::new_v4();
        fixture
            .send(&local, invocation, "retained private text")
            .unwrap();
        let root = SigningKey::from_bytes(&[62; 32]);
        let mut delegated =
            crate::testing::certificate(&root, &SigningKey::from_bytes(&[81; 32]), "Task delegate");
        delegated.capabilities = tilo_user_identity::DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        };
        tilo_user_identity::sign_device_cert(&mut delegated, &root).unwrap();
        let ns = crate::testing::ns(fixture.now).unwrap();
        let delegation = Delegation::sign(
            &root,
            delegated.device_public_key,
            Delegate::Agent {
                agent_id: "scribe".into(),
            },
            "Task delegate",
            [1, 2, 3],
            DelegateScope {
                rooms: vec![fixture.room.to_string()],
                apps: vec![],
            },
            ns.saturating_sub(1),
            ns + 60_000_000_000,
        )
        .unwrap();
        fixture.mutate(|state| {
            assert!(
                state
                    .governance_mut()
                    .admit_agent(delegated.clone(), delegation, &fixture.key, fixture.now)
                    .unwrap()
            );
            assert!(
                state
                    .permissions(delegated.device_public_key, fixture.now)
                    .unwrap()
                    .delegate
                    .is_some()
            );
        });
        assert_person_doors_refused(
            &fixture,
            &CallerAuthority::Person { caller: delegated },
            &key(local.user_public_key),
            invocation,
            "delegated agents",
        );
    }

    #[test]
    fn removed_endpoint_blocks_reads_open_sends_and_saved_retries() {
        let fixture = Fixture::new();
        let peer = fixture.member(77, MemberRole::Editor);
        let invocation = uuid::Uuid::new_v4();
        fixture.send(&peer, invocation, "before removal").unwrap();
        fixture.mutate(|state| {
            assert!(
                state
                    .governance_mut()
                    .remove_member(peer.user_public_key, &fixture.key, fixture.now)
                    .unwrap()
            );
        });
        assert!(fixture.send(&peer, invocation, "before removal").is_err());
        assert!(
            fixture
                .owner
                .subscribe_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    fixture.now
                )
                .is_err()
        );
        assert!(
            fixture
                .owner
                .open_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    fixture.now
                )
                .is_err()
        );
        assert!(
            fixture
                .owner
                .read_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    DirectConversationPage::default(),
                    fixture.now
                )
                .is_err()
        );
        assert!(
            fixture
                .owner
                .direct_contacts(
                    fixture.room,
                    &CallerAuthority::Person { caller: peer },
                    fixture.now
                )
                .is_err()
        );
    }

    #[test]
    fn verified_device_revocation_and_changed_certificate_block_every_door() {
        let mut fixture = Fixture::new();
        let peer = fixture.member(83, MemberRole::Editor);
        // A live, authenticated sibling publishes its root's signed revocation.
        // It expires before the door checks, so it cannot keep the peer active.
        let expires = crate::testing::ns(fixture.now).unwrap() + 1_000_000_000;
        let publisher = fixture.member_device(83, 86, MemberRole::Editor, Some(expires));
        let invocation = uuid::Uuid::new_v4();
        fixture
            .send(&peer, invocation, "before revocation")
            .unwrap();
        let mut changed = peer.clone();
        changed.device_label = "not current evidence".into();
        let forged = CallerAuthority::Person { caller: changed };
        assert!(
            fixture
                .owner
                .subscribe_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &forged,
                    fixture.now
                )
                .is_err()
        );
        assert!(
            fixture
                .owner
                .direct_contacts(fixture.room, &forged, fixture.now)
                .is_err()
        );
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: peer.user_public_key,
            revoked_device_public_key: peer.device_public_key,
            revoked_at_ns: crate::testing::ns(fixture.now).unwrap(),
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[83; 32]))
            .unwrap();
        fixture.mutate(|state| {
            state
                .receive_from(
                    tilo_peer_state::TransportOrigin::UntrustedPresence(
                        publisher.device_public_key,
                    ),
                    neighbour_wire::NeighbourMsg::IdentityHello {
                        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                        cert: Box::new(publisher.clone()),
                        revocations: vec![entry],
                        successions: Vec::new(),
                    },
                    fixture.now,
                )
                .unwrap();
        });
        fixture.now += std::time::Duration::from_secs(2);
        assert!(
            fixture
                .send(&peer, invocation, "before revocation")
                .is_err()
        );
        assert!(
            fixture
                .owner
                .open_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    fixture.now
                )
                .is_err()
        );
        assert!(
            fixture
                .owner
                .read_direct_conversation(
                    fixture.room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    DirectConversationPage::default(),
                    fixture.now
                )
                .is_err()
        );
        assert!(
            !fixture
                .owner
                .direct_contacts(fixture.room, &fixture.person(), fixture.now)
                .unwrap()
                .iter()
                .any(|c| c.public_key == key(peer.user_public_key))
        );
        assert!(
            fixture
                .owner
                .direct_contacts(
                    fixture.room,
                    &CallerAuthority::Person { caller: peer },
                    fixture.now
                )
                .is_err()
        );
    }

    #[test]
    fn ordinary_joined_replica_cannot_become_a_private_host() {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let owner = crate::open_file(&directory.path().join("member.json")).unwrap();
        let (intent, bootstrap, _, _, now) = crate::testing::raw_fixture();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        owner
            .install(
                &crate::testing::finalize(intent, pending, bootstrap).unwrap(),
                now,
            )
            .unwrap();
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let authority = owner
            .with_current(room, |v| {
                Ok(CallerAuthority::Person {
                    caller: v.device().clone(),
                })
            })
            .unwrap();
        assert!(owner.direct_contacts(room, &authority, now).is_err());
    }

    #[test]
    fn failed_store_never_returns_saved_or_installs_private_text() {
        use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
        let fixture = Fixture::new();
        let peer = fixture.member(79, MemberRole::Editor);
        let before = fixture
            .owner
            .with_current(fixture.room, |v| {
                serde_json::to_vec(&v.record.private_direct_conversations)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert!(
            with_failure(Checkpoint::BeforePublish, || fixture.send(
                &peer,
                uuid::Uuid::new_v4(),
                "failed-store-body"
            ))
            .is_err()
        );
        let after = fixture
            .owner
            .with_current(fixture.room, |v| {
                serde_json::to_vec(&v.record.private_direct_conversations)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(before, after);
        assert!(
            !std::fs::read_to_string(&fixture.path)
                .unwrap()
                .contains("failed-store-body")
        );
    }

    #[test]
    fn full_owner_store_refuses_new_mail_and_pairs_without_evicting_history() {
        let fixture = Fixture::new();
        let peer = fixture.member(85, MemberRole::Editor);
        let (id, people) = fixture
            .owner
            .with_current(fixture.room, |v| {
                authorized_pair(
                    v,
                    fixture.room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    fixture.now,
                )
            })
            .unwrap();
        let turn = fixture.owner.0.mutation.lock();
        let mut candidate = fixture
            .owner
            .with_current(fixture.room, |v| Ok(v.clone()))
            .unwrap();
        let store = &mut candidate.record.private_direct_conversations;
        ensure_pair(store, fixture.room, &id, people).unwrap();
        for index in 0..MAX_MESSAGES {
            let uuid = uuid::Uuid::from_u128(index as u128 + 1);
            let mid = message_id(&id, uuid);
            store
                .pairs
                .get_mut(&id)
                .unwrap()
                .messages
                .push(DirectMessage {
                    id: mid.clone(),
                    sender: key(fixture.governor.user_public_key),
                    sequence: index as u64 + 1,
                    received_at: 1,
                    text: "retained private history".into(),
                });
            store.invocations.insert(
                uuid,
                Invocation {
                    conversation: id.clone(),
                    source: fixture.governor.device_public_key,
                    body_hash: *blake3::hash(b"retained private history").as_bytes(),
                    message_id: mid,
                },
            );
        }
        for index in 1..MAX_CONVERSATIONS {
            let mut people = [
                key(fixture.governor.user_public_key),
                format!("{index:064x}"),
            ];
            people.sort();
            let id = pair_id(fixture.room, &people).unwrap();
            ensure_pair(store, fixture.room, &id, people).unwrap();
        }
        store.validate().unwrap();
        candidate.record.generation += 1;
        fixture
            .owner
            .publish_room(fixture.room, candidate, None)
            .unwrap();
        drop(turn);
        assert!(
            fixture
                .send(&peer, uuid::Uuid::new_v4(), "overflow")
                .is_err()
        );
        let another = fixture.member(87, MemberRole::Editor);
        assert!(
            fixture
                .owner
                .open_direct_conversation(
                    fixture.room,
                    &key(another.user_public_key),
                    &fixture.person(),
                    fixture.now
                )
                .is_err()
        );
        let page = fixture
            .owner
            .read_direct_conversation(
                fixture.room,
                &key(peer.user_public_key),
                &fixture.person(),
                DirectConversationPage {
                    after_sequence: MAX_MESSAGES as u64 - 1,
                    limit: 1,
                },
                fixture.now,
            )
            .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].text, "retained private history");
        assert_eq!(page.messages[0].sequence, MAX_MESSAGES as u64);
    }

    #[test]
    fn utf8_pages_source_uuid_and_workspace_ids_are_bounded() {
        let fixture = Fixture::new();
        let peer = fixture.member(81, MemberRole::Editor);
        let invocation = uuid::Uuid::new_v4();
        let saved = fixture.send(&peer, invocation, "first").unwrap();
        let reverse = CallerAuthority::Person {
            caller: peer.clone(),
        };
        assert!(
            fixture
                .owner
                .send_direct_message(
                    fixture.room,
                    &key(fixture.governor.user_public_key),
                    &reverse,
                    DirectMessageSend {
                        invocation,
                        text: "first".into()
                    },
                    fixture.now
                )
                .is_err()
        );
        fixture.send(&peer, uuid::Uuid::new_v4(), "second").unwrap();
        assert!(
            fixture
                .send(
                    &peer,
                    uuid::Uuid::new_v4(),
                    &"é".repeat(MAX_TEXT_BYTES / 2 + 1)
                )
                .is_err()
        );
        assert!(fixture.send(&peer, uuid::Uuid::nil(), "text").is_err());
        assert!(fixture.send(&peer, uuid::Uuid::new_v4(), " \n ").is_err());
        let page = fixture
            .owner
            .read_direct_conversation(
                fixture.room,
                &key(peer.user_public_key),
                &fixture.person(),
                DirectConversationPage {
                    after_sequence: 0,
                    limit: 1,
                },
                fixture.now,
            )
            .unwrap();
        assert!(page.has_more);
        assert_eq!(page.messages, vec![saved.message]);
        for limit in [0, MAX_PAGE + 1] {
            assert!(
                fixture
                    .owner
                    .read_direct_conversation(
                        fixture.room,
                        &key(peer.user_public_key),
                        &fixture.person(),
                        DirectConversationPage {
                            after_sequence: 0,
                            limit
                        },
                        fixture.now
                    )
                    .is_err()
            );
        }
        let other_room = RoomId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let mut people = [
            key(fixture.governor.user_public_key),
            key(peer.user_public_key),
        ];
        people.sort();
        let other = pair_id(other_room, &people).unwrap();
        assert_ne!(saved.conversation_id, other);
        assert_ne!(
            message_id(&saved.conversation_id, invocation),
            message_id(&other, invocation)
        );
        assert!(
            fixture
                .owner
                .read_direct_conversation(
                    other_room,
                    &key(peer.user_public_key),
                    &fixture.person(),
                    DirectConversationPage::default(),
                    fixture.now
                )
                .is_err()
        );
    }
}
