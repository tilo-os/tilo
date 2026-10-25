//! One test, many cases: [`crate::scenario!`] and the `#[scenario]`
//! attribute that declares them, the runtime each phase reports to, and the
//! panic hook that names the case a failure came from.
//!
//! A crate of a few thousand tests was a few thousand test functions, each
//! with its own `#[test]` scaffolding, and a run spent much of its wall time
//! starting and finishing them. Folding a group of related tests into one
//! scenario keeps every case, every fixture and every assertion, and runs them
//! in one pass: a phase that needs a fresh fixture builds it inside its own
//! scope, exactly as the separate test did.
//!
//! ```ignore
//! scenario! {
//!     fn a_saved_project_opens_as_the_same_timeline;
//!
//!     phase saving_writes_a_file_that_opens_back;
//!     /// The whole point of a project file, end to end through the app.
//!     {
//!         let (app, doc) = app_and_doc();
//!         assert!(app.save(&doc, path).is_ok(), "the file is on disk");
//!     }
//!
//!     phase an_open_is_one_undoable_action;
//!     {
//!         // A separate fixture, as the test it came from built.
//!         let (app, doc) = app_and_doc();
//!         assert!(opened_in_one_edit(&app), "an open is one edit");
//!     }
//! }
//! ```
//!
//! A phase keeps its own scope, so two phases may both bind `app` — as two
//! tests did. When a case fails, the panic message names its phase and the
//! line inside the phase that raised it, and the rest of the scenario's cases
//! still run, so one pass reports all of them.

use std::any::Any;
use std::borrow::Cow;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One case: a labelled phase with its own scope.
///
/// The case runs while the scenario does, so it may borrow the test's own
/// values, as the body of a test may.
pub struct Phase<'a> {
    /// What the case is called in a report.
    pub name: &'static str,
    /// `file:line`, where the case starts.
    pub location: &'static str,
    /// The case itself.
    pub run: Box<dyn FnOnce() + 'a>,
}

impl fmt::Debug for Phase<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Phase")
            .field("name", &self.name)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl<'a> Phase<'a> {
    /// The case `name`, starting at `location`.
    #[must_use]
    pub fn new(name: &'static str, location: &'static str, run: impl FnOnce() + 'a) -> Self {
        Self {
            name,
            location,
            run: Box::new(run),
        }
    }
}

/// What a scenario run did: its cases in order, each with the panic it raised,
/// if any.
#[derive(Debug)]
pub struct Report {
    cases: Vec<Case>,
}

#[derive(Debug)]
struct Case {
    name: &'static str,
    location: &'static str,
    failure: Option<Box<str>>,
}

impl Report {
    /// The cases, as `(name, location, failure)`, in the order they ran.
    pub fn cases(&self) -> impl Iterator<Item = (&'static str, &'static str, Option<&str>)> {
        self.cases
            .iter()
            .map(|case| (case.name, case.location, case.failure.as_deref()))
    }

    /// How many cases ran.
    #[must_use]
    pub fn total(&self) -> usize {
        self.cases.len()
    }

    /// Fail the calling test unless every case passed, naming each case that
    /// did not, where it starts and the line inside it that raised.
    ///
    /// # Panics
    ///
    /// Panics when a case failed. A scenario that ran no case also fails: a
    /// test that observes nothing has proved nothing.
    pub fn assert_green(&self) {
        assert!(
            !self.cases.is_empty(),
            "the scenario ran no case, so it proved nothing",
        );
        let failed: Vec<&Case> = self.cases.iter().filter(|case| case.failure.is_some()).collect();
        if failed.is_empty() {
            return;
        }
        let mut message = format!(
            "{} of {} cases failed in this scenario:",
            failed.len(),
            self.cases.len(),
        );
        for case in &failed {
            let failure = case.failure.as_deref().unwrap_or_default();
            let _ = fmt::Write::write_fmt(
                &mut message,
                format_args!(
                    "\n\n  phase `{name}` ({location})\n  {failure}",
                    name = case.name,
                    location = case.location,
                ),
            );
        }
        assert!(failed.is_empty(), "{message}");
    }
}

thread_local! {
    /// The case the current thread is inside, which the hook names.
    static CURRENT: std::cell::RefCell<Option<&'static str>> =
        const { std::cell::RefCell::new(None) };
}

/// A panic hook, as `std::panic` keeps one.
type Hook = Box<dyn Fn(&panic::PanicHookInfo<'_>) + Sync + Send>;

/// The hook a panic no case owns is reported through, while a scenario runs.
static PREVIOUS: Mutex<Option<Hook>> = Mutex::new(None);

/// How many scenarios are running in this process: the hook is installed for
/// the first and taken off by the last, so two tests running scenarios in
/// parallel share one hook rather than replacing each other's.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

fn hook(info: &panic::PanicHookInfo<'_>) {
    let name = CURRENT.with(std::cell::RefCell::take);
    if let Some(name) = name {
        let location = info
            .location()
            .map_or_else(|| "an unknown place".to_owned(), ToString::to_string);
        eprintln!(
            "\nphase `{name}` failed at {location}:\n{}",
            message_of(info.payload()),
        );
        return;
    }
    if let Some(previous) = PREVIOUS.lock().unwrap_or_else(|held| held.into_inner()).as_ref() {
        previous(info);
    }
}

/// The message a panic carried.
fn message_of(payload: &dyn Any) -> Cow<'_, str> {
    if let Some(message) = payload.downcast_ref::<&str>() {
        Cow::Borrowed(message)
    } else if let Some(message) = payload.downcast_ref::<String>() {
        Cow::Borrowed(message)
    } else {
        Cow::Borrowed("a panic without a message")
    }
}

/// Run every `phase` of a scenario, catching each one's panic, and report them
/// together.
///
/// The panic hook names the case a panic came from — including one printed
/// from a thread the case spawned and joined — and is put back before the
/// calling test reports.
pub fn run(phases: Vec<Phase<'_>>) -> Report {
    if RUNNING.fetch_add(1, Ordering::SeqCst) == 0 {
        let previous = panic::take_hook();
        *PREVIOUS.lock().unwrap_or_else(|held| held.into_inner()) = Some(previous);
        panic::set_hook(Box::new(hook));
    }
    CURRENT.with(|current| *current.borrow_mut() = None);
    let mut cases = Vec::with_capacity(phases.len());
    for Phase { name, location, run } in phases {
        CURRENT.with(|current| *current.borrow_mut() = Some(name));
        let outcome = panic::catch_unwind(AssertUnwindSafe(run));
        CURRENT.with(|current| *current.borrow_mut() = None);
        cases.push(Case {
            name,
            location,
            failure: outcome
                .err()
                .map(|payload| message_of(payload.as_ref()).into_owned().into_boxed_str()),
        });
    }
    if RUNNING.fetch_sub(1, Ordering::SeqCst) == 1 {
        let previous = PREVIOUS
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .take();
        let previous = previous.unwrap_or_else(panic::take_hook);
        panic::set_hook(previous);
    }
    Report { cases }
}
