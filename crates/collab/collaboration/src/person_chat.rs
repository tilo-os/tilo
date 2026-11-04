//! What only the device that hosts a workspace can say about a person's old
//! direct conversations: the pages an import reads from the store an older
//! build kept there, and its emptying.
//!
//! A conversation is an ordinary record of the workspace, which the app reads
//! and writes itself ([`crate::record`], the app's own store): nothing of it
//! passes this door. What does is the store's content, so a person's history
//! comes over once into the conversation it belongs to, and the store is
//! emptied in one publication. The operations answer with a [`Pending`] the
//! importing window fills as it syncs, never by waiting on the UI thread.
//!
//! This door goes in the release after the first one that ships the import:
//! by then every device has had a release in which its stored conversations
//! moved into their records.

use std::sync::{Arc, Mutex};
use tilo_lock::MutexRecover;

/// A person a conversation can be opened with, as the sidebar lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    pub public_key: String,
    /// The name the workspace keeps for the person, empty when it has none
    /// (it never answers one of their devices' labels).
    pub display_name: String,
    /// Whether a conversation with them holds messages.
    pub conversation: bool,
}

/// One message of a conversation, as a window shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub author: String,
    pub text: String,
    /// When its writer's device sent it, in milliseconds since the epoch.
    pub at: i64,
}

/// What two people have said to each other, as a window shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub peer: String,
    pub messages: Vec<Message>,
}

/// One page of a conversation still held in the old store, read to import it
/// into the conversation's record.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImportPage {
    /// The person the conversation is with, as asked for.
    pub peer: String,
    pub messages: Vec<StoredMessage>,
    pub has_more: bool,
    /// Where the next page starts.
    #[serde(default)]
    pub next_sequence: u64,
}

/// A message as the old store kept it: its id, its author (under the key a
/// conversation keeps that person), its text, and when the owner took it in.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub author: String,
    pub text: String,
    #[serde(default)]
    pub at: i64,
}

/// What the door asks of the host: the closed vocabulary the native host's
/// person door takes, in the same JSON.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum Request {
    /// One page of the conversation with `public_key` as the old store keeps
    /// it, read for the import into the conversation's record.
    Import {
        public_key: String,
        after_sequence: u64,
    },
    /// Empty the device's old store, once its conversations are in their
    /// records.
    Clear,
}

/// What the host answered a [`Request`].
pub enum Answer {
    ImportPage(ImportPage),
    /// The old store was emptied.
    Cleared,
}

/// Called once with what the host answered.
pub type AnswerDone = Box<dyn FnOnce(Result<Answer, String>) + Send>;

/// The key a conversation keeps a person under, from a person as the
/// workspace names them (the lowercase hex of their root key): the old
/// store's hash of it, which the conversations' authors and read marks use.
#[must_use]
pub fn person_key(person_id: &str) -> String {
    match hex::decode(person_id) {
        Ok(bytes) if bytes.len() == 32 => blake3::hash(&bytes).to_hex().to_string(),
        _ => person_id.to_owned(),
    }
}

/// The id of the record the direct conversation between the root keys `a` and
/// `b` lives in, in `workspace`: the same id from either person's device, so
/// both reach the one record they share. `None` where the workspace is not a
/// UUID, a key is not lowercase hex, or the two are the same person.
#[must_use]
pub fn direct_pair_id(workspace: &str, a: &str, b: &str) -> Option<String> {
    collab_types::direct_pair_id(workspace, a, b)
}

thread_local! { static ORIGINAL: std::cell::Cell<Option<uuid::Uuid>> = const { std::cell::Cell::new(None) }; }

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

/// A door's answer, which may come later: a page asks the workspace over the
/// network, and the UI thread never waits for it. Poll it with [`Self::take`]
/// from sync (its arrival wakes the app), or await it.
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
        self.slot.lock_recover().value.take()
    }
}

