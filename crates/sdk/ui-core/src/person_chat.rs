//! Private person conversations on the authenticated native Chat channel.
//! Reads on the UI sync path use a bounded, revocable local snapshot. There is
//! no standalone store, room-document fallback, or model-facing host tool.
use crate::time::{Duration, Instant};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    pub public_key: String,
    pub display_name: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: String,
    pub author: String,
    pub text: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    pub peer: String,
    #[serde(deserialize_with = "bounded_messages")]
    pub messages: Vec<Message>,
}

thread_local! { static ORIGINAL: Cell<Option<uuid::Uuid>> = const { Cell::new(None) }; }

/// Retain the effect context's original invocation around these SDK calls.
/// This is inert correlation, not authority: the host looks up original custody
/// and rejects Agent/model/native-task invokers even in a person's app worker.
pub fn with_invocation<R>(
    invocation: Option<tilo_actions::InvocationId>,
    call: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<uuid::Uuid>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ORIGINAL.set(self.0);
        }
    }
    let _restore = Restore(ORIGINAL.replace(invocation.map(|id| id.0)));
    call()
}

#[derive(Default)]
struct Cache {
    generation: u64,
    identity: u64,
    contacts: Option<(Instant, Vec<Contact>)>,
    conversations: BTreeMap<String, (Instant, Conversation)>,
    pending: BTreeMap<String, Instant>,
    attempts: BTreeMap<String, Instant>,
    active: BTreeMap<String, ActiveConversation>,
}
struct ActiveConversation {
    owners: usize,
    token: uuid::Uuid,
    // Dropping the sender interrupts even a currently blocked stream.next.
    _cancel: tokio::sync::watch::Sender<()>,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

// Enforce the element bound while decoding, before growing a reply-controlled Vec.
fn bounded_messages<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Message>, D::Error> {
    struct Messages;
    impl<'de> serde::de::Visitor<'de> for Messages {
        type Value = Vec<Message>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("at most 2000 private messages")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            if sequence.size_hint().is_some_and(|length| length > 2000) {
                return Err(serde::de::Error::custom(
                    "private message count exceeds bound",
                ));
            }
            let mut messages = Vec::new();
            while let Some(message) = sequence.next_element::<Message>()? {
                if messages.len() == 2000 {
                    return Err(serde::de::Error::custom(
                        "private message count exceeds bound",
                    ));
                }
                messages.push(message);
            }
            Ok(messages)
        }
    }
    deserializer.deserialize_seq(Messages)
}

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}
fn current(value: &mut Cache) {
    let identity = crate::local_identity::generation();
    if value.identity != identity {
        let generation = value.generation.wrapping_add(1);
        *value = Cache {
            generation,
            identity,
            ..Cache::default()
        };
    }
}
/// Drop the cached contacts: the broker calls this when the identity or
/// connection changes. Only the broker transport and tests change them.
#[cfg(any(test, feature = "broker-transport"))]
pub(crate) fn invalidate() {
    if let Ok(mut value) = cache().lock() {
        let generation = value.generation.wrapping_add(1);
        *value = Cache {
            generation,
            identity: crate::local_identity::generation(),
            ..Cache::default()
        };
    }
}
fn original() -> Result<Option<uuid::Uuid>, String> {
    let original = ORIGINAL.get();
    if crate::invocation_scope::current_invoker().is_some() && original.is_none() {
        return Err("private conversation requires original person invocation custody".into());
    }
    Ok(original)
}
fn request(value: serde_json::Value, original: Option<uuid::Uuid>) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(&value).map_err(|_| "invalid private conversation request")?;
    #[cfg(feature = "broker-transport")]
    {
        crate::collaboration::person_chat_command(
            bytes,
            original.map_or_else(Vec::new, |id| id.as_bytes().to_vec()),
        )
    }
    #[cfg(not(feature = "broker-transport"))]
    {
        let _ = (bytes, original);
        Err("private person conversations require an authenticated native host".into())
    }
}

