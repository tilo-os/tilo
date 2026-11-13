//! Decode and apply untrusted Automerge bytes where a bad input can only cost a
//! worker process.
//!
//! Automerge is a CRDT library, not a parser hardened against hostile input:
//! malformed storage can make it panic, loop forever or allocate without bound, and
//! release builds abort on panic. So peer bytes pass [`precheck`] first, then the
//! same call runs in a worker process with its heap capped ([`Limits::memory_bytes`])
//! and a deadline per request ([`Limits::timeout`]), confined by Landlock and a
//! seccomp allow-list; a crash, a refused syscall or an overrun rejects the input.
//! Only then does the caller make the identical call on its own document, which is
//! deterministic and so does what the worker's did.
//!
//! Workers are long-lived and keep copies of the documents they have seen, keyed by
//! state, each state routed to the worker that last saw it; what a worker keeps is
//! bounded by bytes ([`Limits::cache_bytes`]) and an idle worker exits
//! ([`Limits::idle`]). [`Isolate::load`] takes the change hashes its worker already
//! computed instead of rebuilding them, and leaves the loaded document warm.
//!
//! A worker is the calling executable started again in worker mode, so a program
//! that admits untrusted bytes calls [`dispatch_early`] first thing in `main` (a
//! test crate adds [`test_worker!`]); without it admission fails with
//! [`Rejected::Unavailable`]. A library loaded into a foreign executable ships
//! `tilo-automerge-worker` beside itself and names it with [`serve_workers_from`].
//! A wasm build has no processes: there [`Isolate`] pre-checks and applies in the
//! page, to a copy that replaces the document on success.

mod engine;
#[cfg(target_os = "linux")]
mod pool;
pub mod precheck;
#[cfg(target_os = "linux")]
mod sandbox;
#[cfg(target_os = "linux")]
mod wire;
#[cfg(target_os = "linux")]
mod worker;

use std::time::Duration;

use automerge::AutoCommit;

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_chunks: usize,
    pub memory_bytes: u64,
    pub timeout: Duration,
    /// Time added to [`Limits::timeout`] per MiB of untrusted input. Decoding
    /// grows with the input, and a long history must not be refused only for
    /// being long.
    pub timeout_per_mib: Duration,
    pub workers: usize,
    pub max_documents: usize,
    pub cache_bytes: u64,
    pub idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 48 * 1024 * 1024,
            max_chunks: 65_536,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            timeout: Duration::from_secs(5),
            timeout_per_mib: Duration::from_secs(2),
            workers: 2,
            max_documents: 256,
            cache_bytes: 96 * 1024 * 1024,
            idle: Duration::from_secs(60),
        }
    }
}

/// Why an input was not applied. The caller's document is unchanged.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejected {
    #[error("automerge input exceeds its size limit")]
    TooLarge,
    #[error("malformed automerge input: {0}")]
    Malformed(&'static str),
    #[error("invalid automerge input: {0}")]
    Invalid(String),
    /// The worker died on the input: a panic, an allocation past its heap
    /// ceiling, or a signal.
    #[error("automerge input crashed its worker")]
    Crashed,
    /// The worker did not finish within [`Limits::timeout`].
    #[error("automerge input timed out")]
    TimedOut,
    #[error("automerge worker unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dependencies {
    RequireComplete,
    RetainOrphans,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Changed(bool),
    MissingDependencies,
}

/// Admission of untrusted Automerge bytes under one set of [`Limits`].
pub struct Isolate {
    #[cfg(target_os = "linux")]
    pool: pool::Pool,
    #[cfg(not(target_os = "linux"))]
    limits: Limits,
}

