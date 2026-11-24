//! The record reads of the windows a desktop hosts in process.
//!
//! A window reads what another app keeps through its session's record API
//! ([`tilo_actions::remote::ResourceReads`]), as it does on a native desktop, where the workspace
//! broker answers each read on a thread of its own. A desktop that hosts its windows in process (a
//! page) has one thread, and the records its windows may read are the ones it holds itself. So its
//! session only takes the reads its windows ask, and the desktop answers them on its own thread
//! from what it holds ([`RecordAnswers`]): at once, or, for a read that names the revision its
//! reader has and waits, when that record next differs. Nothing waits on a thread, and a read that
//! waits is not read again until then: each pass only compares the record's mark
//! ([`HeldRecords::mark`]) with the one it was held at.
//!
//! What a caller may read is the desktop's to decide for every read, again each time it answers
//! one ([`HeldRecords::read`]), as the broker decides it: a reader refused is told so, whatever it
//! was shown before.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::remote::{
    ActionCallError, Caller, Completer, MAX_REMOTE_WAIT, Pending, RemoteActionMessage,
    ResourceAddress, ResourceAsk, ResourceCopy, ResourceReadRequest, ResourceReads, WatchScope,
    pending, readings_of,
};
use tilo_lock::MutexRecover as _;
use ui::time::{Duration, Instant};

/// The longest the desktop holds a read for the next write, whatever `wait_ms` asked. Longer than
/// a host a connection reaches holds one ([`MAX_REMOTE_WAIT`]): here no connection is held open,
/// and a reader that follows a record nobody writes is answered, and asks again, this rarely.
pub const HOLD_MAX: Duration = Duration::from_secs(60);

/// What a reader hears that takes a record from the session itself rather than asking it to
/// answer a read: the session holds no record, the desktop does.
pub const ASKED_NOT_TAKEN: &str =
    "this desktop answers a record read where it holds its records, through the record API";

/// How many times one pass takes what the windows asked: a reader asks again as it is answered,
/// and a pass ends even if one never stops.
pub const ROUNDS: usize = 8;

/// What the desktop holds of its workspace's records, as it lets a caller read them.
pub trait HeldRecords {
    /// The record `address` as `caller` may read it now: its committed content, or
    /// [`ResourceCopy::Absent`] for one the workspace holds none of.
    ///
    /// # Errors
    ///
    /// As [`ResourceReads::read`]: [`ActionCallError::Refused`] for a record `caller` may not
    /// read, [`ActionCallError::Unavailable`] for one that is still arriving, or while what the
    /// caller may read is not known yet.
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError>;

    /// A few bytes that differ whenever the committed content of the record `address` does (its
    /// heads), `None` for a record the desktop does not hold. Asked on every pass for every read
    /// held for a write, where [`Self::read`] is asked only once the mark moved: it must cost
    /// next to nothing.
    fn mark(&self, address: &ResourceAddress) -> Option<Vec<u8>>;

    /// A count that moves whenever what decides who reads what may have changed: a role, a grant.
    /// A read held for a write is decided again when it does, whatever its record did.
    fn authority(&self) -> u64;

    /// A count that moves whenever any record the desktop holds may have changed: what a wait for
    /// a change of the workspace's records ends on. Asked only while such a wait is held.
    fn moved(&self) -> u64;
}

/// One read a window asked, waiting for the desktop.
pub struct RecordAsk {
    caller: Caller,
    read: ResourceReadRequest,
    answer: Completer<Result<String, ActionCallError>>,
}

/// The session's end: the reads its windows ask, kept for the desktop.
pub struct Reads {
    asked: std::sync::Mutex<Vec<RecordAsk>>,
    /// The waits for a change of the desktop's records.
    changes: std::sync::Mutex<Vec<Completer<()>>>,
    /// Whether the desktop answers its windows' reads ([`RecordAnswers::of`]).
    served: AtomicBool,
    /// Raised while the desktop answers: a reader asking again as it is answered is taken in the
    /// same pass, and needs no frame for it.
    answering: AtomicBool,
    /// Asks the desktop's host for the frame that takes an ask.
    woken: Arc<AtomicBool>,
}