fn load_contacts_once() {
    let key = String::new();
    let Ok(mut value) = cache().lock() else {
        return;
    };
    current(&mut value);
    if value
        .pending
        .get(&key)
        .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
    {
        return;
    }
    // UI sync is not a refresh trigger. In particular, the completion wake
    // must not cause another RPC after a timeout, refusal or expired snapshot.
    if value.attempts.contains_key(&key) || value.pending.len() >= 8 {
        return;
    }
    let generation = value.generation;
    if value.attempts.len() >= 32 {
        value.attempts.clear();
    }
    value.attempts.insert(key.clone(), crate::time::now());
    value.pending.insert(key.clone(), crate::time::now());
    drop(value);
    let failed_key = key.clone();
    let wake = crate::WakeHandle::new();
    if crate::background::Background::current()
        .spawn("person-chat-snapshot", move || {
            let reply = request(serde_json::json!({"operation":"contacts"}), None);
            let Ok(mut value) = cache().lock() else {
                return;
            };
            current(&mut value);
            if value.generation != generation {
                return;
            }
            value.pending.remove(&key);
            value.contacts = reply
                .ok()
                .filter(|bytes| bytes.len() <= 1024 * 1024)
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .map(|contacts| (crate::time::now(), contacts));
            drop(value);
            wake.wake();
        })
        .is_err()
        && let Ok(mut value) = cache().lock()
    {
        value.pending.remove(&failed_key);
    }
}

/// Current workspace's certified public keys; unavailable hosts answer no contacts.
/// This function never waits on a network or RPC reply. It attempts one initial
/// load per security-context invalidation, not a timer-based refresh.
pub fn contacts() -> Vec<Contact> {
    if original().is_err() {
        return Vec::new();
    }
    load_contacts_once();
    contacts_snapshot()
}

/// Public membership keys from the last authorized native snapshot.
/// Semantic row resolution may inspect these while an invocation is being
/// decoded, before its private effect context exists. This starts no RPC and
/// exposes no conversation bodies or authority to open/read a conversation.
pub fn contacts_snapshot() -> Vec<Contact> {
    let Ok(mut value) = cache().lock() else {
        return Vec::new();
    };
    current(&mut value);
    value
        .contacts
        .as_ref()
        .map_or_else(Vec::new, |(_, contacts)| contacts.clone())
}

fn conversation(command: serde_json::Value, peer: &str) -> Result<Conversation, String> {
    let generation = {
        let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
        current(&mut value);
        value.generation
    };
    // A refused operation on one peer must not tear down other selected
    // conversations. Security-context replacement separately invalidates all
    // lanes; stream failures revoke only their bound peer.
    let bytes = request(command, original()?)?;
    if bytes.len() > 1024 * 1024 {
        return Err("private conversation reply exceeds bound".into());
    }
    let conversation: Conversation =
        serde_json::from_slice(&bytes).map_err(|_| "invalid private conversation reply")?;
    if conversation.peer != peer {
        return Err("private conversation peer binding changed".into());
    }
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    if value.generation != generation {
        return Err("private conversation channel was replaced".into());
    }
    // Only selected views retain private bodies. Sending or explicitly reading
    // another peer must not evict any open floating conversation.
    if value.active.contains_key(peer) {
        value
            .conversations
            .insert(peer.to_owned(), (crate::time::now(), conversation.clone()));
    }
    Ok(conversation)
}

fn start_subscription(peer: &str, original: Option<uuid::Uuid>) -> Result<uuid::Uuid, String> {
    let token = uuid::Uuid::new_v4();
    let (cancel, receiver) = tokio::sync::watch::channel(());
    {
        let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
        current(&mut value);
        if let Some(active) = value.active.get_mut(peer) {
            active.owners += 1;
            return Ok(active.token);
        }
        if value.active.len() >= 16 {
            return Err("too many open private conversations".into());
        }
        value.active.insert(
            peer.to_owned(),
            ActiveConversation {
                owners: 1,
                token,
                _cancel: cancel,
            },
        );
    }
    #[cfg(feature = "broker-transport")]
    let result = crate::collaboration::subscribe_person_chat(
        peer.to_owned(),
        original.map_or_else(Vec::new, |id| id.as_bytes().to_vec()),
        token,
        receiver,
    );
    #[cfg(not(feature = "broker-transport"))]
    let result = {
        let _ = (original, receiver);
        Err("private subscriptions require an authenticated native host".to_owned())
    };
    if result.is_err() {
        subscription_failed(token, peer);
    }
    result.map(|()| token)
}

