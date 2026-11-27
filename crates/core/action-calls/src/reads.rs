//! Reading records through the API: the walk over what an ask names, the wait for a write that
//! follows the read, and the answer a provider hands back.
//!
//! [`ResourceReads`] in [`tilo_actions`] is what a provider implements to read the records of its
//! workspace; this is how a host answers a read asked over
//! [`RemoteActionMessage::ResourceRead`] from it. The reading walk, the JSON every reader is
//! answered and the document behind it are reached only from here, so a provider that serves an
//! app's windows and reads no record through the API carries none of it.

use tilo_actions::document_sync::{DocumentSync, document_part, document_sync};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, HeldCopies, Pending, ReadAnswer, RemoteActionMessage,
    ResourceAddress, ResourceAsk, ResourceCopy, ResourceReadRequest, ResourceReadings,
    ResourceReads, ResourceWake, unanswered, wait_deadline,
};

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
use tilo_actions::remote::{Completer, ReadingAnswer, pending};

/// `copies` read for `asks`, as every reader is answered
/// ([`ResourceReadings::readable`]): what a reader planning with the copies it has reads.
///
/// # Errors
///
/// As [`readings_of`] and [`ResourceReadings::readable`].
pub fn copies_readable(
    copies: &HeldCopies,
    asks: &[ResourceAsk],
) -> Result<serde_json::Value, String> {
    readings_of(asks, |address| {
        Ok(copies
            .0
            .get(&address.record)
            .map_or(ResourceCopy::Absent, |bytes| {
                ResourceCopy::Document(bytes.clone())
            }))
    })
    .map_err(|error| error.to_string())?
    .readable()
}

/// One pass over `asks`: each record `read` answers (a joined one as its parts) and the revision of
/// them all, which changes with any write to any of them. A record `read` answers
/// [`ActionCallError::Unavailable`] for is still arriving and reads as `None`. A provider that waits on
/// writes watches a record inside `read`, before it reads it, so no write between the two is missed.
///
/// This walks records already in hand, which answer at once: the desktop's, which it reads on its
/// own turn. A reader that awaits its records walks them with [`readings_from`].
///
/// # Errors
///
/// What `read` refuses a record with, other than [`ActionCallError::Unavailable`].
pub fn readings_of(
    asks: &[ResourceAsk],
    mut read: impl FnMut(&ResourceAddress) -> Result<ResourceCopy, ActionCallError>,
) -> Result<ResourceReadings, ActionCallError> {
    let mut walk = Walk::default();
    for ask in asks {
        let mut asking = Asking::of(ask);
        while let Some(address) = asking.next() {
            let copy = held(read(&address))?;
            asking.took(&mut walk.revision, address, copy);
        }
        walk.records.extend(asking.answered());
    }
    Ok(walk.readings())
}

/// [`readings_of`], over records `read` answers once it has them.
///
/// # Errors
///
/// What `read` refuses a record with, other than [`ActionCallError::Unavailable`].
pub async fn readings_from<Read, Answer>(
    asks: &[ResourceAsk],
    mut read: Read,
) -> Result<ResourceReadings, ActionCallError>
where
    Read: FnMut(ResourceAddress) -> Answer,
    Answer: std::future::Future<Output = Result<ResourceCopy, ActionCallError>>,
{
    let mut walk = Walk::default();
    for ask in asks {
        let mut asking = Asking::of(ask);
        while let Some(address) = asking.next() {
            let copy = held(read(address.clone()).await)?;
            asking.took(&mut walk.revision, address, copy);
        }
        walk.records.extend(asking.answered());
    }
    Ok(walk.readings())
}