impl<T> Fill<T> {
    fn fill(mut self, value: Result<T, String>) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let waker = {
            let mut slot = slot.lock_recover();
            slot.value = Some(value);
            slot.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        crate::WakeHandle::new().wake();
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
        let mut slot = self.slot.lock_recover();
        if let Some(value) = slot.value.take() {
            return std::task::Poll::Ready(value);
        }
        slot.waker = Some(cx.waker().clone());
        std::task::Poll::Pending
    }
}

fn original() -> Result<Option<uuid::Uuid>, String> {
    let original = ORIGINAL.get();
    if crate::invocation_scope::current_invoker().is_some() && original.is_none() {
        return Err("private conversation requires original person invocation custody".into());
    }
    Ok(original)
}

/// Ask the host `request` and hand `done` the answer: the broker answers in
/// place, waiting, off nothing but this thread.
fn exchange(
    request: Request,
    original: Option<uuid::Uuid>,
    done: impl FnOnce(Result<Answer, String>),
) {
    #[cfg(feature = "broker-transport")]
    {
        let reply = Reply::of(&request);
        let answer = serde_json::to_vec(&request)
            .map_err(|_| "invalid private conversation request".to_owned())
            .and_then(|bytes| {
                crate::person_chat_command(
                    bytes,
                    original.map_or_else(Vec::new, |id| id.as_bytes().to_vec()),
                )
            })
            .and_then(|bytes| decode(reply, &bytes));
        done(answer);
    }
    // A page holds no old store: the conversations it reads and writes are
    // the workspace's records, and the store's import happens on the device
    // that hosts them.
    #[cfg(not(feature = "broker-transport"))]
    {
        let _ = (request, original);
        done(Err(
            "the old store is on the device that hosts the workspace".into(),
        ));
    }
}

/// What a [`Request`] is answered with.
#[cfg(feature = "broker-transport")]
#[derive(Clone, Copy)]
enum Reply {
    ImportPage,
    Cleared,
}

#[cfg(feature = "broker-transport")]
impl Reply {
    fn of(request: &Request) -> Self {
        match request {
            Request::Import { .. } => Self::ImportPage,
            Request::Clear => Self::Cleared,
        }
    }
}

#[cfg(feature = "broker-transport")]
fn decode(reply: Reply, bytes: &[u8]) -> Result<Answer, String> {
    if bytes.len() > 1024 * 1024 {
        return Err("private conversation reply exceeds bound".into());
    }
    match reply {
        Reply::ImportPage => serde_json::from_slice(bytes)
            .map(Answer::ImportPage)
            .map_err(|_| "invalid private conversation import reply".into()),
        Reply::Cleared => Ok(Answer::Cleared),
    }
}

/// One page of the conversation with `peer` as the old store keeps it, read
/// to import it into the conversation's record: the app has it imported once,
/// and never writes that store again.
pub fn import_archive(public_key: &str, after_sequence: u64) -> Pending<ImportPage> {
    let original = match original() {
        Ok(original) => original,
        Err(error) => return Pending::ready(Err(error)),
    };
    let peer = public_key.to_owned();
    let (pending, fill) = Pending::new();
    exchange(
        Request::Import {
            public_key: peer.clone(),
            after_sequence,
        },
        original,
        move |answer| {
            fill.fill(match answer {
                Ok(Answer::ImportPage(page)) if page.peer == peer => Ok(page),
                Ok(_) => Err("invalid private conversation import reply".into()),
                Err(error) => Err(error),
            });
        },
    );
    pending
}

/// Empty the device's old conversation store, once every conversation it held
/// is in its record: what the workspace keeps of a person's old history is
/// the records.
pub fn clear_archive() -> Pending<()> {
    let original = match original() {
        Ok(original) => original,
        Err(error) => return Pending::ready(Err(error)),
    };
    let (pending, fill) = Pending::new();
    exchange(Request::Clear, original, move |answer| {
        fill.fill(match answer {
            Ok(Answer::Cleared) => Ok(()),
            Ok(_) => Err("invalid private conversation clear reply".into()),
            Err(error) => Err(error),
        });
    });
    pending
}