/// Identifies the lane acquired by a successful open. Views keep this token so
/// a close after security-context replacement cannot release a newer owner.
pub fn subscription_token(public_key: &str) -> Option<uuid::Uuid> {
    let mut value = cache().lock().ok()?;
    current(&mut value);
    value.active.get(public_key).map(|active| active.token)
}

/// Retain only the selected lane, not a replacement opened after revocation.
pub fn retain_current(public_key: &str, token: uuid::Uuid) -> bool {
    let Ok(mut value) = cache().lock() else {
        return false;
    };
    current(&mut value);
    let Some(active) = value
        .active
        .get_mut(public_key)
        .filter(|active| active.token == token)
    else {
        return false;
    };
    active.owners += 1;
    true
}

/// Release a view without affecting a replacement lane for the same peer.
pub fn close_current(public_key: &str, token: uuid::Uuid) {
    release(public_key, Some(token));
}

/// Retain an already selected view when creating another presentation of it.
/// This never opens a channel or polls the host.
pub fn retain(public_key: &str) -> bool {
    let Ok(mut value) = cache().lock() else {
        return false;
    };
    current(&mut value);
    let Some(active) = value.active.get_mut(public_key) else {
        return false;
    };
    active.owners += 1;
    true
}

/// Release one selected view. Only the last owner cancels the live stream and
/// forgets its private snapshot; closing a float cannot close its parent.
pub fn close(public_key: &str) {
    release(public_key, None);
}

fn release(public_key: &str, token: Option<uuid::Uuid>) {
    if let Ok(mut value) = cache().lock() {
        current(&mut value);
        if let Some(active) = value.active.get_mut(public_key) {
            if token.is_some_and(|token| active.token != token) {
                return;
            }
            active.owners -= 1;
            if active.owners != 0 {
                return;
            }
            value.active.remove(public_key);
        }
        value.conversations.remove(public_key);
    }
}

#[cfg(feature = "broker-transport")]
pub(crate) fn subscription_current(token: uuid::Uuid, peer: &str) -> bool {
    let Ok(mut value) = cache().lock() else {
        return false;
    };
    current(&mut value);
    value
        .active
        .get(peer)
        .is_some_and(|active| active.token == token)
}

pub(crate) fn subscription_failed(token: uuid::Uuid, peer: &str) {
    if let Ok(mut value) = cache().lock() {
        current(&mut value);
        if value
            .active
            .get(peer)
            .is_some_and(|active| active.token == token)
        {
            value.active.remove(peer);
            value.conversations.remove(peer);
            drop(value);
            crate::WakeHandle::new().wake();
        }
    }
}

#[cfg(feature = "broker-transport")]
pub(crate) fn subscription_snapshot(
    token: uuid::Uuid,
    peer: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if bytes.len() > 1024 * 1024 {
        return Err("private conversation reply exceeds bound".into());
    }
    let snapshot: Conversation =
        serde_json::from_slice(bytes).map_err(|_| "invalid private conversation snapshot")?;
    if snapshot.peer != peer {
        return Err("private conversation peer binding changed".into());
    }
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    if value
        .active
        .get(peer)
        .is_none_or(|active| active.token != token)
    {
        return Ok(());
    }
    // An already accepted send may have overtaken this refresh.
    if value
        .conversations
        .get(peer)
        .is_none_or(|(_, old)| old.messages.len() <= snapshot.messages.len())
    {
        value
            .conversations
            .insert(peer.to_owned(), (crate::time::now(), snapshot));
    }
    drop(value);
    crate::WakeHandle::new().wake();
    Ok(())
}