impl Isolate {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            pool: pool::Pool::new(limits),
            #[cfg(not(target_os = "linux"))]
            limits,
        }
    }

    pub fn shared() -> &'static Self {
        static SHARED: std::sync::OnceLock<Isolate> = std::sync::OnceLock::new();
        SHARED.get_or_init(|| Self::new(Limits::default()))
    }

    #[must_use]
    pub fn limits(&self) -> &Limits {
        #[cfg(target_os = "linux")]
        return self.pool.limits();
        #[cfg(not(target_os = "linux"))]
        return &self.limits;
    }

    pub fn apply(
        &self,
        doc: &mut AutoCommit,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        precheck::check(incoming, self.limits())?;
        #[cfg(target_os = "linux")]
        return self.pool.apply(doc, incoming, dependencies);
        #[cfg(not(target_os = "linux"))]
        return in_page::apply(doc, incoming, dependencies);
    }

    /// Load untrusted `bytes` as a complete document.
    ///
    /// # Errors
    ///
    /// A [`Rejected`] reason.
    pub fn load(&self, bytes: &[u8]) -> Result<AutoCommit, Rejected> {
        precheck::check(bytes, self.limits())?;
        #[cfg(target_os = "linux")]
        return self.pool.load(bytes);
        #[cfg(not(target_os = "linux"))]
        return engine::load(bytes);
    }
}

pub fn apply(
    doc: &mut AutoCommit,
    incoming: &[u8],
    dependencies: Dependencies,
) -> Result<Applied, Rejected> {
    Isolate::shared().apply(doc, incoming, dependencies)
}

pub fn load(bytes: &[u8]) -> Result<AutoCommit, Rejected> {
    Isolate::shared().load(bytes)
}

/// The hash of every change `incoming` carries, in the order it carries them,
/// when it is exactly a run of change chunks — what a participant publishes as
/// its own changes since the last take. `None` when it carries anything else,
/// so the caller cannot say which changes are its own and must decide on the
/// whole input instead. Nothing is decoded but each change's own header, and
/// no document is touched: this only names them.
#[must_use]
pub fn change_hashes(incoming: &[u8]) -> Option<Vec<automerge::ChangeHash>> {
    engine::change_hashes(incoming)
}

/// Outside Linux there are no workers yet, so none of their bounds apply:
/// input that passes [`precheck`] is decoded in this process with no heap
/// ceiling, deadline or sandbox, and a decoder panic or hang is the caller's.
/// Only the copy-then-replace keeps a refused input from changing `doc`.
#[cfg(not(target_os = "linux"))]
mod in_page {
    use super::{Applied, AutoCommit, Dependencies, Rejected, engine};

    /// Apply to a copy that replaces `doc` only on success.
    pub(super) fn apply(
        doc: &mut AutoCommit,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        let mut candidate = doc.clone();
        let applied = engine::apply(&mut candidate, incoming, dependencies)?;
        if applied != Applied::MissingDependencies {
            *doc = candidate;
        }
        Ok(applied)
    }
}

/// Serve as an Automerge worker when this process was started as one,
/// returning its exit code; `None` on an ordinary launch.
///
/// Call it first thing in `main`, before starting threads or a runtime:
///
/// ```no_run
/// fn main() -> std::process::ExitCode {
///     if let Some(code) = tilo_automerge_isolate::dispatch_early() {
///         return std::process::ExitCode::from(code);
///     }
///     // ...
///     std::process::ExitCode::SUCCESS
/// }
/// ```
#[must_use]
pub fn dispatch_early() -> Option<u8> {
    #[cfg(target_os = "linux")]
    if let Some(config) = worker::requested() {
        return Some(u8::try_from(worker::serve(config)).unwrap_or(1));
    }
    None
}

static WORKER_PROGRAM: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub fn serve_workers_from(program: std::path::PathBuf) -> bool {
    match WORKER_PROGRAM.set(program) {
        Ok(()) => true,
        Err(program) => WORKER_PROGRAM.get() == Some(&program),
    }
}

#[macro_export]
macro_rules! test_worker {
    () => {
        #[test]
        fn tilo_automerge_isolate_worker() {
            $crate::serve_test_worker();
        }
    };
}

#[doc(hidden)]
pub fn serve_test_worker() {
    if let Some(code) = dispatch_early() {
        #[expect(
            clippy::exit,
            reason = "the worker's test must not return to the test harness"
        )]
        std::process::exit(i32::from(code));
    }
}

#[cfg(test)]
test_worker!();
