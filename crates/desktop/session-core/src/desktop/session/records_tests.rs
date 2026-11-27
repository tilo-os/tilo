//! A window a desktop hosts in process reads what another app keeps, sees it change, and is refused
//! what the desktop does not let it read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, CHANGED_UNAVAILABLE, Caller, MAX_REMOTE_WAIT,
    RESOURCE_READ_REFUSED, RESOURCE_READS_UNAVAILABLE, ResourceAddress, ResourceCopy,
    ResourceReadRequest, WatchScope,
};
use tilo_ui_base::Arrival;
use ui::RemoteActions;
use ui::automerge::transaction::Transactable as _;
use ui::time::Duration;

use super::{HOLD_MAX, HeldRecords, Openings, RecordAnswers};

pub const WORKSPACE: &str = "workspace";
pub const TASKS: &str = "so.tilo.tasks";
pub const MAIL: &str = "so.tilo.mail";

/// A record a desktop holds: what it says, and the write that left it so.
pub struct Kept {
    written: u64,
    record: Vec<u8>,
}

/// What a desktop holds: each record by its app and route, the apps its person may not read, and
/// how many records it read.
#[derive(Default)]
pub struct Held {
    records: RefCell<HashMap<(String, String), Kept>>,
    denied: RefCell<HashSet<String>>,
    /// Moves with every change of who may read what.
    decided: Cell<u64>,
    written: Cell<u64>,
    read: Cell<usize>,
}

impl Held {
    /// Write `app`'s record of the workspace: its title is `title`.
    fn write(&self, app: &str, title: &str) {
        let mut record = ui::automerge::AutoCommit::new();
        record.put(ui::automerge::ROOT, "title", title).unwrap();
        self.written.set(self.written.get().saturating_add(1));
        self.records.borrow_mut().insert(
            (app.to_owned(), String::new()),
            Kept {
                written: self.written.get(),
                record: record.save(),
            },
        );
    }

    /// Whether the desktop's person may read `app`'s records from now on.
    fn let_read(&self, app: &str, may: bool) {
        if may {
            self.denied.borrow_mut().remove(app);
        } else {
            self.denied.borrow_mut().insert(app.to_owned());
        }
        self.decided.set(self.decided.get().saturating_add(1));
    }
}

impl HeldRecords for Held {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        self.read.set(self.read.get().saturating_add(1));
        if caller.workspace() != Some(WORKSPACE) || self.denied.borrow().contains(&address.app) {
            return Err(refused());
        }
        Ok(self
            .records
            .borrow()
            .get(&(address.app.clone(), address.record.clone()))
            .map_or(ResourceCopy::Absent, |kept| {
                ResourceCopy::Document(kept.record.clone())
            }))
    }

    fn mark(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        self.records
            .borrow()
            .get(&(address.app.clone(), address.record.clone()))
            .map(|kept| kept.written.to_le_bytes().to_vec())
    }

    fn authority(&self) -> u64 {
        self.decided.get()
    }

    fn moved(&self) -> u64 {
        self.written.get()
    }
}

/// A desktop that answers its windows' reads, one of its windows, and what wakes its host.
pub fn desktop() -> (RemoteActions, Openings, RecordAnswers, Arc<AtomicBool>) {
    let woken = Arc::new(AtomicBool::new(false));
    let (session, openings) = super::serve(WORKSPACE, Arc::clone(&woken));
    let answers = openings.answer_records();
    let window =
        RemoteActions::with_provider(session, Caller::local_person("ada").in_workspace(WORKSPACE));
    (window, openings, answers, woken)
}

pub fn record_of(app: &str) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: WORKSPACE.into(),
        app: app.into(),
        record: String::new(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: tilo_actions::remote::ReadAnswer::Reading,
    }
}