/// Open only after accepted private owner persistence; no optimistic success.
pub fn open(public_key: &str) -> Result<Conversation, String> {
    let original = original()?;
    let snapshot = conversation(
        serde_json::json!({"operation":"open","public_key":public_key}),
        public_key,
    )?;
    let token = start_subscription(public_key, original)?;
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    if value
        .active
        .get(public_key)
        .is_none_or(|active| active.token != token)
    {
        return Err("private conversation channel was replaced".into());
    }
    // A stream may already have delivered a newer snapshot.
    value
        .conversations
        .entry(public_key.to_owned())
        .or_insert_with(|| (crate::time::now(), snapshot.clone()));
    Ok(snapshot)
}
/// UI sync reads a local snapshot. Explicit semantic invocations are checked
/// against retained original host custody before returning any private body.
pub fn read(public_key: &str) -> Result<Conversation, String> {
    if original()?.is_some() {
        return conversation(
            serde_json::json!({"operation":"read","public_key":public_key}),
            public_key,
        );
    }
    // Only explicit open/send/semantic read may hydrate private bodies. Passive
    // UI sync never opens a conversation or polls an already opened one.
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    value
        .conversations
        .get(public_key)
        .map(|(_, conversation)| conversation.clone())
        .ok_or_else(|| "private conversation snapshot unavailable".into())
}
/// Success means accepted owner persistence. An uncertain reply is an error,
/// never an optimistic acknowledgement.
pub fn send(public_key: &str, text: &str) -> Result<Conversation, String> {
    if text.trim().is_empty() || text.len() > 16384 {
        return Err("invalid private message text".into());
    }
    let invocation = original()?.unwrap_or_else(uuid::Uuid::new_v4);
    conversation(
        serde_json::json!({"operation":"send","public_key":public_key,"text":text,"invocation":invocation}),
        public_key,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_host_never_fabricates_a_ready_conversation() {
        let _guard = tilo_test_env::lock();
        invalidate();
        assert!(open(&"01".repeat(32)).is_err());
        assert!(read(&"01".repeat(32)).is_err());
        assert!(send(&"01".repeat(32), "hello").is_err());
        assert!(send(&"01".repeat(32), "  ").is_err());
    }
    #[test]
    fn ui_sync_does_not_retry_contacts_or_hydrate_private_bodies() {
        let _guard = tilo_test_env::lock();
        invalidate();
        let attempted = crate::time::now() - Duration::from_secs(10);
        cache()
            .lock()
            .unwrap()
            .attempts
            .insert(String::new(), attempted);
        for _ in 0..10 {
            assert!(contacts().is_empty());
            assert!(read(&"01".repeat(32)).is_err());
        }
        let value = cache().lock().unwrap();
        assert_eq!(value.attempts.get(""), Some(&attempted));
        assert!(value.pending.is_empty());
        assert!(value.conversations.is_empty());
    }
    #[test]
    fn open_snapshot_stays_valid_until_close_or_security_invalidation() {
        let _guard = tilo_test_env::lock();
        invalidate();
        let token = uuid::Uuid::new_v4();
        let (cancel, stopped) = tokio::sync::watch::channel(());
        {
            let mut value = cache().lock().unwrap();
            value.active.insert(
                "peer".into(),
                ActiveConversation {
                    owners: 1,
                    token,
                    _cancel: cancel,
                },
            );
            value.conversations.insert(
                "peer".into(),
                (
                    crate::time::now() - Duration::from_secs(60),
                    Conversation {
                        peer: "peer".into(),
                        messages: Vec::new(),
                    },
                ),
            );
        }
        for _ in 0..10 {
            assert!(read("peer").is_ok());
        }
        assert!(stopped.has_changed().is_ok());
        close("another-peer");
        assert!(read("peer").is_ok());
        // Floating clones the selected view, then its parent leaves the route.
        assert!(retain("peer"));
        close("peer");
        assert!(stopped.has_changed().is_ok());
        assert!(read("peer").is_ok());
        // Reopening this peer and dropping a returned float is balanced too.
        start_subscription("peer", None).unwrap();
        close("peer");
        assert!(stopped.has_changed().is_ok());
        assert!(read("peer").is_ok());
        close("peer");
        assert!(stopped.has_changed().is_err());
        assert!(read("peer").is_err());
        // A context change also drops a blocked lane and clears its bodies.
        let (cancel, stopped) = tokio::sync::watch::channel(());
        cache().lock().unwrap().active.insert(
            "peer".into(),
            ActiveConversation {
                owners: 1,
                token,
                _cancel: cancel,
            },
        );
        invalidate();
        assert!(stopped.has_changed().is_err());
        assert!(read("peer").is_err());
    }

    #[test]
    #[cfg(feature = "broker-transport")]
    fn late_subscription_reply_cannot_restore_a_closed_or_switched_peer() {
        let _guard = tilo_test_env::lock();
        invalidate();
        let old = uuid::Uuid::new_v4();
        let current = uuid::Uuid::new_v4();
        let (cancel, _receiver) = tokio::sync::watch::channel(());
        cache().lock().unwrap().active.insert(
            "new-peer".into(),
            ActiveConversation {
                owners: 1,
                token: current,
                _cancel: cancel,
            },
        );
        let bytes = br#"{"peer":"old-peer","messages":[]}"#;
        subscription_snapshot(old, "old-peer", bytes).unwrap();
        subscription_failed(old, "old-peer");
        assert!(subscription_current(current, "new-peer"));
        // An older view of this very peer must not release or retain the new
        // lane after a host/security-context replacement.
        close_current("new-peer", old);
        assert!(!retain_current("new-peer", old));
        assert!(subscription_current(current, "new-peer"));
        assert!(read("old-peer").is_err());
        close("old-peer");
        assert!(subscription_current(current, "new-peer"));
        invalidate();
        assert!(!subscription_current(current, "new-peer"));
    }

    #[test]
    fn selected_peers_have_independent_owners_and_cancellation() {
        let _guard = tilo_test_env::lock();
        invalidate();
        let mut receivers = Vec::new();
        for peer in ["first", "second"] {
            let (cancel, receiver) = tokio::sync::watch::channel(());
            receivers.push(receiver);
            let mut value = cache().lock().unwrap();
            value.active.insert(
                peer.into(),
                ActiveConversation {
                    owners: 1,
                    token: uuid::Uuid::new_v4(),
                    _cancel: cancel,
                },
            );
            value.conversations.insert(
                peer.into(),
                (
                    crate::time::now(),
                    Conversation {
                        peer: peer.into(),
                        messages: Vec::new(),
                    },
                ),
            );
        }
        assert!(retain("first"));
        close("first");
        assert!(
            receivers
                .iter()
                .all(|receiver| receiver.has_changed().is_ok())
        );
        assert!(read("first").is_ok());
        assert!(read("second").is_ok());
        close("first");
        assert!(receivers[0].has_changed().is_err());
        assert!(receivers[1].has_changed().is_ok());
        assert!(read("first").is_err());
        assert!(read("second").is_ok());
        close("second");
        assert!(receivers[1].has_changed().is_err());
        assert!(read("second").is_err());
        assert!(!retain("second"));
    }

    #[test]
    fn oversized_message_array_is_refused_during_decode() {
        let message = serde_json::json!({"id":"id", "author":"peer", "text":"body"});
        let reply = serde_json::json!({"peer":"peer", "messages":vec![message; 2001]});
        assert!(serde_json::from_value::<Conversation>(reply).is_err());
    }
    #[test]
    fn original_correlation_is_scoped_and_restored() {
        assert!(ORIGINAL.get().is_none());
        let id = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        with_invocation(Some(id), || assert_eq!(ORIGINAL.get(), Some(id.0)));
        assert!(ORIGINAL.get().is_none());
    }
}
