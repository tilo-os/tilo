//! The job registry: what it holds, what it says, and what survives.

use super::record::{Outcome, Progress, Spec, Subject, Unit};
use super::{dismiss, finished, rows, running, start, stop, summary};
use crate::attention::{self, Action, Notice, Owner, Urgency};

fn owner() -> Owner {
    Owner::new("so.tilo.test", "Test").with_icon(crate::icons::Icon::Download)
}

fn began(title: &str) -> super::Running {
    start(Spec::new(title).subject(Subject::new(owner(), title)))
}

/// A job survives the surface that started it: it is in the registry before
/// its worker reports anything, and the row is there for whoever looks next.
#[test]
fn a_job_is_visible_before_its_worker_says_anything() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    let job = began("Installing LibreOffice");
    let rows = rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title(), "Installing LibreOffice");
    assert!(rows[0].is_running());
    assert_eq!(rows[0].live_line(), "Starting");
    assert_eq!(summary().running, 1);
    assert!(super::is_active());
    job.done("Installed LibreOffice");
    assert!(!super::is_active());
}

/// What a job is doing, how far, and how it ended read as the person's
/// sentences, never as a bare number.
#[test]
fn a_job_says_what_it_is_doing_and_how_far() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    let job = start(
        Spec::new("Installing LibreOffice")
            .subject(Subject::new(owner(), "LibreOffice").opens("store/app/libreoffice"))
            .unit(Unit::Bytes)
            .phase("Downloading"),
    );
    job.progress(312_000_000, 480_000_000);
    let row = &rows()[0];
    assert_eq!(row.live_line(), "Downloading 312 MB of 480 MB");
    assert_eq!(row.percent().as_deref(), Some("65%"));
    assert_eq!(
        row.subject().target.as_deref(),
        Some("store/app/libreoffice")
    );

    job.phase("Verifying");
    job.seen(1_204);
    let row = &rows()[0];
    assert_eq!(row.live_line(), "Verifying 1.2 kB");
    assert_eq!(row.percent(), None, "no total, no percentage");

    job.done("Installed LibreOffice");
    let row = &finished()[0];
    assert_eq!(row.outcome_line().as_deref(), Some("Installed LibreOffice"));
    assert_eq!(row.end_line(), "Installed LibreOffice");
    assert!(!row.can_stop());
}

/// Stop is asked of the worker, and a job that can stop says so; a job
/// without a way out shows no Stop, because a Stop that would not be
/// honoured is a lie.
#[test]
fn only_a_job_that_can_be_stopped_is_offered_a_stop() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    let stoppable = start(
        Spec::new("Scanning Home")
            .subject(Subject::new(owner(), "Home"))
            .stoppable(|| {}),
    );
    assert!(running()[0].can_stop());
    let plain = began("Copying");
    assert!(!running()[1].can_stop());

    assert!(stop(stoppable.id()));
    assert!(stoppable.is_stopped(), "the worker hears it");
    let being_stopped = running()[0].clone();
    assert_eq!(being_stopped.id(), stoppable.id());
    assert!(being_stopped.is_stopping());
    assert!(!being_stopped.can_stop(), "Stop is not offered twice");
    stoppable.stopped();
    assert_eq!(finished()[0].outcome(), Some(&Outcome::Stopped));
    assert_eq!(finished()[0].outcome_line().as_deref(), Some("Stopped"));
    assert!(!stop(plain.id()), "a job with no way out is not asked");
}