/// A record's copy as a reading holds it: `None` while it is still arriving, whose write wakes
/// the reader.
fn held(
    read: Result<ResourceCopy, ActionCallError>,
) -> Result<Option<ResourceCopy>, ActionCallError> {
    match read {
        Ok(copy) => Ok(Some(copy)),
        Err(ActionCallError::Unavailable { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The records a walk over several asks read so far, and the revision of them all.
#[derive(Default)]
struct Walk {
    revision: blake3::Hasher,
    records: Vec<(String, Option<ResourceCopy>)>,
}

impl Walk {
    fn readings(self) -> ResourceReadings {
        ResourceReadings {
            revision: self.revision.finalize().to_hex().to_string(),
            records: self.records,
        }
    }
}

/// One ask's records as they are read: the record alone, or the parts of its chain one after
/// another up to the first that holds no document.
struct Asking<'a> {
    ask: &'a ResourceAsk,
    /// The chain's base, for an ask that reads one.
    base: Option<&'a str>,
    /// Whether the ask is a chain's head read alone: it answers the newest part.
    newest_only: bool,
    part: u64,
    done: bool,
    held: Vec<(String, Option<ResourceCopy>)>,
}

impl<'a> Asking<'a> {
    fn of(ask: &'a ResourceAsk) -> Self {
        let newest = match document_sync(&ask.address.record) {
            DocumentSync::ChainHead { base } if !ask.joined => Some(base),
            _ => None,
        };
        let base = newest.or_else(|| ask.joined.then_some(ask.address.record.as_str()));
        Self {
            ask,
            base,
            newest_only: newest.is_some(),
            part: 0,
            done: false,
            held: Vec::new(),
        }
    }

    /// The record to read next, `None` once the ask is read.
    fn next(&self) -> Option<ResourceAddress> {
        if self.done {
            return None;
        }
        let record = match self.base {
            Some(base) => document_part(base, self.part),
            None => self.ask.address.record.clone(),
        };
        Some(ResourceAddress {
            workspace: self.ask.address.workspace.clone(),
            app: self.ask.address.app.clone(),
            record,
        })
    }

    /// `address`, the record [`Self::next`] named, read as `copy`.
    fn took(
        &mut self,
        revision: &mut blake3::Hasher,
        address: ResourceAddress,
        copy: Option<ResourceCopy>,
    ) {
        let chained = self.base.is_some();
        // A reading's revision is the one its provider names; one naming none, and a record its
        // app reads for itself, are hashed by their JSON, which is all a reader can tell about
        // them.
        let hashed;
        let bytes: &[u8] = match &copy {
            Some(ResourceCopy::Document(bytes)) => bytes,
            Some(ResourceCopy::Reading {
                revision: Some(revision),
                ..
            }) => revision.as_bytes(),
            Some(
                ResourceCopy::Read(value)
                | ResourceCopy::Reading {
                    value,
                    revision: None,
                },
            ) => {
                hashed = value.to_string();
                hashed.as_bytes()
            }
            Some(ResourceCopy::Absent) => b"",
            None => b"unavailable",
        };
        for bytes in [address.record.as_bytes(), bytes] {
            revision.update(&(bytes.len() as u64).to_le_bytes());
            revision.update(bytes);
        }
        let last = !chained || !matches!(copy, Some(ResourceCopy::Document(_)));
        // The part after a chain's last is watched, not answered: its first write wakes.
        if !(chained && self.part > 0 && matches!(copy, Some(ResourceCopy::Absent))) {
            self.held.push((address.record, copy));
        }
        if last {
            self.done = true;
        } else {
            self.part = self.part.saturating_add(1);
        }
    }

    /// What the ask answers: a head the newest part alone, every part read staying in the
    /// revision, so a write to any of them, or a new part, wakes its reader.
    fn answered(mut self) -> Vec<(String, Option<ResourceCopy>)> {
        if self.newest_only
            && let Some(newest) = self
                .held
                .iter()
                .rposition(|(_, copy)| matches!(copy, Some(ResourceCopy::Document(_))))
                .or_else(|| self.held.len().checked_sub(1))
        {
            self.held = vec![self.held.swap_remove(newest)];
        }
        self.held
    }
}

/// Read `asks` for `caller` once they differ from the revision `after` or `deadline` passed:
/// each record is watched before it is read, so the next write to any of them wakes the wait.
///
/// # Errors
///
/// As [`ResourceReads::read`], but a record still arriving answers `None` rather than failing.
pub async fn read_resources<Reads: ResourceReads + ?Sized>(
    reads: &Reads,
    caller: &Caller,
    asks: &[ResourceAsk],
    after: Option<&str>,
    deadline: tilo_clock::time::Instant,
) -> Result<ResourceReadings, ActionCallError> {
    loop {
        let (wake, woken) = ResourceWake::new();
        let wake = &wake;
        let readings = readings_from(asks, |address| async move {
            reads.watch(caller, &address, wake).await?;
            reads.read(caller, &address).await
        })
        .await?;
        let now = tilo_clock::now();
        if after != Some(readings.revision.as_str()) || now >= deadline {
            return Ok(readings);
        }
        let mut woken = std::pin::pin!(woken);
        #[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
        let mut passed = std::pin::pin!(tilo_rt::sleep(deadline.saturating_duration_since(now)));
        // A build without a runtime (a model module, or one that answers no reads) has no timer: it
        // only waits for the write.
        #[cfg(not(all(feature = "answer-reads", not(tilo_no_js))))]
        let mut passed = std::pin::pin!(std::future::pending::<()>());
        std::future::poll_fn(|cx| {
            if woken.as_mut().poll(cx).is_ready() || passed.as_mut().poll(cx).is_ready() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
    }
}

/// Answer `read`, asked as `correlation`, for `caller` from `reads`, the provider's record API:
/// awaited where it is asked, through the wait for a write past `read.after` and the record's
/// capture.
///
/// Every provider that reads records answers [`ResourceReads::answer_read`] with this, and the API
/// posts a read through that method rather than here: the reading, the JSON every reader is
/// answered and the record format behind it are reached only from a reader's own code, so a provider
/// that serves an app's windows and reads no record does not carry them.
pub async fn answer_read<Reads: ResourceReads + ?Sized>(
    reads: std::sync::Arc<Reads>,
    caller: Caller,
    correlation: u64,
    read: ResourceReadRequest,
) -> RemoteActionMessage {
    RemoteActionMessage::ResourceReading {
        correlation,
        result: serve_readings(reads.as_ref(), &caller, read).await,
    }
}

/// `reading`, answered where it is completed: polled first in the call that asked, so a reader
/// that has the record answers it there, and again on the thread each wait of it ends on, so a
/// reader that answers on its own turn — the desktop reading its windows' records — is delivered
/// inside that turn, and a follow asks again in it. No thread is parked on the read: whoever ends
/// a wait runs the rest of it.
///
/// A read is polled within a runtime's context ([`tilo_rt::Handle`]), so one that waits on a timer
/// has one wherever it is resumed.
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
pub(crate) fn answered_where_it_ends(reading: ReadingAnswer) -> Pending<RemoteActionMessage> {
    let (answer, answered) = pending();
    let driven = std::sync::Arc::new(Driven {
        slot: std::sync::Mutex::new(Slot::Held(Held { reading, answer })),
        again: std::sync::atomic::AtomicBool::new(false),
    });
    driven.drive();
    answered
}

/// What a [`Driven`] holds between polls, if anything.
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
enum Slot {
    /// The reading and the answer it completes.
    Held(Held),
    /// This thread polls it now: a wake meanwhile asks for one more poll.
    Polling,
    /// It answered, or the client went.
    Done,
}

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
struct Held {
    reading: ReadingAnswer,
    answer: Completer<RemoteActionMessage>,
}

/// A read answered on the thread that ends its wait: [`Driven::drive`] polls it, and its waker —
/// given to whatever the read awaits — drives it again on the thread that completes that wait.
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
struct Driven {
    slot: std::sync::Mutex<Slot>,
    /// A wake that came while the reading was polled on another thread.
    again: std::sync::atomic::AtomicBool,
}

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
impl Driven {
    /// Poll the held reading as far as it goes. Called in the call that asked and on every wake,
    /// on whichever thread that is.
    fn drive(self: &std::sync::Arc<Self>) {
        use std::sync::atomic::Ordering;
        use tilo_lock::MutexRecover as _;

        loop {
            let mut slot = self.slot.lock_recover();
            let taken = std::mem::replace(&mut *slot, Slot::Polling);
            drop(slot);
            let held = match taken {
                Slot::Held(held) => held,
                Slot::Polling => {
                    // Another thread polls it: that poll ends with one more.
                    self.again.store(true, Ordering::Release);
                    return;
                }
                Slot::Done => {
                    *self.slot.lock_recover() = Slot::Done;
                    return;
                }
            };
            self.again.store(false, Ordering::Release);
            let Held {
                mut reading,
                answer,
            } = held;
            let waker = std::task::Waker::from(std::sync::Arc::clone(self));
            let mut context = std::task::Context::from_waker(&waker);
            // A read may wait on a timer, and resumed on a thread that runs no runtime it would
            // fail to find one: poll it in the context of this thread's runtime, or the app's.
            #[cfg(not(target_family = "wasm"))]
            let polled = {
                let runtime = tilo_rt::Handle::try_current()
                    .unwrap_or_else(|_| tilo_rt::app().handle().clone());
                let _entered = runtime.enter();
                reading.as_mut().poll(&mut context)
            };
            #[cfg(target_family = "wasm")]
            let polled = reading.as_mut().poll(&mut context);
            if let std::task::Poll::Ready(message) = polled {
                *self.slot.lock_recover() = Slot::Done;
                answer.complete(message);
                return;
            }
            let mut slot = self.slot.lock_recover();
            *slot = Slot::Held(Held { reading, answer });
            let again = self.again.swap(false, Ordering::Acquire);
            drop(slot);
            if !again {
                return;
            }
        }
    }
}

#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
impl std::task::Wake for Driven {
    fn wake(self: std::sync::Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.drive();
    }
}

async fn serve_readings<Reads: ResourceReads + ?Sized>(
    reads: &Reads,
    caller: &Caller,
    read: ResourceReadRequest,
) -> Result<String, ActionCallError> {
    let write = read.answer == ReadAnswer::WriteDecision;
    if write || read.answer == ReadAnswer::Decision {
        let address = ResourceAddress {
            workspace: read.workspace,
            app: read.app,
            record: read.record,
        };
        let decided = if write {
            reads.may_write(caller, &address).await
        } else {
            reads.may_read(caller, &address).await
        };
        return decided.map(|()| "null".to_owned());
    }
    if read.answer == ReadAnswer::Listing {
        return listing(reads, caller, &read.workspace, &read.app).await;
    }
    let document = read.answer == ReadAnswer::Document;
    let deadline = wait_deadline(read.wait_ms.unwrap_or(0));
    let ask = [ResourceAsk {
        address: ResourceAddress {
            workspace: read.workspace,
            app: read.app,
            record: read.record,
        },
        joined: read.joined,
    }];
    let readings = read_resources(reads, caller, &ask, read.after.as_deref(), deadline).await?;
    if document {
        return Ok(readings.documents().to_string());
    }
    readings
        .readable()
        .map(|answer| answer.to_string())
        .map_err(|reason| ActionCallError::Refused { reason })
}

/// The resources of `workspace` `caller` may read of `app` (every app's, empty), as a listing
/// answers them: `{"records": [{"app", "record", "kind"}]}`, `kind` only for a record the
/// workspace's store keeps.
async fn listing<Reads: ResourceReads + ?Sized>(
    reads: &Reads,
    caller: &Caller,
    workspace: &str,
    app: &str,
) -> Result<String, ActionCallError> {
    let listed: Vec<serde_json::Value> = reads
        .list(caller, workspace, app)
        .await?
        .into_iter()
        .map(|listed| {
            let mut entry = serde_json::Map::new();
            entry.insert("app".into(), listed.address.app.into());
            entry.insert("record".into(), listed.address.record.into());
            if let Some(kind) = listed.kind {
                entry.insert("kind".into(), kind.into());
            }
            serde_json::Value::Object(entry)
        })
        .collect();
    Ok(serde_json::json!({ "records": listed }).to_string())
}

/// Answer a posted read for `provider`: through its own reader, or as a provider that reads no
/// records when it keeps none ([`ActionProvider::resource_reads`]).
#[cfg(all(feature = "answer-reads", not(tilo_no_js)))]
pub fn read_answered<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    read: ResourceReadRequest,
) -> Pending<RemoteActionMessage> {
    provider.resource_reads().map_or_else(
        || unanswered(correlation),
        |reads| answered_where_it_ends(reads.answer_read(caller, correlation, read)),
    )
}

/// A build with no runtime to answer on (a model module, or one that reads no records) answers
/// every posted read as a provider that reads no records does.
#[cfg(not(all(feature = "answer-reads", not(tilo_no_js))))]
pub fn read_answered<H: ActionProvider>(
    _provider: &H,
    _caller: &Caller,
    correlation: u64,
    _read: ResourceReadRequest,
) -> Pending<RemoteActionMessage> {
    unanswered(correlation)
}

#[cfg(test)]
#[path = "reads_tests.rs"]
mod tests;
