//! Jobs: the long work the whole system can see.
//!
//! Anything that can take longer than a breath is a **job**: it has a name,
//! a subject, a progress a person can read, a way out, and an outcome — and
//! it belongs to the system, not to the window that started it.
//!
//! An app starts one with [`start`] and keeps the [`Running`] it gets back
//! while its worker runs: the worker says what it is doing ([`Running::phase`])
//! and how far it has got ([`Running::progress`], [`Running::seen`]), checks
//! whether the person stopped it ([`Running::is_stopped`]), and ends it
//! exactly once ([`Running::done`], [`Running::failed`], [`Running::stopped`]).
//! The record lives in a registry of the process, so closing the page, the
//! panel or the window that started the work does not end it: whoever shows
//! the surface next reads the same rows ([`rows`]) and the same progress.
//!
//! What a person is shown is one row per job ([`surface`]): its title, what
//! it is doing now, a bar that is determinate when the total is known and
//! honest when it is not, a **Stop** for a job that can be stopped, and a
//! **Show me** that goes to the thing being worked on. A finished job keeps
//! its row until it is read; a failed one states why, and stands in
//! [`crate::attention`] as well, because a failure is something a person has
//! to decide about — see [`crate::attention::publish`].
//!
//! ```ignore
//! let job = ui::jobs::start(
//!     ui::jobs::Spec::new("Installing LibreOffice")
//!         .subject(ui::jobs::Subject::new(store, "LibreOffice").opens("store/app/libreoffice"))
//!         .unit(ui::jobs::Unit::Bytes)
//!         .stoppable(|| worker.cancel()),
//! );
//! let worker_job = job.clone();
//! ui::background::Background::current().spawn("install", move || {
//!     for chunk in download() {
//!         if worker_job.is_stopped() { return worker_job.stopped(); }
//!         worker_job.progress(chunk.seen, 480_000_000);
//!     }
//!     worker_job.done("Installed LibreOffice");
//! });
//! ```
//!
//! The rule this module exists to keep: an app never improvises a progress
//! story of its own. A spinner with no words, a percentage with no way out,
//! a "working…" that dies with its window — all of those are this module
//! seen from the wrong side.

mod record;
mod surface;
#[cfg(test)]
mod tests;

/// Who a job belongs to, as the surfaces name it: the same owner an
/// attention row carries.
pub use crate::attention::Owner;
pub use record::{
    Job, JobId, Outcome, Progress, Running, Spec, Subject, Summary, Unit, clear, dismiss, finished,
    is_active, revision, rows, running, show, start, stop, summary,
};
pub use surface::{Band, Press, Presses, apply, band, pane};

use crate::wake::WakeHandle;

/// The chord that opens the Jobs surface, wherever the person is.
///
/// A host resolves it ([`take_opening`]) and shows the surface the same way
/// it shows [`crate::attention`]'s.
pub const CHORD: &str = "ctrl+shift+j";

thread_local! {
    static OPENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Ask for the Jobs surface: what [`CHORD`] and the bar's activity pill do.
pub fn open() {
    OPENING.with(|opening| opening.set(true));
}

/// Whether the surface was asked for since the last call.
#[must_use]
pub fn take_opening() -> bool {
    OPENING.with(|opening| opening.replace(false))
}

/// Call `wake` whenever a job starts, reports or ends, until the returned
/// guard drops: a surface that shows jobs redraws then, rather than on a
/// timer.
#[must_use = "the surface stops hearing about jobs when the guard drops"]
pub fn watch(wake: WakeHandle) -> Watch {
    record::watch(wake)
}

/// One test at a time: the registry is the process's, as the real one is,
/// so two tests running at once would see each other's jobs.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    use crate::lock::MutexRecover as _;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock_recover()
}

/// A registered watcher; dropping it stops the wakes.
pub struct Watch {
    id: u64,
}

impl Watch {
    pub(crate) const fn new(id: u64) -> Self {
        Self { id }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        record::unwatch(self.id);
    }
}