impl Reads {
    pub fn new(woken: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            asked: std::sync::Mutex::default(),
            changes: std::sync::Mutex::default(),
            served: AtomicBool::new(false),
            answering: AtomicBool::new(false),
            woken,
        })
    }

    /// The record API, once the desktop answers it.
    pub fn served(self: &Arc<Self>) -> Option<Arc<dyn ResourceReads>> {
        self.served
            .load(Ordering::Acquire)
            .then(|| Arc::clone(self) as Arc<dyn ResourceReads>)
    }

    /// The next time the desktop's records may have changed, for a member of its workspace.
    pub fn changed(&self, caller: &Caller, scope: WatchScope) -> Option<Pending<()>> {
        if !self.served.load(Ordering::Acquire)
            || scope != WatchScope::Records
            || caller.workspace().is_none()
        {
            return None;
        }
        let (moved, waiting) = pending();
        self.changes.lock_recover().push(moved);
        self.wake();
        Some(waiting)
    }

    fn wake(&self) {
        if !self.answering.load(Ordering::Acquire) {
            self.woken.store(true, Ordering::Release);
        }
    }
}

impl ResourceReads for Reads {
    /// Only the desktop's thread holds the records: a read is asked of it
    /// ([`Self::answer_read`]), never taken here. Refused, not unavailable: a reader that takes
    /// records itself ([`tilo_actions::remote::read_resources`]) reads an unavailable record as
    /// one still arriving, and would answer with a record that never comes.
    fn read(&self, _: &Caller, _: &ResourceAddress) -> Result<ResourceCopy, ActionCallError> {
        Err(ActionCallError::Refused {
            reason: ASKED_NOT_TAKEN.into(),
        })
    }

    fn watch(
        &self,
        _: &Caller,
        _: &ResourceAddress,
        _: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        Err(ActionCallError::Refused {
            reason: ASKED_NOT_TAKEN.into(),
        })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        let (answer, answered) = pending();
        self.asked.lock_recover().push(RecordAsk {
            caller: caller.clone(),
            read,
            answer,
        });
        self.wake();
        answered.map(move |result| RemoteActionMessage::ResourceReading {
            correlation,
            result,
        })
    }
}

/// A read held until its record differs from what its reader has, or its wait ends.
pub struct Waiting {
    ask: RecordAsk,
    until: Instant,
    /// What it was held at ([`proof`]): it is read again once this differs.
    proof: (u64, String),
}

/// What a read of `ask` is held at: who may read what, and the mark of every record it reads (a
/// joined one's parts, as [`readings_of`] walks them). It differs whenever the readings may.
pub fn proof(held: &dyn HeldRecords, ask: &RecordAsk) -> (u64, String) {
    let marks = readings_of(&asks_of(ask), |address| {
        Ok(held
            .mark(address)
            .map_or(ResourceCopy::Absent, ResourceCopy::Document))
    })
    .map(|marks| marks.revision)
    .unwrap_or_default();
    (held.authority(), marks)
}

/// The record `ask` reads, as [`readings_of`] takes it.
pub fn asks_of(ask: &RecordAsk) -> [ResourceAsk; 1] {
    [ResourceAsk {
        address: ResourceAddress {
            workspace: ask.read.workspace.clone(),
            app: ask.read.app.clone(),
            record: ask.read.record.clone(),
        },
        joined: ask.read.joined,
    }]
}

/// The desktop's answers to its windows' record reads ([`super::Openings::answer_records`]).
pub struct RecordAnswers {
    reads: Arc<Reads>,
    waiting: Vec<Waiting>,
    /// The waits for a change, each with when it is answered whatever happened.
    changes: Vec<(Completer<()>, Instant)>,
    /// What [`HeldRecords::moved`] said on the last pass a wait for a change was held at: one
    /// asked since is answered by a change made before the pass that takes it.
    moved: Option<u64>,
}

impl RecordAnswers {
    pub fn of(reads: &Arc<Reads>) -> Self {
        reads.served.store(true, Ordering::Release);
        Self {
            reads: Arc::clone(reads),
            waiting: Vec::new(),
            changes: Vec::new(),
            moved: None,
        }
    }

    /// Whether this pass has anything to do: a read or a wait asked since the last, or one held.
    /// A desktop none of whose windows reads another app's records answers `false` at the cost of
    /// two uncontended locks, and skips [`Self::answer`].
    #[must_use]
    pub fn has_work(&self) -> bool {
        self.waits()
            || !self.reads.asked.lock_recover().is_empty()
            || !self.reads.changes.lock_recover().is_empty()
    }