/// A failure states why, stays in the list, and waits in Needs you: a failed
/// job is something a person has to decide about.
#[test]
fn a_failure_says_why_and_waits_for_a_person() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    attention::clear();
    let job = began("Installing LibreOffice");
    job.failed("480 MB needed, 210 MB free");
    let row = &rows()[0];
    assert_eq!(
        row.outcome_line().as_deref(),
        Some("Couldn’t finish: 480 MB needed, 210 MB free"),
        "{:?}",
        row.outcome_line()
    );
    let waiting = attention::items();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0].title(), "Installing LibreOffice");
    assert_eq!(
        waiting[0].because_line(),
        Some("480 MB needed, 210 MB free")
    );
    assert_eq!(waiting[0].action().verb, attention::SHOW_JOB);
    assert_eq!(waiting[0].urgency(), Urgency::Waiting);

    // Reading the row takes the failure away with it.
    dismiss(job.id());
    assert!(rows().is_empty());
    assert!(attention::items().is_empty());
}

/// The same failure twice is one row that says the latest why, not a stack.
#[test]
fn the_same_failure_twice_is_one_waiting_row() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    attention::clear();
    for reason in ["no disk", "no disk, again"] {
        let job = began("Installing LibreOffice");
        job.failed(reason);
    }
    let waiting = attention::items();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].because_line(), Some("no disk, again"));
    assert_eq!(attention::unseen(), 1);
    for row in &finished() {
        dismiss(row.id());
    }
}

/// What a person chooses to do about a failure is one verb, and acting on it
/// reports it once.
#[test]
fn acting_on_a_failure_reports_the_verb_once() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    attention::clear();
    let job = began("Printing report");
    job.failed("The printer stopped answering");
    let item = &attention::items()[0];
    let acted = attention::take(item.id()).expect("an item to act on");
    assert_eq!(acted.action.verb, attention::SHOW_JOB);
    assert_eq!(acted.owner.name, "Test");
    assert_eq!(attention::take(item.id()), None, "acted on once");
    assert!(attention::items().is_empty());
}

/// A recorded outcome is not attention: it is written to the ledger, does
/// not count on the bar, and does not expire.
#[test]
fn an_outcome_that_needs_nobody_is_recorded_not_shown() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    attention::clear();
    attention::clear_ledger();
    let recorded =
        attention::publish(Notice::did(owner(), "Moved 3 files to Trash").about("Trash"));
    assert!(matches!(recorded, attention::Published::Recorded(_)));
    assert!(attention::items().is_empty());
    assert_eq!(attention::unseen(), 0);
    let entries = attention::entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].what(), "Moved 3 files to Trash");
    assert_eq!(entries[0].owner().name, "Test");
    assert!(entries[0].matches("trash"), "the record is searchable");
    assert!(attention::search_ledger("files").len() == 1);
    assert!(attention::search_ledger("nothing at all").is_empty());
}

/// Reading a finished row is not reading a running one: dismiss leaves what
/// still runs alone.
#[test]
fn a_running_row_cannot_be_read_away() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    let job = began("Scanning Home");
    dismiss(job.id());
    assert_eq!(running().len(), 1);
    job.stopped();
    dismiss(job.id());
    assert!(rows().is_empty());
}

/// Two jobs run at once: the store stays usable while an install runs, and
/// every row is its own.
#[test]
fn jobs_are_concurrent_by_construction() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    let install = began("Installing LibreOffice");
    let _scan = began("Scanning Home");
    assert_eq!(running().len(), 2);
    install.progress(1, 4);
    assert_eq!(
        running()[0].progress(),
        &Progress::Measured { done: 1, total: 4 }
    );
    assert_eq!(
        running()[1].progress(),
        &Progress::Starting,
        "one job's progress is not another's"
    );
}

/// An owner's name is always there: a row says who is asking, not only its
/// icon.
#[test]
fn a_row_always_names_the_app_that_asks() {
    let _serial = crate::jobs::test_lock();
    crate::jobs::clear();
    attention::clear();
    let _ = attention::publish(Notice::needs(
        owner(),
        "Couldn’t reach the printer",
        Action::new("print.retry", "Print again"),
    ));
    let item = &attention::items()[0];
    assert_eq!(item.owner().name, "Test");
    assert_eq!(item.action().label, "Print again");
    assert_eq!(item.state(), attention::State::Unseen);
}
