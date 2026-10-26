//! Private person conversations, kept by the room's governor and reached
//! through the host: the native broker's authenticated Chat channel, or the
//! page's own session with the room ([`install_transport`]). Reads on the UI
//! sync path use a bounded, revocable local snapshot; an operation answers
//! with a [`Pending`] reply that a page fills later, never by waiting on the
//! UI thread. There is no standalone store, room-document fallback, or
//! model-facing host tool.
use crate::time::{Duration, Instant};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// What a private operation asks of the host: the closed vocabulary the
/// native host's person door takes, in the same JSON.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum Request {
    /// The people of the room this person may talk to privately.
    Contacts,
    /// Open the conversation with `public_key`, then the whole of it.
    Open { public_key: String },
    /// The whole conversation with `public_key`.
    Read { public_key: String },
    /// Send `text` to `public_key` once (`invocation` makes a retry the same
    /// message), then the whole conversation.
    Send {
        public_key: String,
        text: String,
        invocation: uuid::Uuid,
    },
}

/// What the host answered a [`Request`].
pub enum Answer {
    Contacts(Vec<Contact>),
    Conversation(Conversation),
}

/// Called once with what the host answered.
pub type AnswerDone = Box<dyn FnOnce(Result<Answer, String>) + Send>;
/// Called once with the conversation's sequence after a watch.
pub type WatchDone = Box<dyn FnOnce(Result<u64, String>) + Send>;

/// A host's door to the room governor's private conversations, for a
/// process without the native broker: the page installs its session with
/// the room ([`install_transport`]). It never blocks; it answers through
/// the callbacks, on the thread that runs the app.
pub trait Transport: Send + Sync {
    /// Carry out `request` as the authenticated device of this process.
    fn call(&self, request: Request, done: AnswerDone);
    /// Wait until the conversation with `peer` passes sequence `after`, or
    /// a while: `done` gets the sequence then (`after` when nothing changed).
    fn watch(&self, peer: String, after: u64, done: WatchDone);
}

#[cfg(not(feature = "broker-transport"))]
static TRANSPORT: std::sync::RwLock<Option<Arc<dyn Transport>>> = std::sync::RwLock::new(None);

/// Make `transport` this process's door to private conversations, or take
/// it away with `None`. Whatever was cached or open through the previous
/// one is dropped.
#[cfg(not(feature = "broker-transport"))]
pub fn install_transport(transport: Option<Arc<dyn Transport>>) {
    use tilo_lock::RwLockRecover as _;
    *TRANSPORT.write_recover() = transport;
    invalidate();
    wake_all();
}

