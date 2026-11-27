//! A record's reading kept between asks: the record opened once for the person who reads it, on a
//! thread of its own (what a host holds stays on the thread that made it), and read again only
//! where the workspace's writes moved it since the last ask. A page asked after a write costs
//! about a page, not an open of the record and a page.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::store::MovedRecord;
use tilo_action_policy::WorkspacePolicy;
use tilo_workspace_service::workspaces::ResourceWrite;
use tokio::sync::broadcast::{Receiver, error::TryRecvError};

use super::{Opened, Record};

/// How long a reading nobody asked of stays open: a caller listing a record a page at a time asks
/// again within it, and one that went away gives the memory back.
const IDLE: Duration = Duration::from_secs(120);

/// The record of an app in a workspace, as one person reads it.
type Key = (String, String, String);

/// What a reading's thread runs: an ask, which answers its caller itself.
type Ask = Box<dyn FnOnce(&mut Option<Reading>) + Send>;

/// The readings kept on this device, each on its own thread.
#[derive(Default)]
pub(crate) struct Kept {
    threads: Arc<Mutex<HashMap<Key, mpsc::Sender<Ask>>>>,
}

/// A record opened for its reader, with the writes the workspace told of since it was read.
struct Reading {
    opened: Opened,
    /// The policy it was opened under: a reading under another is opened again.
    policy: Option<Arc<WorkspacePolicy>>,
    /// The workspace's writes, subscribed to before the record was opened, so none it missed.
    writes: Receiver<ResourceWrite>,
}

impl Kept {
    /// `read` of `app`'s `record` under `policy`, on the reading kept for it: opened by `open`
    /// when none is kept or the policy moved, else read again where the workspace's writes moved
    /// it since. `None` when the record does not open.
    ///
    /// # Panics
    /// `read` or `open` panicked: the panic goes on in the caller, and the reading is opened
    /// again by the next ask.
    pub(crate) fn read<R: Send + 'static>(
        &self,
        record: Record,
        policy: Option<Arc<WorkspacePolicy>>,
        open: impl FnOnce(Record, Option<Arc<WorkspacePolicy>>) -> Result<Opened, String>
        + Send
        + 'static,
        read: impl FnOnce(&Opened) -> R + Send + 'static,
    ) -> Result<R, String> {
        let key = (
            record.workspace().to_owned(),
            record.app().to_owned(),
            record.reader().to_owned(),
        );
        let (answer, answered) = mpsc::sync_channel(1);
        let ask: Ask = Box::new(move |kept: &mut Option<Reading>| {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let reading = current(kept, record, policy, open)?;
                Ok(read(&reading.opened))
            }));
            if ran.is_err() {
                *kept = None;
            }
            let _ = answer.send(ran);
        });
        let mut threads = self
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Sent under the lock: a thread that goes idle leaves the map under it, and runs what was
        // sent before it left.
        let sender = match threads.entry(key) {
            Entry::Occupied(kept) => kept.into_mut(),
            Entry::Vacant(none) => {
                let key = none.key().clone();
                none.insert(spawn(&self.threads, key)?)
            }
        };
        let sent = sender.send(ask).is_ok();
        drop(threads);
        if !sent {
            return Err("a record's reading stopped before it was asked".to_owned());
        }
        match answered.recv() {
            Ok(Ok(read)) => read,
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => Err("a record's reading stopped before it answered".to_owned()),
        }
    }
}

/// The reading `kept` holds, read again where the workspace's writes moved it, or `record` opened
/// by `open` when none is kept, it fell too far behind the writes, or the policy moved.
fn current(
    kept: &mut Option<Reading>,
    record: Record,
    policy: Option<Arc<WorkspacePolicy>>,
    open: impl FnOnce(Record, Option<Arc<WorkspacePolicy>>) -> Result<Opened, String>,
) -> Result<&Reading, String> {
    if kept
        .as_ref()
        .is_some_and(|reading| reading.policy.as_deref() != policy.as_deref())
    {
        *kept = None;
    }
    if let Some(reading) = kept.as_mut() {
        caught_up(reading, record.workspace(), record.app());
    } else {
        // Subscribed first: a write told while the record opens is read again by the next ask.
        let writes = record.writes();
        let opened = open(record, policy.clone())?;
        *kept = Some(Reading {
            opened,
            policy,
            writes,
        });
    }
    kept.as_ref()
        .ok_or_else(|| "the record's reading was not kept".to_owned())
}

/// Tell `reading`'s store what the workspace's writes to `app`'s record of `workspace` moved
/// since it last read them: those documents are read again, and the rest stands. A reading that
/// fell behind more writes than were kept for it forgets all it read.
fn caught_up(reading: &mut Reading, workspace: &str, app: &str) {
    let mut moved = BTreeSet::new();
    let mut whole = false;
    loop {
        match reading.writes.try_recv() {
            Ok(write) if write.workspace == workspace && write.app == app => {
                let records = std::iter::once(&write.record).chain(write.together.iter());
                for record in records {
                    // The app's own document is no store's: whatever it held may have moved.
                    if record.is_empty() {
                        whole = true;
                    } else {
                        moved.insert(record.clone());
                    }
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => whole = true,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    let store = reading.opened.host().store();
    if whole {
        store.moved();
    } else {
        let moved: Vec<MovedRecord> = moved.into_iter().map(MovedRecord::whole).collect();
        store.moved_records(&moved);
    }
}

/// The thread a reading of `key` is kept on, until nobody asks of it for [`IDLE`].
///
/// # Errors
/// The device gives no thread.
fn spawn(
    threads: &Arc<Mutex<HashMap<Key, mpsc::Sender<Ask>>>>,
    key: Key,
) -> Result<mpsc::Sender<Ask>, String> {
    let (sender, asks) = mpsc::channel::<Ask>();
    let threads = Arc::clone(threads);
    std::thread::Builder::new()
        .name("tl-record-reading".to_owned())
        .spawn(move || {
            let mut reading = None;
            loop {
                match asks.recv_timeout(IDLE) {
                    Ok(ask) => ask(&mut reading),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        threads
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&key);
                        // What was sent before it left the map is still answered.
                        while let Ok(ask) = asks.try_recv() {
                            ask(&mut reading);
                        }
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .map_err(|error| format!("a record's reading has no thread: {error}"))?;
    Ok(sender)
}