/// The title the readings in `call` show.
pub fn title(call: &ui::ResourceReadCall) -> Option<String> {
    call.with(|arrival| {
        arrival
            .ready()
            .and_then(|readings| readings.pointer("/records/0/value/title"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
}

/// The revision the readings in `call` name.
pub fn revision(call: &ui::ResourceReadCall) -> Option<String> {
    call.with(|arrival| {
        arrival
            .ready()
            .and_then(|readings| readings.get("revision"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
}

pub fn refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: RESOURCE_READ_REFUSED.into(),
    }
}

/// A window follows what another app keeps: the desktop answers on its own pass, the window sees
/// each write on the pass after it, and a pass in which its record did not move reads nothing.
#[test]
pub fn a_window_reads_what_another_app_keeps_and_sees_it_change() {
    let (window, _openings, mut answers, woken) = desktop();
    let held = Held::default();
    held.write(TASKS, "Ship it");
    let now = ui::time::now();

    assert!(!answers.has_work(), "nothing was asked");
    let followed = window.follow_resource(None, record_of(TASKS));
    assert!(
        followed.call().get().is_pending(),
        "asked of the desktop: no thread reads it meanwhile"
    );
    assert!(
        woken.swap(false, Ordering::SeqCst),
        "the ask wakes the desktop's host"
    );
    assert!(answers.has_work());
    let due = answers.answer(&held, now);
    assert_eq!(title(followed.call()).as_deref(), Some("Ship it"));
    assert_eq!(
        due,
        Some(now + HOLD_MAX),
        "the reader asked again as it was answered, and that read is held"
    );
    assert!(
        !woken.load(Ordering::SeqCst),
        "taken in the same pass: the held read asks for no frame"
    );

    let (read, shown) = (held.read.get(), followed.call().revision());
    answers.answer(&held, now + Duration::from_secs(1));
    assert_eq!(held.read.get(), read, "nothing moved: nothing is read");
    assert_eq!(
        followed.call().revision(),
        shown,
        "and nothing is shown again"
    );

    held.write(TASKS, "Shipped");
    answers.answer(&held, now + Duration::from_secs(2));
    assert_eq!(title(followed.call()).as_deref(), Some("Shipped"));
    assert!(answers.waits(), "it is held for the next write");

    // Another app's write is not this record's: it is not read again, and its reader is shown
    // nothing.
    held.write(MAIL, "Inbox");
    let (read, shown) = (held.read.get(), followed.call().revision());
    answers.answer(&held, now + Duration::from_secs(3));
    assert_eq!(held.read.get(), read);
    assert_eq!(followed.call().revision(), shown);

    drop(followed);
    held.write(TASKS, "Archived");
    answers.answer(&held, now + Duration::from_secs(4));
    assert!(!answers.waits(), "a reader that left asks no more");
    assert!(!answers.has_work());
}

/// What the desktop does not let its person read is refused, read once or followed; a reader the
/// desktop refuses later is told so and shown nothing more.
#[test]
pub fn a_read_the_desktop_refuses_is_refused() {
    let (window, _openings, mut answers, _woken) = desktop();
    let held = Held::default();
    held.write(TASKS, "Ship it");
    held.write(MAIL, "Inbox");
    held.let_read(MAIL, false);
    let now = ui::time::now();

    let once = window.read_resource(None, record_of(MAIL));
    let followed = window.follow_resource(None, record_of(MAIL));
    let allowed = window.follow_resource(None, record_of(TASKS));
    answers.answer(&held, now);
    assert_eq!(once.get(), Arrival::Failed(refused()));
    assert_eq!(followed.call().get(), Arrival::Failed(refused()));
    assert_eq!(title(allowed.call()).as_deref(), Some("Ship it"));

    // The workspace takes the read back while the window shows the record, which nobody wrote.
    held.let_read(TASKS, false);
    answers.answer(&held, now);
    assert_eq!(
        allowed.call().get(),
        Arrival::Failed(refused()),
        "what it showed is no longer its to see"
    );
    assert!(!answers.waits(), "a refused reader asks no more");

    // Someone from outside the workspace reads nothing of it.
    let (session, openings) = super::serve(WORKSPACE, Arc::default());
    let mut answers = openings.answer_records();
    let outsider = RemoteActions::with_provider(session, Caller::local_person("eve"));
    let read = outsider.read_resource(None, record_of(TASKS));
    held.let_read(TASKS, true);
    answers.answer(&held, now);
    assert_eq!(read.get(), Arrival::Failed(refused()));
}

/// A desktop that holds no records answers no read, as a host without the record API does.
#[test]
pub fn a_desktop_that_answers_no_reads_says_so_at_once() {
    let (session, _openings) = super::serve(WORKSPACE, Arc::default());
    let caller = Caller::local_person("ada").in_workspace(WORKSPACE);
    assert!(session.changed(&caller, WatchScope::Records).is_none());
    let window = RemoteActions::with_provider(session, caller);
    assert_eq!(
        window.read_resource(None, record_of(TASKS)).get(),
        Arrival::Failed(ActionCallError::Unavailable {
            reason: RESOURCE_READS_UNAVAILABLE.into(),
        })
    );
}

/// A reader that takes records from the session itself, as a shortcut's read step does on a
/// native host, is refused in so many words, and at once: it never reads a record as still
/// arriving that only the desktop's own pass can answer, and never waits out a write for one.
#[test]
pub fn a_reader_that_takes_records_itself_is_refused_not_left_waiting() {
    use std::future::Future as _;
    use tilo_actions::remote::{ResourceAsk, read_resources};
    let (session, openings) = super::serve(WORKSPACE, Arc::default());
    let _answers = openings.answer_records();
    let reads = session.resource_reads().expect("the desktop answers reads");
    let caller = Caller::local_person("ada").in_workspace(WORKSPACE);
    let asks = [ResourceAsk {
        address: ResourceAddress {
            workspace: WORKSPACE.into(),
            app: TASKS.into(),
            record: String::new(),
        },
        joined: false,
    }];
    let mut read = std::pin::pin!(read_resources(
        reads.as_ref(),
        &caller,
        &asks,
        None,
        tilo_clock::now(),
    ));
    let taken = match read
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(taken) => taken,
        std::task::Poll::Pending => panic!("the read waits for the desktop's own pass"),
    };
    assert_eq!(
        taken.err(),
        Some(ActionCallError::Refused {
            reason: super::ASKED_NOT_TAKEN.into(),
        })
    );
}

/// A read that names what its reader has and waits is answered when its wait ends, with the record
/// as it is; the desktop holds none longer than [`HOLD_MAX`].
#[test]
pub fn a_read_that_waits_is_answered_when_its_wait_ends() {
    let (window, _openings, mut answers, _woken) = desktop();
    let held = Held::default();
    held.write(TASKS, "Ship it");
    let now = ui::time::now();

    let first = window.read_resource(None, record_of(TASKS));
    assert_eq!(answers.answer(&held, now), None, "nothing waits");
    let at = revision(&first).expect("readings name their revision");

    let waiting = window.read_resource(
        None,
        ResourceReadRequest {
            after: Some(at.clone()),
            wait_ms: Some(5_000),
            answer: tilo_actions::remote::ReadAnswer::Reading,
            ..record_of(TASKS)
        },
    );
    let ends = now + Duration::from_secs(5);
    assert_eq!(answers.answer(&held, now), Some(ends));
    assert_eq!(
        answers.answer(&held, now + Duration::from_secs(4)),
        Some(ends)
    );
    assert!(waiting.get().is_pending());
    assert_eq!(answers.answer(&held, ends), None);
    assert_eq!(
        revision(&waiting),
        Some(at.clone()),
        "its wait ended: the record as it is"
    );

    let long = window.read_resource(
        None,
        ResourceReadRequest {
            after: Some(at),
            wait_ms: Some(u32::MAX),
            answer: tilo_actions::remote::ReadAnswer::Reading,
            ..record_of(TASKS)
        },
    );
    assert_eq!(answers.answer(&held, now), Some(now + HOLD_MAX));
    assert!(long.get().is_pending());
}

/// A wait for a change of the desktop's records ends when they move, and within the bound a host
/// keeps for one when they do not; who is in the workspace is not this session's to tell.
#[test]
pub fn a_wait_for_a_change_ends_when_the_records_move_or_within_its_bound() {
    let (window, _openings, mut answers, woken) = desktop();
    let held = Held::default();
    let now = ui::time::now();

    let moved = window.changed(WatchScope::Records);
    assert!(woken.swap(false, Ordering::SeqCst));
    assert_eq!(answers.answer(&held, now), Some(now + MAX_REMOTE_WAIT));
    assert!(moved.get().is_pending());
    held.write(TASKS, "Ship it");
    answers.answer(&held, now);
    assert_eq!(moved.get(), Arrival::Ready(()));

    // A waiter asks again as it reads what changed: a write before the pass that takes that wait
    // still ends it.
    let again = window.changed(WatchScope::Records);
    held.write(TASKS, "Shipped");
    answers.answer(&held, now);
    assert_eq!(again.get(), Arrival::Ready(()));

    let quiet = window.changed(WatchScope::Records);
    answers.answer(&held, now);
    assert!(quiet.get().is_pending());
    answers.answer(&held, now + MAX_REMOTE_WAIT);
    assert_eq!(
        quiet.get(),
        Arrival::Ready(()),
        "bounded: a waiter looks again"
    );
    assert_eq!(held.read.get(), 0, "a wait for a change reads no record");

    assert_eq!(
        window.changed(WatchScope::Here).get(),
        Arrival::Failed(ActionCallError::Unavailable {
            reason: CHANGED_UNAVAILABLE.into(),
        })
    );
}