#[cfg(not(feature = "broker-transport"))]
fn transport() -> Option<Arc<dyn Transport>> {
    use tilo_lock::RwLockRecover as _;
    TRANSPORT.read_recover().clone()
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

/// A private operation's answer, which may come later: a page asks the
/// room over the network, and the UI thread never waits for it. Poll it
/// with [`Self::take`] from sync (its arrival wakes the app), or await it.
pub struct Pending<T> {
    slot: Arc<Mutex<Slot<T>>>,
}
struct Slot<T> {
    value: Option<Result<T, String>>,
    waker: Option<std::task::Waker>,
}
/// Fills a [`Pending`] once; dropped unfilled, it fails it.
struct Fill<T> {
    slot: Option<Arc<Mutex<Slot<T>>>>,
}

impl<T> Pending<T> {
    fn new() -> (Self, Fill<T>) {
        let slot = Arc::new(Mutex::new(Slot {
            value: None,
            waker: None,
        }));
        (Self { slot: slot.clone() }, Fill { slot: Some(slot) })
    }

    fn ready(value: Result<T, String>) -> Self {
        let (pending, fill) = Self::new();
        fill.fill(value);
        pending
    }

    /// The answer, once it is here; taken, it is gone.
    pub fn take(&self) -> Option<Result<T, String>> {
        self.slot.lock().ok()?.value.take()
    }
}

impl<T> Fill<T> {
    fn fill(mut self, value: Result<T, String>) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let waker = {
            let Ok(mut slot) = slot.lock() else {
                return;
            };
            slot.value = Some(value);
            slot.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        wake_all();
    }
}

impl<T> Drop for Fill<T> {
    fn drop(&mut self) {
        if self.slot.is_some() {
            Fill {
                slot: self.slot.take(),
            }
            .fill(Err("private conversation reply lost".into()));
        }
    }
}

impl<T> std::future::Future for Pending<T> {
    type Output = Result<T, String>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let Ok(mut slot) = self.slot.lock() else {
            return std::task::Poll::Ready(Err("private snapshot unavailable".into()));
        };
        if let Some(value) = slot.value.take() {
            return std::task::Poll::Ready(value);
        }
        slot.waker = Some(cx.waker().clone());
        std::task::Poll::Pending
    }
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
    /// The apps that read these snapshots, woken when one changes.
    readers: Vec<crate::WakeHandle>,
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
        let readers = std::mem::take(&mut value.readers);
        *value = Cache {
            generation,
            identity,
            readers,
            ..Cache::default()
        };
    }
}
/// Remember the calling app as a reader of these snapshots.
fn register_reader(value: &mut Cache) {
    let wake = crate::WakeHandle::new();
    value.readers.retain(crate::WakeHandle::is_active);
    if !value
        .readers
        .iter()
        .any(|reader| reader.same_open_scope(&wake))
        && value.readers.len() < 16
    {
        value.readers.push(wake);
    }
}
/// Wake every app reading these snapshots, and the caller's.
fn wake_all() {
    let readers = cache()
        .lock()
        .map(|value| value.readers.clone())
        .unwrap_or_default();
    for reader in readers {
        reader.wake();
    }
    crate::WakeHandle::new().wake();
}
/// Drop the cached contacts and conversations: the host calls this when
/// the identity or connection changes.
pub(crate) fn invalidate() {
    if let Ok(mut value) = cache().lock() {
        let generation = value.generation.wrapping_add(1);
        let readers = std::mem::take(&mut value.readers);
        *value = Cache {
            generation,
            identity: crate::local_identity::generation(),
            readers,
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

/// Ask the host `request` and hand `done` the answer: at once from the
/// native broker (which waits, bounded, off nothing but this thread), later
/// from a page's transport.
fn exchange(
    request: Request,
    original: Option<uuid::Uuid>,
    done: impl FnOnce(Result<Answer, String>) + Send + 'static,
) {
    #[cfg(feature = "broker-transport")]
    {
        let contacts = matches!(request, Request::Contacts);
        let answer = serde_json::to_vec(&request)
            .map_err(|_| "invalid private conversation request".to_owned())
            .and_then(|bytes| {
                crate::collaboration::person_chat_command(
                    bytes,
                    original.map_or_else(Vec::new, |id| id.as_bytes().to_vec()),
                )
            })
            .and_then(|bytes| decode(contacts, &bytes));
        done(answer);
    }
    #[cfg(not(feature = "broker-transport"))]
    {
        // A page has no broker to look up the original invocation's custody:
        // it refuses any invoker that is not the person themself.
        if original.is_some()
            && crate::invocation_scope::current_invoker().is_none_or(|invoker| {
                invoker.kind != tilo_actions::ActorKind::Person || invoker.on_behalf_of.is_some()
            })
        {
            done(Err(
                "private conversations take only the person's own invocations".into(),
            ));
            return;
        }
        match transport() {
            Some(transport) => transport.call(request, Box::new(done)),
            None => done(Err(
                "private person conversations require an authenticated host".into(),
            )),
        }
    }
}

#[cfg(feature = "broker-transport")]
fn decode(contacts: bool, bytes: &[u8]) -> Result<Answer, String> {
    if bytes.len() > 1024 * 1024 {
        return Err("private conversation reply exceeds bound".into());
    }
    if contacts {
        serde_json::from_slice(bytes)
            .map(Answer::Contacts)
            .map_err(|_| "invalid private contacts reply".into())
    } else {
        serde_json::from_slice(bytes)
            .map(Answer::Conversation)
            .map_err(|_| "invalid private conversation reply".into())
    }
}

fn load_contacts_once() {
    let key = String::new();
    let Ok(mut value) = cache().lock() else {
        return;
    };
    current(&mut value);
    register_reader(&mut value);
    if value.pending.get(&key).is_some_and(|at| {
        crate::time::now().saturating_duration_since(*at) < Duration::from_secs(5)
    }) {
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
    let load = move || {
        exchange(Request::Contacts, None, move |reply| {
            let Ok(mut value) = cache().lock() else {
                return;
            };
            current(&mut value);
            if value.generation != generation {
                return;
            }
            value.pending.remove(&key);
            value.contacts = match reply {
                Ok(Answer::Contacts(contacts)) if contacts.len() <= 4096 => {
                    Some((crate::time::now(), contacts))
                }
                _ => None,
            };
            drop(value);
            wake_all();
        });
    };
    // The broker answers in place, waiting: off the UI thread. A page's
    // transport answers later on its own.
    #[cfg(feature = "broker-transport")]
    if crate::background::Background::current()
        .spawn("person-chat-snapshot", load)
        .is_err()
        && let Ok(mut value) = cache().lock()
    {
        value.pending.remove("");
    }
    #[cfg(not(feature = "broker-transport"))]
    load();
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

/// Ask the room for its people again, when someone is there whom the last
/// list did not have: at most once every ten seconds, and the list in hand
/// stays until the new one comes.
pub fn refresh_contacts() {
    let Ok(mut value) = cache().lock() else {
        return;
    };
    current(&mut value);
    let recent = value.attempts.get("").is_some_and(|at| {
        crate::time::now().saturating_duration_since(*at) < Duration::from_secs(10)
    });
    if recent || value.pending.contains_key("") {
        return;
    }
    value.attempts.remove("");
    drop(value);
    load_contacts_once();
}

/// Public membership keys from the last authorized snapshot.
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

/// A conversation answer bound to `peer` and the security context of
/// `generation`, kept for the views that have it open.
fn bound_conversation(
    answer: Result<Answer, String>,
    peer: &str,
    generation: u64,
) -> Result<Conversation, String> {
    let Answer::Conversation(conversation) = answer? else {
        return Err("invalid private conversation reply".into());
    };
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

fn generation() -> Result<u64, String> {
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    register_reader(&mut value);
    Ok(value.generation)
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
        let _ = original;
        match transport() {
            Some(transport) => {
                follow(transport, peer.to_owned(), token, 0, receiver);
                Ok(())
            }
            None => Err("private subscriptions require an authenticated host".to_owned()),
        }
    };
    if result.is_err() {
        subscription_failed(token, peer);
    }
    result.map(|()| token)
}

/// Follow the conversation with `peer` through a page's transport: each
/// time its sequence passes `after`, read it whole again into the open
/// lane `token`, until the lane closes or is replaced.
#[cfg(not(feature = "broker-transport"))]
fn follow(
    transport: Arc<dyn Transport>,
    peer: String,
    token: uuid::Uuid,
    after: u64,
    cancel: tokio::sync::watch::Receiver<()>,
) {
    if cancel.has_changed().is_err() || !subscription_current(token, &peer) {
        return;
    }
    let watching = transport.clone();
    watching.watch(
        peer.clone(),
        after,
        Box::new(move |changed| match changed {
            // The poll's time ran out with nothing new: wait again.
            Ok(sequence) if sequence == after => follow(transport, peer, token, after, cancel),
            Ok(sequence) if sequence > after => {
                if cancel.has_changed().is_err() || !subscription_current(token, &peer) {
                    return;
                }
                let reading = transport.clone();
                reading.call(
                    Request::Read {
                        public_key: peer.clone(),
                    },
                    Box::new(move |answer| {
                        let kept = match answer {
                            Ok(Answer::Conversation(snapshot)) => {
                                store_snapshot(token, &peer, snapshot)
                            }
                            _ => Err("invalid private conversation refresh".into()),
                        };
                        match kept {
                            Ok(()) => follow(transport, peer, token, sequence, cancel),
                            Err(_) => subscription_failed(token, &peer),
                        }
                    }),
                );
            }
            // Refused, gone, or a cursor going back: the lane ends.
            _ => subscription_failed(token, &peer),
        }),
    );
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
    register_reader(&mut value);
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
    register_reader(&mut value);
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
            wake_all();
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
    store_snapshot(token, peer, snapshot)
}

/// Keep a refreshed `snapshot` for the open lane `token` of `peer`.
fn store_snapshot(token: uuid::Uuid, peer: &str, snapshot: Conversation) -> Result<(), String> {
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
    wake_all();
    Ok(())
}

/// Open only after accepted private owner persistence; no optimistic success.
/// The answer comes with the lane open ([`subscription_token`]) and its
/// snapshot kept; from the native broker it is here at once.
pub fn open(public_key: &str) -> Pending<Conversation> {
    let original = match original() {
        Ok(original) => original,
        Err(error) => return Pending::ready(Err(error)),
    };
    let generation = match generation() {
        Ok(generation) => generation,
        Err(error) => return Pending::ready(Err(error)),
    };
    let (pending, fill) = Pending::new();
    let peer = public_key.to_owned();
    exchange(
        Request::Open {
            public_key: peer.clone(),
        },
        original,
        move |answer| fill.fill(opened(answer, &peer, generation, original)),
    );
    pending
}

fn opened(
    answer: Result<Answer, String>,
    peer: &str,
    generation: u64,
    original: Option<uuid::Uuid>,
) -> Result<Conversation, String> {
    // A refused operation on one peer must not tear down other selected
    // conversations. Security-context replacement separately invalidates all
    // lanes; stream failures revoke only their bound peer.
    let snapshot = bound_conversation(answer, peer, generation)?;
    let token = start_subscription(peer, original)?;
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    if value
        .active
        .get(peer)
        .is_none_or(|active| active.token != token)
    {
        return Err("private conversation channel was replaced".into());
    }
    // A stream may already have delivered a newer snapshot.
    value
        .conversations
        .entry(peer.to_owned())
        .or_insert_with(|| (crate::time::now(), snapshot.clone()));
    Ok(snapshot)
}

/// The open conversation's local snapshot, for UI sync and passive views.
/// It never opens a conversation or polls the host.
///
/// # Errors
///
/// When no view has the conversation open, or an invocation lacks its
/// original person custody.
pub fn snapshot(public_key: &str) -> Result<Conversation, String> {
    original()?;
    let mut value = cache().lock().map_err(|_| "private snapshot unavailable")?;
    current(&mut value);
    register_reader(&mut value);
    value
        .conversations
        .get(public_key)
        .map(|(_, conversation)| conversation.clone())
        .ok_or_else(|| "private conversation snapshot unavailable".into())
}

/// An explicit read. An invocation is checked against retained original
/// host custody before any private body returns; without one this is the
/// local [`snapshot`].
pub fn read(public_key: &str) -> Pending<Conversation> {
    let original = match original() {
        Ok(Some(original)) => original,
        Ok(None) => return Pending::ready(snapshot(public_key)),
        Err(error) => return Pending::ready(Err(error)),
    };
    conversation(
        Request::Read {
            public_key: public_key.to_owned(),
        },
        public_key,
        Some(original),
    )
}

/// Success means accepted owner persistence. An uncertain reply is an error,
/// never an optimistic acknowledgement.
pub fn send(public_key: &str, text: &str) -> Pending<Conversation> {
    if text.trim().is_empty() || text.len() > 16384 {
        return Pending::ready(Err("invalid private message text".into()));
    }
    let original = match original() {
        Ok(original) => original,
        Err(error) => return Pending::ready(Err(error)),
    };
    conversation(
        Request::Send {
            public_key: public_key.to_owned(),
            text: text.to_owned(),
            invocation: original.unwrap_or_else(uuid::Uuid::new_v4),
        },
        public_key,
        original,
    )
}

fn conversation(
    request: Request,
    peer: &str,
    original: Option<uuid::Uuid>,
) -> Pending<Conversation> {
    let generation = match generation() {
        Ok(generation) => generation,
        Err(error) => return Pending::ready(Err(error)),
    };
    let (pending, fill) = Pending::new();
    let peer = peer.to_owned();
    exchange(request, original, move |answer| {
        fill.fill(bound_conversation(answer, &peer, generation));
    });
    pending
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(pending: Pending<Conversation>) -> Result<Conversation, String> {
        pending.take().expect("answered at once")
    }

    #[test]
    fn missing_host_never_fabricates_a_ready_conversation() {
        let _guard = tilo_test_env::lock();
        invalidate();
        assert!(now(open(&"01".repeat(32))).is_err());
        assert!(snapshot(&"01".repeat(32)).is_err());
        assert!(now(read(&"01".repeat(32))).is_err());
        assert!(now(send(&"01".repeat(32), "hello")).is_err());
        assert!(now(send(&"01".repeat(32), "  ")).is_err());
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
            assert!(snapshot(&"01".repeat(32)).is_err());
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
            assert!(snapshot("peer").is_ok());
        }
        assert!(stopped.has_changed().is_ok());
        close("another-peer");
        assert!(snapshot("peer").is_ok());
        assert!(retain("peer"));
        close("peer");
        assert!(stopped.has_changed().is_ok());
        assert!(snapshot("peer").is_ok());
        start_subscription("peer", None).unwrap();
        close("peer");
        assert!(stopped.has_changed().is_ok());
        assert!(snapshot("peer").is_ok());
        close("peer");
        assert!(stopped.has_changed().is_err());
        assert!(snapshot("peer").is_err());
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
        assert!(snapshot("peer").is_err());
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
        assert!(snapshot("old-peer").is_err());
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
        assert!(snapshot("first").is_ok());
        assert!(snapshot("second").is_ok());
        close("first");
        assert!(receivers[0].has_changed().is_err());
        assert!(receivers[1].has_changed().is_ok());
        assert!(snapshot("first").is_err());
        assert!(snapshot("second").is_ok());
        close("second");
        assert!(receivers[1].has_changed().is_err());
        assert!(snapshot("second").is_err());
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

    #[test]
    fn requests_are_the_native_door_vocabulary() {
        let json = |request: &Request| serde_json::to_value(request).unwrap();
        assert_eq!(
            json(&Request::Contacts),
            serde_json::json!({"operation":"contacts"})
        );
        assert_eq!(
            json(&Request::Open {
                public_key: "k".into()
            }),
            serde_json::json!({"operation":"open","public_key":"k"})
        );
        let invocation = uuid::Uuid::new_v4();
        assert_eq!(
            json(&Request::Send {
                public_key: "k".into(),
                text: "hi".into(),
                invocation
            }),
            serde_json::json!({"operation":"send","public_key":"k","text":"hi","invocation":invocation})
        );
    }
}