    /// Whether a window's read or wait is held for the desktop's records to move.
    #[must_use]
    pub fn waits(&self) -> bool {
        !self.waiting.is_empty() || !self.changes.is_empty()
    }

    /// Answer what the windows asked since the last pass from `held`, and each read held for a
    /// write whose record's mark moved, whose reader may be decided differently now, or whose
    /// wait ended by `now`. A held read is read again only then: a pass in which nothing it reads
    /// moved reads no record.
    ///
    /// Returns when the next pass is due at the latest though nothing moves, the first wait to
    /// end; `None` when nothing waits. The desktop's host calls again by then, on every frame it
    /// draws, and on the frame its session's wake asks for.
    pub fn answer(&mut self, held: &dyn HeldRecords, now: Instant) -> Option<Instant> {
        self.reads.answering.store(true, Ordering::Release);
        self.answer_changes(held, now);
        for waiting in std::mem::take(&mut self.waiting) {
            if waiting.ask.answer.is_abandoned() {
                continue;
            }
            if waiting.until <= now || proof(held, &waiting.ask) != waiting.proof {
                self.read(waiting.ask, Some(waiting.until), held, now);
            } else {
                self.waiting.push(waiting);
            }
        }
        for _ in 0..ROUNDS {
            let asked = std::mem::take(&mut *self.reads.asked.lock_recover());
            if asked.is_empty() {
                break;
            }
            for ask in asked {
                self.read(ask, None, held, now);
            }
        }
        self.reads.answering.store(false, Ordering::Release);
        // What a pass that ran out of rounds left, and a wait asked meanwhile, is the next one's.
        if !self.reads.asked.lock_recover().is_empty()
            || !self.reads.changes.lock_recover().is_empty()
        {
            self.reads.woken.store(true, Ordering::Release);
        }
        self.waiting
            .iter()
            .map(|waiting| waiting.until)
            .chain(self.changes.iter().map(|(_, until)| *until))
            .min()
    }

    /// End each wait for a change of the desktop's records once they moved since it was asked,
    /// or within the bound a host keeps for one.
    fn answer_changes(&mut self, held: &dyn HeldRecords, now: Instant) {
        let within = now.checked_add(MAX_REMOTE_WAIT).unwrap_or(now);
        let asked = std::mem::take(&mut *self.reads.changes.lock_recover());
        self.changes
            .extend(asked.into_iter().map(|change| (change, within)));
        if self.changes.is_empty() {
            self.moved = None;
            return;
        }
        let at = held.moved();
        let moved = self.moved.is_some_and(|before| before != at);
        for (change, until) in std::mem::take(&mut self.changes) {
            if change.is_abandoned() {
                continue;
            }
            if moved || until <= now {
                change.complete(());
            } else {
                self.changes.push((change, until));
            }
        }
        // Kept while a waiter is answered too: it asks again as it reads what changed, and that
        // wait is the next pass's to take.
        self.moved = Some(at);
    }

    /// Read what `ask` names and answer it, or hold it until `until` (from its own wait, when
    /// `None`) while its record is what its reader has.
    fn read(
        &mut self,
        ask: RecordAsk,
        until: Option<Instant>,
        held: &dyn HeldRecords,
        now: Instant,
    ) {
        let until = until.unwrap_or_else(|| {
            let wait = ask.read.wait_ms.map_or(Duration::ZERO, |wait| {
                Duration::from_millis(u64::from(wait)).min(HOLD_MAX)
            });
            now.checked_add(wait).unwrap_or(now)
        });
        // Marked before it is read: a write between the two is seen on the next pass.
        let proof = proof(held, &ask);
        match readings_of(&asks_of(&ask), |address| held.read(&ask.caller, address)) {
            Ok(readings)
                if now < until && ask.read.after.as_deref() == Some(readings.revision.as_str()) =>
            {
                self.waiting.push(Waiting { ask, until, proof });
            }
            Ok(readings) => ask.answer.complete(
                readings
                    .readable()
                    .map(|readings| readings.to_string())
                    .map_err(|reason| ActionCallError::Refused { reason }),
            ),
            Err(error) => ask.answer.complete(Err(error)),
        }
    }
}
